//! Intent service core: create, read, edit and version listing.
//!
//! Mutations of one intent are serialized through [`IntentService::reserve`].
//! The choice is a per-intent `tokio::sync::Mutex`: a second mutation waits
//! its turn instead of failing. A mutation that finds the stored phase to be
//! `Polishing` returns [`IntentError::Polishing`] (the polish job itself holds
//! the reservation only while it applies results, never across harness calls).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use smithagents_core::event::EventKind;
use smithagents_core::ids::{AgentId, IntentId, RepoId};
use smithagents_core::intent::{
    Content, CreateIntentRequest, Intent, IntentFilter, IntentKind, IntentState, IntentVersion,
    Patch, Phase, Seed, SeedInput, SeedSource, VersionAuthor,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::events::EventBus;
use crate::harness::Harness;
use crate::paths::Paths;
use crate::polish::{seed_from_transcript, SeedError, MAX_SEED_BYTES};
use crate::service::{AgentService, ServiceError};
use crate::store::{Store, StoreError};

const TITLE_MAX: usize = 120;
const DEFAULT_TITLE: &str = "Untitled intent";

/// Starts a polish pass for an intent in the background. Task 6 supplies the
/// real runner; the default does nothing.
pub trait PolishRunner: Send + Sync {
    fn start(&self, id: IntentId, pass: u8);
}

struct NoopPolishRunner;

impl PolishRunner for NoopPolishRunner {
    fn start(&self, _id: IntentId, _pass: u8) {}
}

#[derive(Debug, thiserror::Error)]
pub enum IntentError {
    #[error("intent not found")]
    NotFound,
    #[error("repo not found")]
    RepoNotFound,
    #[error("agent not found")]
    AgentNotFound,
    #[error("intent is {actual:?}, expected {expected}")]
    InvalidState {
        expected: &'static str,
        actual: IntentState,
    },
    #[error("intent is being polished")]
    Polishing,
    #[error("{0}")]
    Invalid(String),
    #[error("dependency would create a cycle")]
    DependencyCycle,
    #[error("dependency is in a different repository")]
    DependencyCrossRepo,
    #[error(transparent)]
    Service(ServiceError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<ServiceError> for IntentError {
    fn from(e: ServiceError) -> Self {
        match e {
            ServiceError::RepoNotFound => IntentError::RepoNotFound,
            ServiceError::AgentNotFound => IntentError::AgentNotFound,
            ServiceError::Store(s) => IntentError::Store(s),
            other => IntentError::Service(other),
        }
    }
}

impl IntentError {
    /// Stable machine code for the API.
    pub fn code(&self) -> &'static str {
        match self {
            IntentError::NotFound => "intent_not_found",
            IntentError::RepoNotFound => "repo_not_found",
            IntentError::AgentNotFound => "agent_not_found",
            IntentError::InvalidState { .. } => "invalid_state",
            IntentError::Polishing => "polishing",
            IntentError::Invalid(_) => "invalid",
            IntentError::DependencyCycle => "dependency_cycle",
            IntentError::DependencyCrossRepo => "dependency_cross_repo",
            IntentError::Service(_) => "service_error",
            IntentError::Store(_) => "store_error",
        }
    }
}

/// Exclusive right to mutate one intent; released on drop.
pub struct Reservation {
    _guard: OwnedMutexGuard<()>,
}

struct Inner {
    store: Store,
    bus: EventBus,
    harness: Arc<dyn Harness>,
    agents: AgentService,
    paths: Paths,
    polish: Mutex<Arc<dyn PolishRunner>>,
    locks: Mutex<HashMap<IntentId, Arc<AsyncMutex<()>>>>,
}

#[derive(Clone)]
pub struct IntentService {
    inner: Arc<Inner>,
}

impl IntentService {
    pub fn new(
        store: Store,
        bus: EventBus,
        harness: Arc<dyn Harness>,
        agents: AgentService,
        paths: Paths,
    ) -> Self {
        IntentService {
            inner: Arc::new(Inner {
                store,
                bus,
                harness,
                agents,
                paths,
                polish: Mutex::new(Arc::new(NoopPolishRunner)),
                locks: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Replaces the polish runner (builder form of [`Self::set_polish_runner`]).
    pub fn with_polish_runner(self, runner: Arc<dyn PolishRunner>) -> Self {
        self.set_polish_runner(runner);
        self
    }

    /// Replaces the polish runner; needed because the real runner holds a
    /// clone of this service.
    pub fn set_polish_runner(&self, runner: Arc<dyn PolishRunner>) {
        *self.inner.polish.lock().unwrap_or_else(|e| e.into_inner()) = runner;
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn bus(&self) -> &EventBus {
        &self.inner.bus
    }

    pub fn harness(&self) -> &Arc<dyn Harness> {
        &self.inner.harness
    }

    pub fn agents(&self) -> &AgentService {
        &self.inner.agents
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    /// Waits for and takes the per-intent mutation lock.
    pub async fn reserve(&self, id: IntentId) -> Reservation {
        let lock = {
            let mut locks = self.inner.locks.lock().unwrap_or_else(|e| e.into_inner());
            // Drop entries nobody holds or waits on, so the map stays small.
            locks.retain(|k, l| *k == id || Arc::strong_count(l) > 1);
            locks.entry(id).or_default().clone()
        };
        Reservation {
            _guard: lock.lock_owned().await,
        }
    }

    fn start_polish(&self, id: IntentId, pass: u8) {
        let runner = self
            .inner
            .polish
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        runner.start(id, pass);
    }

    pub async fn get(&self, id: IntentId) -> Result<Intent, IntentError> {
        self.inner
            .store
            .get_intent(id)
            .await?
            .ok_or(IntentError::NotFound)
    }

    pub async fn list(&self, filter: IntentFilter) -> Result<Vec<Intent>, IntentError> {
        Ok(self.inner.store.list_intents(filter).await?)
    }

    pub async fn versions(&self, id: IntentId) -> Result<Vec<IntentVersion>, IntentError> {
        self.get(id).await?;
        Ok(self.inner.store.list_versions(id).await?)
    }

    pub async fn create(&self, req: CreateIntentRequest) -> Result<Intent, IntentError> {
        let store = &self.inner.store;
        let (repo_id, seed) = match req.seed {
            SeedInput::Chat {
                agent_id,
                from_seq,
                to_seq,
            } => {
                let agent = store
                    .get_agent(agent_id)
                    .await?
                    .ok_or(IntentError::AgentNotFound)?;
                let messages = load_messages(store, agent_id).await?;
                let text = seed_from_transcript(&messages, from_seq, to_seq, MAX_SEED_BYTES)
                    .map_err(|e| match e {
                        SeedError::Empty => {
                            IntentError::Invalid("no user or assistant messages in range".into())
                        }
                        SeedError::BadRange => {
                            IntentError::Invalid("from_seq is greater than to_seq".into())
                        }
                    })?;
                let source = SeedSource::Chat {
                    agent_id,
                    session_id: agent.harness_session_id.clone(),
                    from_seq,
                    to_seq,
                };
                (req.repo_id.unwrap_or(agent.repo_id), Seed { source, text })
            }
            SeedInput::Text(text) => {
                let repo_id = req.repo_id.ok_or_else(|| {
                    IntentError::Invalid("a repository is required for a text seed".into())
                })?;
                if text.trim().is_empty() {
                    return Err(IntentError::Invalid("seed text is empty".into()));
                }
                (
                    repo_id,
                    Seed {
                        source: SeedSource::Text,
                        text,
                    },
                )
            }
        };
        store
            .get_repo(repo_id)
            .await?
            .ok_or(IntentError::RepoNotFound)?;

        let id = IntentId::new();
        let now = Utc::now();
        let content = Content {
            title: DEFAULT_TITLE.into(),
            kind: IntentKind::Task,
            current_behavior: String::new(),
            questions: vec![],
            preconditions: vec![],
            criteria: vec![],
            stories: vec![],
            depends_on: vec![],
        };
        let intent = Intent {
            id,
            repo_id,
            slug: self.provisional_slug(repo_id, id).await?,
            state: IntentState::Draft,
            phase: Phase::Polishing { pass: 1 },
            seed,
            content: content.clone(),
            agent_id: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
        };
        let first = IntentVersion {
            intent_id: id,
            version: 1,
            author: VersionAuthor::Seed,
            content,
            at: now,
        };
        store.insert_intent(&intent, &first).await?;
        self.inner
            .bus
            .publish(EventKind::IntentCreated(intent.clone()));
        self.start_polish(id, 1);
        Ok(intent)
    }

    pub async fn update(&self, id: IntentId, patch: Patch) -> Result<Intent, IntentError> {
        let _reservation = self.reserve(id).await;
        let store = &self.inner.store;
        let mut intent = self.get(id).await?;
        match intent.state {
            IntentState::Draft => {
                if matches!(intent.phase, Phase::Polishing { .. }) {
                    return Err(IntentError::Polishing);
                }
            }
            IntentState::Complete => {}
            actual => {
                return Err(IntentError::InvalidState {
                    expected: "draft or complete",
                    actual,
                })
            }
        }
        // Validate everything before touching the intent.
        let title = match patch.title {
            Some(t) => {
                let t = t.trim().to_string();
                if t.is_empty() || t.chars().count() > TITLE_MAX {
                    return Err(IntentError::Invalid(format!(
                        "title must be 1 to {TITLE_MAX} characters"
                    )));
                }
                Some(t)
            }
            None => None,
        };
        if let Some(repo_id) = patch.repo_id.filter(|r| *r != intent.repo_id) {
            store
                .get_repo(repo_id)
                .await?
                .ok_or(IntentError::RepoNotFound)?;
            intent.slug = self.provisional_slug(repo_id, id).await?;
            intent.repo_id = repo_id;
            // Dependencies must live in the same repository.
            intent.content.depends_on.clear();
        }
        if let Some(t) = title {
            intent.content.title = t;
        }
        if let Some(k) = patch.kind {
            intent.content.kind = k;
        }
        if let Some(b) = patch.current_behavior {
            intent.content.current_behavior = b;
        }
        intent.updated_at = Utc::now();
        store.save_intent(&intent).await?;
        self.inner
            .bus
            .publish(EventKind::IntentUpdated(intent.clone()));
        Ok(intent)
    }

    /// `intent-<first 8 hex of the id>`, lengthened one step at a time in the
    /// (practically impossible) case that the repository already has it.
    async fn provisional_slug(&self, repo_id: RepoId, id: IntentId) -> Result<String, IntentError> {
        let hex = id.0.simple().to_string();
        for len in (8..=hex.len()).step_by(4) {
            let slug = format!("intent-{}", &hex[..len]);
            if !self.inner.store.slug_exists(repo_id, &slug).await? {
                return Ok(slug);
            }
        }
        // A full id is unique, so this only trips on a corrupted store.
        Err(IntentError::Store(StoreError::Conflict(
            "slug collision".into(),
        )))
    }
}

async fn load_messages(
    store: &Store,
    agent_id: AgentId,
) -> Result<Vec<smithagents_core::agent::ChatMessage>, StoreError> {
    store.list_messages(agent_id, 0, u32::MAX).await
}
