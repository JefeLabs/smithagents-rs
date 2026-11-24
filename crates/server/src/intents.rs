//! Intent service: create, read, edit, version listing and the polish
//! pipeline (pass 1, steering questions, pass 2, retry, restart recovery).
//!
//! Mutations of one intent are serialized through [`IntentService::reserve`].
//! The choice is a per-intent `tokio::sync::Mutex`: a second mutation waits
//! its turn instead of failing. A mutation that finds the stored phase to be
//! `Polishing` returns [`IntentError::Polishing`] (the polish job itself holds
//! the reservation only while it applies results, never across harness calls).
//!
//! A polish pass runs as a background job whose claim on the intent is the
//! stored `Polishing{pass}` phase. The job always moves the intent on: to its
//! result, or to `Failed{pass, message}` on an error or a (caught) panic, with
//! a drop guard as the backstop when the job ends any other way. Jobs lost
//! with a daemon restart are failed by [`IntentService::recover`].

use std::any::Any;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use futures::FutureExt;
use smithagents_core::event::EventKind;
use smithagents_core::ids::{AgentId, IntentId, QuestionId, RepoId};
use smithagents_core::intent::{
    slugify, Answer, AnswerInput, Content, CreateIntentRequest, Intent, IntentFilter, IntentKind,
    IntentState, IntentVersion, ItemStatus, Patch, Phase, Seed, SeedInput, SeedSource,
    VersionAuthor,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::events::EventBus;
use crate::harness::{Harness, HarnessError};
use crate::paths::Paths;
use crate::polish::{
    apply_pass1, apply_pass2, pass1_request, pass2_request, seed_from_transcript, Candidate,
    DepContext, PolishError, SeedError, MAX_SEED_BYTES,
};
use crate::service::{AgentService, ServiceError};
use crate::store::{Store, StoreError};

const TITLE_MAX: usize = 120;
const DEFAULT_TITLE: &str = "Untitled intent";
/// The length limit `slugify` applies; numbered slugs keep to it too.
const SLUG_MAX: usize = 48;
/// Saves tried when concurrent writers keep taking the slug picked for a title.
const SLUG_SAVE_ATTEMPTS: usize = 8;
/// Attempts at recording a polish failure before leaving it to `recover`.
const FAIL_ATTEMPTS: u32 = 3;
const RESTART_MESSAGE: &str = "polish was interrupted by a daemon restart; retry";

/// Starts a polish pass for an intent in the background. Without one set,
/// the service runs passes itself (see [`IntentService::set_polish_runner`]).
pub trait PolishRunner: Send + Sync {
    fn start(&self, id: IntentId, pass: u8);
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
    /// The question was answered or skipped before (HTTP 409).
    #[error("question already answered")]
    AlreadyAnswered,
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
            IntentError::AlreadyAnswered => "already_answered",
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
    /// `None`: the service's own background runner.
    polish: Mutex<Option<Arc<dyn PolishRunner>>>,
    locks: Mutex<HashMap<IntentId, Arc<AsyncMutex<()>>>>,
}

#[derive(Clone)]
pub struct IntentService {
    inner: Arc<Inner>,
    /// Model for polish runs; `None` leaves the CLI's default.
    polish_model: Option<String>,
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
                polish: Mutex::new(None),
                locks: Mutex::new(HashMap::new()),
            }),
            polish_model: None,
        }
    }

    /// Sets the model polish passes ask the harness for.
    pub fn with_polish_model(mut self, model: Option<String>) -> Self {
        self.polish_model = model;
        self
    }

    /// Replaces the polish runner (builder form of [`Self::set_polish_runner`]).
    pub fn with_polish_runner(self, runner: Arc<dyn PolishRunner>) -> Self {
        self.set_polish_runner(runner);
        self
    }

    /// Hands polish starts to `runner` instead of the service's own
    /// background runner (tests use this to observe or suppress passes).
    pub fn set_polish_runner(&self, runner: Arc<dyn PolishRunner>) {
        *self.inner.polish.lock().unwrap_or_else(|e| e.into_inner()) = Some(runner);
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

    /// Starts `pass` for an intent whose stored phase is `Polishing{pass}`.
    fn start_polish(&self, id: IntentId, pass: u8) {
        let runner = self
            .inner
            .polish
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match runner {
            Some(runner) => runner.start(id, pass),
            None => {
                tokio::spawn(self.clone().polish_job(id, pass));
            }
        }
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
        touch(&mut intent);
        store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(intent)
    }

    /// Records the answer to one steering question; only in the questions
    /// phase. The answer that closes the last open question also moves the
    /// intent to `Polishing{pass: 2}` in the same store write, under the
    /// reservation, so exactly one caller starts pass 2.
    pub async fn answer(
        &self,
        id: IntentId,
        question_id: QuestionId,
        answer: AnswerInput,
    ) -> Result<Intent, IntentError> {
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        if intent.state != IntentState::Draft || intent.phase != Phase::Questions {
            return Err(IntentError::InvalidState {
                expected: "draft in the questions phase",
                actual: intent.state,
            });
        }
        let question = intent
            .content
            .questions
            .iter_mut()
            .find(|q| q.id == question_id)
            .ok_or_else(|| IntentError::Invalid("unknown question".into()))?;
        if question.answer != Answer::Unanswered {
            return Err(IntentError::AlreadyAnswered);
        }
        question.answer = match answer {
            AnswerInput::Skip => Answer::Skipped,
            AnswerInput::Option(option_id) => {
                if !question.options.iter().any(|o| o.id == option_id) {
                    return Err(IntentError::Invalid("unknown option".into()));
                }
                Answer::Option { option_id }
            }
        };
        let start_pass2 = intent.content.questions_open() == 0;
        if start_pass2 {
            intent.phase = Phase::Polishing { pass: 2 };
        }
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        drop(reservation);
        if start_pass2 {
            self.start_polish(id, 2);
        }
        Ok(intent)
    }

    /// Re-runs the pass recorded in `Phase::Failed`.
    pub async fn retry_polish(&self, id: IntentId) -> Result<Intent, IntentError> {
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        let pass = match (intent.state, &intent.phase) {
            (IntentState::Draft, Phase::Failed { pass: 2, .. }) => 2,
            (IntentState::Draft, Phase::Failed { .. }) => 1,
            (IntentState::Draft, Phase::Polishing { .. }) => return Err(IntentError::Polishing),
            (actual, _) => {
                return Err(IntentError::InvalidState {
                    expected: "draft with a failed polish",
                    actual,
                })
            }
        };
        intent.phase = Phase::Polishing { pass };
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        drop(reservation);
        self.start_polish(id, pass);
        Ok(intent)
    }

    /// For daemon start, before anything else runs: every intent still in
    /// `Polishing` lost its job with the previous daemon and becomes
    /// `Failed`, ready for a retry. Returns how many were changed.
    pub async fn recover(&self) -> Result<u64, IntentError> {
        let mut changed = 0;
        for intent in self.inner.store.intents_in_phase_polishing().await? {
            if let Phase::Polishing { pass } = intent.phase {
                if self
                    .record_failure(intent.id, pass, RESTART_MESSAGE.into())
                    .await?
                {
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    fn publish_updated(&self, intent: &Intent) {
        self.inner
            .bus
            .publish(EventKind::IntentUpdated(intent.clone()));
    }

    // ------------------------------------------------------------ polish job

    /// Runs one pass to its end: its result, or `Failed` on an error or a
    /// panic. The guard covers every other way out (task cancelled, a panic
    /// while recording the failure).
    async fn polish_job(self, id: IntentId, pass: u8) {
        let guard = PolishGuard {
            svc: Some(self.clone()),
            id,
            pass,
        };
        let failed = match AssertUnwindSafe(self.run_pass(id, pass))
            .catch_unwind()
            .await
        {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                tracing::warn!(intent_id = %id, pass, "polish pass failed: {e}");
                Some(e.user_message(pass))
            }
            Err(panic) => {
                tracing::error!(
                    intent_id = %id,
                    pass,
                    "polish pass panicked: {}",
                    panic_text(&*panic)
                );
                Some(stopped_message(pass))
            }
        };
        if let Some(message) = failed {
            self.fail_polish(id, pass, message).await;
        }
        guard.disarm();
    }

    async fn run_pass(&self, id: IntentId, pass: u8) -> Result<(), PassFailure> {
        if pass == 2 {
            self.run_pass2(id).await
        } else {
            self.run_pass1(id).await
        }
    }

    async fn run_pass1(&self, id: IntentId) -> Result<(), PassFailure> {
        let Some(intent) = self.polishing(id, 1).await? else {
            return Ok(());
        };
        let root = self.repo_root(&intent).await?;
        let request = pass1_request(&intent, &root, self.polish_model.clone());
        let raw = self.inner.harness.run_structured(request).await?.value;
        let mut polished = intent.content.clone();
        apply_pass1(&mut polished, &raw)?;

        let reservation = self.reserve(id).await;
        // Nothing else changes an intent while it polishes; re-read anyway so
        // a superseded job (e.g. failed by `recover`) cannot overwrite it.
        let Some(mut intent) = self.polishing(id, 1).await? else {
            return Ok(());
        };
        let c = &mut intent.content;
        c.title = polished.title;
        c.kind = polished.kind;
        c.current_behavior = polished.current_behavior;
        c.questions = polished.questions;
        let start_pass2 = c.questions.is_empty();
        intent.phase = if start_pass2 {
            Phase::Polishing { pass: 2 }
        } else {
            Phase::Questions
        };
        touch(&mut intent);
        self.append_version(&intent, VersionAuthor::Polish1).await?;
        self.save_with_title_slug(&mut intent).await?;
        self.publish_updated(&intent);
        drop(reservation);
        if start_pass2 {
            self.start_polish(id, 2);
        }
        Ok(())
    }

    async fn run_pass2(&self, id: IntentId) -> Result<(), PassFailure> {
        let Some(intent) = self.polishing(id, 2).await? else {
            return Ok(());
        };
        let root = self.repo_root(&intent).await?;
        let ctx = self.dep_context(&intent).await?;
        let request = pass2_request(&intent, &root, self.polish_model.clone(), &ctx.candidates);
        let raw = self.inner.harness.run_structured(request).await?.value;
        let mut polished = intent.content.clone();
        let report = apply_pass2(&mut polished, &raw, &ctx)?;
        for (given, why) in &report.dropped_dependencies {
            tracing::debug!(intent_id = %id, candidate = %given, "dropped a proposed dependency: {why}");
        }

        let _reservation = self.reserve(id).await;
        let Some(mut intent) = self.polishing(id, 2).await? else {
            return Ok(());
        };
        let c = &mut intent.content;
        c.preconditions = polished.preconditions;
        c.criteria = polished.criteria;
        c.stories = polished.stories;
        c.depends_on = polished.depends_on;
        intent.phase = Phase::Review;
        // Every new item is pending, so this is Complete only when all four
        // sections came back empty.
        intent.state = if intent.is_complete_content() {
            IntentState::Complete
        } else {
            IntentState::Draft
        };
        touch(&mut intent);
        self.append_version(&intent, VersionAuthor::Polish2).await?;
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(())
    }

    /// The intent, if it still exists and is in `Polishing{pass}`.
    async fn polishing(&self, id: IntentId, pass: u8) -> Result<Option<Intent>, StoreError> {
        let intent = self.inner.store.get_intent(id).await?;
        Ok(intent.filter(|i| i.phase == Phase::Polishing { pass }))
    }

    async fn repo_root(&self, intent: &Intent) -> Result<PathBuf, PassFailure> {
        let repo = self.inner.store.get_repo(intent.repo_id).await?;
        Ok(repo.ok_or(PassFailure::RepoMissing)?.path)
    }

    /// Candidates are the other intents of the same repository, in any state;
    /// edges are their confirmed (accepted) depends-on items.
    async fn dep_context(&self, intent: &Intent) -> Result<DepContext, StoreError> {
        let same_repo = self
            .inner
            .store
            .list_intents(IntentFilter {
                state: None,
                repo_id: Some(intent.repo_id),
            })
            .await?;
        let mut candidates = Vec::new();
        let mut edges = HashMap::new();
        for other in same_repo.into_iter().filter(|o| o.id != intent.id) {
            let confirmed: Vec<IntentId> = other
                .content
                .depends_on
                .iter()
                .filter(|d| d.status == ItemStatus::Accepted)
                .filter_map(|d| d.intent_ref)
                .collect();
            if !confirmed.is_empty() {
                edges.insert(other.id, confirmed);
            }
            candidates.push(Candidate {
                id: other.id,
                slug: other.slug,
                title: other.content.title,
                state: other.state,
            });
        }
        Ok(DepContext {
            self_id: intent.id,
            candidates,
            edges,
        })
    }

    async fn append_version(
        &self,
        intent: &Intent,
        author: VersionAuthor,
    ) -> Result<u32, StoreError> {
        self.inner
            .store
            .append_version(&IntentVersion {
                intent_id: intent.id,
                version: 0,
                author,
                content: intent.content.clone(),
                at: intent.updated_at,
            })
            .await
    }

    /// Saves the intent. While it has never been sent, its slug first follows
    /// the title: `slugify(title)`, then `-2`, `-3`... until unique in the
    /// repository. A concurrent writer taking the slug between the check and
    /// the save makes the save conflict, and the slug is picked again.
    async fn save_with_title_slug(&self, intent: &mut Intent) -> Result<(), StoreError> {
        if intent.sent_at.is_some() {
            return self.inner.store.save_intent(intent).await;
        }
        let stored_slug = intent.slug.clone();
        let base = slugify(&intent.content.title);
        let mut attempt = 1;
        loop {
            intent.slug = self.free_slug(intent.repo_id, &base, &stored_slug).await?;
            match self.inner.store.save_intent(intent).await {
                Err(StoreError::Conflict(_)) if attempt < SLUG_SAVE_ATTEMPTS => attempt += 1,
                other => return other,
            }
        }
    }

    /// The first of `base`, `base-2`, `base-3`... not taken in the repository
    /// by another intent (`own` is the intent's current slug).
    async fn free_slug(
        &self,
        repo_id: RepoId,
        base: &str,
        own: &str,
    ) -> Result<String, StoreError> {
        let mut n = 1;
        loop {
            let slug = numbered_slug(base, n);
            if slug == own || !self.inner.store.slug_exists(repo_id, &slug).await? {
                return Ok(slug);
            }
            n += 1;
        }
    }

    /// Records `Failed{pass, message}`, retrying a failing store a few times;
    /// if it still fails, the next daemon start's [`Self::recover`] does it.
    async fn fail_polish(&self, id: IntentId, pass: u8, message: String) {
        for attempt in 1..=FAIL_ATTEMPTS {
            match self.record_failure(id, pass, message.clone()).await {
                Ok(_) => return,
                Err(e) => {
                    tracing::error!(intent_id = %id, pass, attempt, "could not record a polish failure: {e}");
                    if attempt < FAIL_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
                    }
                }
            }
        }
    }

    /// Sets `Failed{pass, message}` if the intent is still in
    /// `Polishing{pass}`; true when it did.
    async fn record_failure(
        &self,
        id: IntentId,
        pass: u8,
        message: String,
    ) -> Result<bool, StoreError> {
        let _reservation = self.reserve(id).await;
        let Some(mut intent) = self.polishing(id, pass).await? else {
            return Ok(false);
        };
        intent.phase = Phase::Failed { pass, message };
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(true)
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

/// Sets `updated_at` to now, and strictly after its previous value, so the
/// updates published for one intent are ordered in time even on a coarse clock.
fn touch(intent: &mut Intent) {
    let now = Utc::now();
    intent.updated_at = if now > intent.updated_at {
        now
    } else {
        intent.updated_at + TimeDelta::microseconds(1)
    };
}

/// `base` for 1, else `base-n` with `base` shortened so the result stays
/// within the slug limit (`base` is a `slugify` result, so ASCII).
fn numbered_slug(base: &str, n: u32) -> String {
    if n <= 1 {
        return base.to_string();
    }
    let suffix = format!("-{n}");
    let keep = SLUG_MAX.saturating_sub(suffix.len()).min(base.len());
    let stem = base[..keep].trim_end_matches('-');
    let stem = if stem.is_empty() { "intent" } else { stem };
    format!("{stem}{suffix}")
}

/// Why a polish pass produced no result. Only [`Self::user_message`] reaches
/// the intent; the details (which may quote model output) go to the log.
#[derive(Debug, thiserror::Error)]
enum PassFailure {
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Output(#[from] PolishError),
    #[error("the intent's repository is not registered")]
    RepoMissing,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl PassFailure {
    /// A fixed, user-readable sentence: never prompt text or model output.
    fn user_message(&self, pass: u8) -> String {
        match self {
            PassFailure::Harness(HarnessError::Timeout) => {
                format!("polish pass {pass} timed out; retry")
            }
            PassFailure::Harness(HarnessError::Spawn(_)) => format!(
                "polish pass {pass} could not start the claude CLI; check that it is installed, then retry"
            ),
            PassFailure::Harness(HarnessError::Failed(_)) => {
                format!("polish pass {pass} failed in the claude CLI; retry")
            }
            PassFailure::Harness(HarnessError::InvalidOutput(_)) => {
                format!("polish pass {pass} got unreadable output from the model; retry")
            }
            PassFailure::Output(_) => {
                format!("polish pass {pass} got output from the model that could not be used; retry")
            }
            PassFailure::RepoMissing => {
                format!("polish pass {pass} could not find the intent's repository")
            }
            PassFailure::Store(_) => format!("polish pass {pass} could not save its result; retry"),
        }
    }
}

fn stopped_message(pass: u8) -> String {
    format!("polish pass {pass} stopped unexpectedly; retry")
}

fn panic_text(payload: &(dyn Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s
    } else {
        "non-string panic payload"
    }
}

/// Armed for the life of a polish job; dropped armed (the job was cancelled,
/// or a panic escaped it), it records the failure from a new task so the
/// intent does not stay `Polishing`.
struct PolishGuard {
    svc: Option<IntentService>,
    id: IntentId,
    pass: u8,
}

impl PolishGuard {
    fn disarm(mut self) {
        self.svc = None;
    }
}

impl Drop for PolishGuard {
    fn drop(&mut self) {
        let Some(svc) = self.svc.take() else {
            return;
        };
        let (id, pass) = (self.id, self.pass);
        tracing::warn!(intent_id = %id, pass, "polish job ended without an outcome");
        // A runtime that is shutting down drops the task; `recover` then
        // fails the intent at the next start.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move { svc.fail_polish(id, pass, stopped_message(pass)).await });
        }
    }
}
