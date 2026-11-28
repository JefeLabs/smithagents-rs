//! Intent service: create, read, edit, version listing and the polish
//! pipeline (pass 1, steering questions, pass 2, retry, restart recovery).
//!
//! Mutations of one intent are serialized through [`IntentService::reserve`].
//! The choice is a per-intent `tokio::sync::Mutex`: a second mutation waits
//! its turn instead of failing. `update` and `retry_polish` refuse an intent
//! whose stored phase is `Polishing` with [`IntentError::Polishing`]; `answer`
//! is valid only in the questions phase and refuses every other phase with
//! [`IntentError::InvalidState`], naming the phase. The polish job itself
//! holds the reservation only while it applies results, never across harness
//! calls. A review that confirms a dependency also takes a service-wide
//! dependency lock across its cycle check and save. Locks are always taken in
//! one order: the start lock, then a reservation, then the dependency lock.
//!
//! A polish pass runs as a background job whose claim on the intent is the
//! stored `Polishing{pass}` phase. The job always moves the intent on: to its
//! result, or to `Failed{pass, message}` on an error or a (caught) panic, with
//! a drop guard as the backstop when the job ends any other way. Jobs lost
//! with a daemon restart are failed by [`IntentService::recover`]. The calls
//! that write `Polishing{pass}` (`create`, `answer`, `retry_polish`) run on
//! their own task, so a caller that stops waiting cannot separate that write
//! from the start of the pass.
//!
//! Execution: [`IntentService::send`] accepts a `Complete` intent and either
//! starts its agent at once or leaves it `Queued` for the scheduler
//! ([`crate::queue`]), which calls [`IntentService::start`] when its
//! dependencies are `Resolved` and a slot is free. Deciding to start and
//! starting happen under one service-wide start lock (`send` and every
//! scheduler pass take it), so intent starts never overrun the queue limit
//! by a race (agents the user messages directly may; they never wait).
//! A start re-reads the intent under its reservation, does nothing to an
//! intent already `Sent`, and refuses (recording why in `last_start_error`)
//! while an open agent is attached to the intent, as a start interrupted
//! between creating the agent and saving `Sent` leaves it; so an intent never
//! gets a second agent. `send`, `start`, `resolve` and `set_agent_defaults`
//! run detached like `create`.
//!
//! On a repository with no source control the agent works in the folder
//! itself, so its intents take turns: the queue starts none on a folder
//! that `IntentService::held_folders` names, and Send on such a folder
//! queues the intent.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use chrono::{DateTime, TimeDelta, Utc};
use futures::FutureExt;
use smithagents_core::agent::{AgentState, Repo};
use smithagents_core::event::{Event, EventKind};
use smithagents_core::feedback::{FeedbackOrigin, IdeaPackage};
use smithagents_core::ids::{AgentId, IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    slugify, AgentDefaults, Answer, AnswerInput, Content, CreateIntentRequest, Intent,
    IntentFilter, IntentKind, IntentState, IntentVersion, Item, ItemStatus, Patch, Phase,
    ReviewAction, Seed, SeedInput, SeedSource, VersionAuthor,
};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard, OwnedMutexGuard};

use crate::brief::{self, DepSummary};
use crate::events::EventBus;
use crate::git::{self, GitError};
use crate::harness::{Harness, HarnessError};
use crate::paths::Paths;
use crate::polish::{
    apply_pass1, apply_pass2, pass1_request, pass2_request, seed_from_transcript,
    would_create_cycle, Candidate, DepContext, PolishError, SeedError, MAX_SEED_BYTES,
};
use crate::queue::{self, Kicker};
use crate::service::{AgentService, CreateAgentOptions, ServiceError};
use crate::store::{Store, StoreError};

/// Settings key of [`AgentDefaults`].
const AGENT_DEFAULTS_KEY: &str = "agent_defaults";
/// Highest queue limit accepted (0 or none means no limit).
const QUEUE_LIMIT_MAX: u32 = 64;
/// Hex digits of the intent id appended to an intent branch name that is taken.
const BRANCH_SUFFIX_HEX: usize = 6;
/// Why a start fails when the stored base branch is not a usable branch name
/// (the name is not repeated).
const BASE_BRANCH_INVALID: &str = "the branch this intent starts from no longer exists";
/// How long to wait for a turn to end before trying again to close the agent
/// of a failed start (in case the end of the turn was never published).
const CLOSE_RETRY: Duration = Duration::from_secs(30);

const TITLE_MAX: usize = 120;
const ITEM_MAX: usize = 2000;
/// Characters allowed in a current behavior set through `update`: what
/// polish holds the model to as well.
const BEHAVIOR_MAX: usize = crate::polish::MAX_BEHAVIOR_CHARS;
const DEFAULT_TITLE: &str = "Untitled intent";
/// The length limit `slugify` applies; numbered slugs keep to it too.
const SLUG_MAX: usize = 48;
/// Saves tried when concurrent writers keep taking the slug picked for a title.
const SLUG_SAVE_ATTEMPTS: usize = 8;
/// Attempts at recording a polish failure before leaving it to `recover`.
const FAIL_ATTEMPTS: u32 = 3;
const RESTART_MESSAGE: &str = "polish was interrupted by a daemon restart; retry";
/// Why a fixed item cannot be reviewed away.
const FIXED_ITEM: &str = "this item is fixed: it cannot be edited or dropped";

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
    /// Wrong state, or (`phase` set) a draft in the wrong phase.
    #[error("intent is {}, expected {expected}", state_text(.actual, .phase))]
    InvalidState {
        expected: &'static str,
        actual: IntentState,
        phase: Option<Phase>,
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
    /// The intent has an origin: the files it was made from are on a branch
    /// of its repository, so it cannot move to another one (HTTP 409).
    #[error("This intent's evidence is in its repository, so the repository cannot be changed.")]
    EvidenceInRepo,
    /// The intent has a fixed dependency, which must stay in its repository
    /// (HTTP 409).
    #[error(
        "This intent depends on another intent in its repository, so the repository cannot be changed."
    )]
    FixedDependency,
    /// The agent's worktree could not be made from the intent's base branch
    /// (HTTP 409); what git said is not kept.
    #[error("The intent's base branch could not be checked out.")]
    BaseBranchFailed,
    /// The intent names a branch to start from, or files on one, and its
    /// repository has no source control (HTTP 409).
    #[error("This intent needs a repository with source control.")]
    NeedsSourceControl,
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
            IntentError::EvidenceInRepo => "evidence_in_repo",
            IntentError::FixedDependency => "fixed_dependency",
            IntentError::BaseBranchFailed => "base_branch_failed",
            IntentError::NeedsSourceControl => "needs_source_control",
            IntentError::Service(_) => "service_error",
            IntentError::Store(_) => "store_error",
        }
    }

    /// The HTTP status and the code this error is answered with, wherever it
    /// is answered: a wrapped agent-service error keeps that error's own.
    pub fn status_and_code(&self) -> (StatusCode, &'static str) {
        use StatusCode as S;
        let status = match self {
            IntentError::Service(inner) => return inner.status_and_code(),
            IntentError::NotFound | IntentError::RepoNotFound | IntentError::AgentNotFound => {
                S::NOT_FOUND
            }
            IntentError::InvalidState { .. }
            | IntentError::Polishing
            | IntentError::AlreadyAnswered
            | IntentError::DependencyCycle
            | IntentError::DependencyCrossRepo
            | IntentError::EvidenceInRepo
            | IntentError::FixedDependency
            | IntentError::BaseBranchFailed
            | IntentError::NeedsSourceControl => S::CONFLICT,
            IntentError::Invalid(_) => S::BAD_REQUEST,
            IntentError::Store(_) => S::INTERNAL_SERVER_ERROR,
        };
        (status, self.code())
    }
}

/// What [`IntentService::create_with`] adds to a new intent beyond the
/// request.
///
/// The fixed items are accepted from the start and stay as they are: polish
/// pass 2 keeps them first ([`crate::polish::apply_pass2`]) and a review
/// cannot edit or drop them.
#[derive(Debug, Clone, Default)]
pub struct CreateExtras {
    /// The branch the intent's agent starts from.
    pub base_branch: Option<String>,
    /// Where a Submit-made intent's files are.
    pub origin: Option<FeedbackOrigin>,
    /// Acceptance criteria, in order (an idea package's).
    pub fixed_criteria: Vec<String>,
    /// An intent of the same repository this one depends on (an
    /// implementation intent's idea intent); the item's text is its title.
    pub fixed_dependency: Option<IntentId>,
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
    /// Model for polish runs; `None` leaves the CLI's default.
    polish_model: Mutex<Option<String>>,
    locks: Mutex<HashMap<IntentId, Arc<AsyncMutex<()>>>>,
    /// Wakes the queue scheduler; `None` until one is connected.
    scheduler: Mutex<Option<Kicker>>,
    /// Held while deciding whether to start intents and starting them (by
    /// `send` and by each scheduler pass). Taken before any reservation.
    starts: AsyncMutex<()>,
    /// Held by a review that confirms a depends-on item, from its cycle
    /// check (which reads other intents' edges) through its save, so two
    /// confirmations cannot each pass the check and close a cycle together.
    /// Taken after the intent's reservation.
    dependencies: AsyncMutex<()>,
    /// Runs at the beginning of every start (see [`IntentService::set_start_hook`]).
    #[cfg(feature = "test-util")]
    start_hook: Mutex<Option<StartHook>>,
}

/// A test hook run with the intent's id at the beginning of every start.
#[cfg(feature = "test-util")]
pub type StartHook = Arc<dyn Fn(IntentId) + Send + Sync>;

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
                polish: Mutex::new(None),
                polish_model: Mutex::new(None),
                locks: Mutex::new(HashMap::new()),
                scheduler: Mutex::new(None),
                starts: AsyncMutex::new(()),
                dependencies: AsyncMutex::new(()),
                #[cfg(feature = "test-util")]
                start_hook: Mutex::new(None),
            }),
        }
    }

    /// Test hook: `hook` runs at the beginning of every start, before the
    /// agent is made (tests use it to make a start panic). `None` removes it.
    #[cfg(feature = "test-util")]
    pub fn set_start_hook(&self, hook: Option<StartHook>) {
        *self
            .inner
            .start_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    #[cfg(feature = "test-util")]
    fn run_start_hook(&self, id: IntentId) {
        let hook = self
            .inner
            .start_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(id);
        }
    }

    /// Connects the queue scheduler, which `send`, `resolve` and
    /// `set_agent_defaults` then wake ([`crate::queue::Scheduler::start`]
    /// does this). Holds only the kick channel, never the scheduler task.
    pub fn set_scheduler(&self, kicker: Kicker) {
        *self
            .inner
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(kicker);
    }

    fn kick_scheduler(&self) {
        let kicker = self
            .inner
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(kicker) = kicker {
            kicker.kick();
        }
    }

    /// The start lock (see the module docs).
    pub(crate) async fn lock_starts(&self) -> AsyncMutexGuard<'_, ()> {
        self.inner.starts.lock().await
    }

    /// Sets the model polish passes ask the harness for (shared by all
    /// clones of this service).
    pub fn with_polish_model(self, model: Option<String>) -> Self {
        *self
            .inner
            .polish_model
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = model;
        self
    }

    fn polish_model(&self) -> Option<String> {
        self.inner
            .polish_model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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

    /// Creates a draft from a chat or text seed and starts polish pass 1.
    pub async fn create(&self, req: CreateIntentRequest) -> Result<Intent, IntentError> {
        self.create_with(req, CreateExtras::default()).await
    }

    /// [`Self::create`], with what the daemon's own callers (Submit) add to
    /// the new intent: its base branch and origin are stored with it, and
    /// its fixed items are in its content from the first version on. A
    /// fixed dependency on an intent that does not exist is `Invalid`; on
    /// one in another repository, `DependencyCrossRepo`.
    pub async fn create_with(
        &self,
        req: CreateIntentRequest,
        extras: CreateExtras,
    ) -> Result<Intent, IntentError> {
        let svc = self.clone();
        detached(async move { svc.create_now(req, extras).await }).await
    }

    /// Body of [`Self::create_with`], run detached from the caller.
    async fn create_now(
        &self,
        req: CreateIntentRequest,
        extras: CreateExtras,
    ) -> Result<Intent, IntentError> {
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
                (
                    req.repo_id.unwrap_or(agent.repo_id),
                    Seed {
                        source,
                        text,
                        kind: req.kind,
                    },
                )
            }
            SeedInput::Text(text) => {
                let repo_id = req.repo_id.ok_or_else(|| {
                    IntentError::Invalid("a repository is required for a text seed".into())
                })?;
                if text.trim().is_empty() {
                    return Err(IntentError::Invalid("seed text is empty".into()));
                }
                // A chat seed is cut to this size; typed text is refused instead.
                if text.len() > MAX_SEED_BYTES {
                    return Err(IntentError::Invalid(format!(
                        "seed text must be at most {MAX_SEED_BYTES} bytes"
                    )));
                }
                (
                    repo_id,
                    Seed {
                        source: SeedSource::Text,
                        text,
                        kind: req.kind,
                    },
                )
            }
        };
        store
            .get_repo(repo_id)
            .await?
            .ok_or(IntentError::RepoNotFound)?;
        let CreateExtras {
            base_branch,
            origin,
            fixed_criteria,
            fixed_dependency,
        } = extras;
        let depends_on = match fixed_dependency {
            None => vec![],
            Some(target) => {
                let other = store.get_intent(target).await?.ok_or_else(|| {
                    IntentError::Invalid("the intent this one depends on does not exist".into())
                })?;
                if other.repo_id != repo_id {
                    return Err(IntentError::DependencyCrossRepo);
                }
                vec![fixed_item(other.content.title, Some(target))]
            }
        };

        let id = IntentId::new();
        let now = Utc::now();
        let content = Content {
            title: DEFAULT_TITLE.into(),
            kind: seed.kind.unwrap_or(IntentKind::Task),
            current_behavior: String::new(),
            questions: vec![],
            preconditions: vec![],
            criteria: fixed_criteria
                .into_iter()
                .map(|text| fixed_item(text, None))
                .collect(),
            stories: vec![],
            depends_on,
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
            base_branch,
            origin,
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
                    phase: None,
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
        if patch
            .current_behavior
            .as_ref()
            .is_some_and(|b| b.chars().count() > BEHAVIOR_MAX)
        {
            return Err(IntentError::Invalid(format!(
                "current behavior must be at most {BEHAVIOR_MAX} characters"
            )));
        }
        let new_repo = patch.repo_id.filter(|r| *r != intent.repo_id);
        // A fixed dependency (which a move would clear) is on an intent of
        // this repository; checked first, origin or not.
        if new_repo.is_some() && intent.content.depends_on.iter().any(|d| d.fixed) {
            return Err(IntentError::FixedDependency);
        }
        // Its agent starts from a branch of this repository that holds its
        // files: in another repository that branch is not there, or is not
        // the same.
        if new_repo.is_some() && intent.origin.is_some() {
            return Err(IntentError::EvidenceInRepo);
        }
        if title.is_none()
            && new_repo.is_none()
            && patch.kind.is_none()
            && patch.current_behavior.is_none()
        {
            return Ok(intent);
        }
        if let Some(repo_id) = new_repo {
            store
                .get_repo(repo_id)
                .await?
                .ok_or(IntentError::RepoNotFound)?;
            // Keep the slug unless the new repository already uses it.
            if store.slug_exists(repo_id, &intent.slug).await? {
                intent.slug = self.free_slug(repo_id, &intent.slug, "").await?;
            }
            intent.repo_id = repo_id;
            // Dependencies must live in the same repository.
            intent.content.depends_on.clear();
        }
        if let Some(t) = title {
            intent.content.title = t;
        }
        if let Some(k) = patch.kind {
            intent.content.kind = k;
            // Pass 1 (and its retry) keeps the seed's kind, so the user's
            // choice must land there too.
            intent.seed.kind = Some(k);
        }
        if let Some(b) = patch.current_behavior {
            intent.content.current_behavior = b;
        }
        recompute_state(&mut intent);
        touch(&mut intent);
        store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(intent)
    }

    /// Accepts, edits or drops one item. Valid in a draft's review phase and
    /// in a complete intent (a changed decision reopens or keeps it). A
    /// fixed item cannot be edited or dropped (`Invalid`); accepting it
    /// again changes nothing.
    pub async fn review(
        &self,
        id: IntentId,
        item_id: ItemId,
        action: ReviewAction,
    ) -> Result<Intent, IntentError> {
        let svc = self.clone();
        detached(async move { svc.review_now(id, item_id, action).await }).await
    }

    /// Body of [`Self::review`], run detached from the caller.
    async fn review_now(
        &self,
        id: IntentId,
        item_id: ItemId,
        action: ReviewAction,
    ) -> Result<Intent, IntentError> {
        let _reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        let open = match intent.state {
            IntentState::Draft => intent.phase == Phase::Review,
            IntentState::Complete => true,
            _ => false,
        };
        if !open {
            return Err(wrong_state_or_phase(&intent, "the review phase"));
        }
        let c = &intent.content;
        let fixed = c
            .preconditions
            .iter()
            .chain(&c.criteria)
            .chain(&c.stories)
            .chain(&c.depends_on)
            .any(|i| i.id == item_id && i.fixed);
        if fixed {
            return match action {
                ReviewAction::Accept => Ok(intent),
                ReviewAction::Edit(_) | ReviewAction::Drop => {
                    Err(IntentError::Invalid(FIXED_ITEM.into()))
                }
            };
        }
        let text = match &action {
            ReviewAction::Edit(t) => {
                let t = t.trim();
                if t.is_empty() || t.chars().count() > ITEM_MAX {
                    return Err(IntentError::Invalid(format!(
                        "text must be 1 to {ITEM_MAX} characters"
                    )));
                }
                Some(t.to_string())
            }
            _ => None,
        };
        let is_dep = intent.content.depends_on.iter().any(|d| d.id == item_id);
        // Held until the save below (see `Inner::dependencies`).
        let _confirming = if is_dep && !matches!(action, ReviewAction::Drop) {
            let guard = self.inner.dependencies.lock().await;
            let target = intent
                .content
                .depends_on
                .iter()
                .find(|d| d.id == item_id)
                .and_then(|d| d.intent_ref);
            self.check_dependency(&intent, target).await?;
            Some(guard)
        } else {
            None
        };
        let c = &mut intent.content;
        let item = c
            .preconditions
            .iter_mut()
            .chain(c.criteria.iter_mut())
            .chain(c.stories.iter_mut())
            .chain(c.depends_on.iter_mut())
            .find(|i| i.id == item_id)
            .ok_or_else(|| IntentError::Invalid("unknown item".into()))?;
        match (action, text) {
            (ReviewAction::Accept, _) => item.status = ItemStatus::Accepted,
            (ReviewAction::Drop, _) => item.status = ItemStatus::Dropped,
            (ReviewAction::Edit(_), t) => {
                item.text = t.unwrap_or_default();
                item.edited = true;
                item.status = ItemStatus::Accepted;
            }
        }
        recompute_state(&mut intent);
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(intent)
    }

    /// A dependency about to be confirmed: the target must still exist, be in
    /// the intent's repository, and not close a cycle with the confirmed edges.
    async fn check_dependency(
        &self,
        intent: &Intent,
        target: Option<IntentId>,
    ) -> Result<(), IntentError> {
        let target =
            target.ok_or_else(|| IntentError::Invalid("dependency has no target".into()))?;
        let other = self
            .inner
            .store
            .get_intent(target)
            .await?
            .ok_or_else(|| IntentError::Invalid("the dependency no longer exists".into()))?;
        if other.repo_id != intent.repo_id {
            return Err(IntentError::DependencyCrossRepo);
        }
        let same_repo = self
            .inner
            .store
            .list_intents(IntentFilter {
                state: None,
                repo_id: Some(intent.repo_id),
            })
            .await?;
        let edges: HashMap<IntentId, Vec<IntentId>> = same_repo
            .into_iter()
            .filter(|o| o.id != intent.id)
            .filter_map(|o| {
                let confirmed: Vec<IntentId> = o
                    .content
                    .depends_on
                    .iter()
                    .filter(|d| d.status == ItemStatus::Accepted)
                    .filter_map(|d| d.intent_ref)
                    .collect();
                (!confirmed.is_empty()).then_some((o.id, confirmed))
            })
            .collect();
        if would_create_cycle(&edges, intent.id, target) {
            return Err(IntentError::DependencyCycle);
        }
        Ok(())
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
        let svc = self.clone();
        detached(async move { svc.answer_now(id, question_id, answer).await }).await
    }

    /// Body of [`Self::answer`], run detached from the caller.
    async fn answer_now(
        &self,
        id: IntentId,
        question_id: QuestionId,
        answer: AnswerInput,
    ) -> Result<Intent, IntentError> {
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        if intent.phase != Phase::Questions || intent.state != IntentState::Draft {
            return Err(wrong_state_or_phase(&intent, "the questions phase"));
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
        let svc = self.clone();
        detached(async move { svc.retry_polish_now(id).await }).await
    }

    /// Body of [`Self::retry_polish`], run detached from the caller.
    async fn retry_polish_now(&self, id: IntentId) -> Result<Intent, IntentError> {
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        let pass = match (intent.state, &intent.phase) {
            (IntentState::Draft, Phase::Failed { pass: 2, .. }) => 2,
            (IntentState::Draft, Phase::Failed { .. }) => 1,
            (IntentState::Draft, Phase::Polishing { .. }) => return Err(IntentError::Polishing),
            _ => return Err(wrong_state_or_phase(&intent, "a failed polish")),
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

    // ------------------------------------------------------------- execution

    /// Sends a `Complete` intent to a new agent, writing its `Send` version.
    /// It starts now when every accepted dependency is `Resolved`, the queue
    /// has a slot for it (older ready intents go first) and its folder is not
    /// held; otherwise it becomes `Queued` for the scheduler. So it does when
    /// another agent's turn takes its plain folder before the new agent's
    /// first message: `folder_busy` is answered only to a chat message. A
    /// failed immediate start returns the error and leaves the intent
    /// `Complete` with `last_start_error` set.
    pub async fn send(&self, id: IntentId) -> Result<Intent, IntentError> {
        let svc = self.clone();
        detached(async move { svc.send_now(id).await }).await
    }

    /// Body of [`Self::send`], run detached from the caller.
    async fn send_now(&self, id: IntentId) -> Result<Intent, IntentError> {
        let starts = self.lock_starts().await;
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        if intent.state != IntentState::Complete {
            return Err(wrong_state_or_phase(&intent, "complete"));
        }
        let store = &self.inner.store;
        let now = Utc::now();
        // Decide as the scheduler would, with this intent queued last.
        let mut candidate = intent.clone();
        candidate.state = IntentState::Queued;
        candidate.queued_at = Some(now);
        let mut queued = store.queued_intents().await?;
        queued.push(candidate);
        let resolved = queue::resolved_ids(store).await?;
        let running = queue::running_count(&self.inner.agents);
        let held = self.held_folders().await?;
        let limit = self.agent_defaults().await?.queue_limit;
        if queue::next_startable(&queued, &resolved, running, limit, &held).contains(&id) {
            return self.start_reserved(intent).await;
        }
        self.save_queued(&mut intent, now).await?;
        drop(reservation);
        drop(starts);
        self.kick_scheduler();
        Ok(intent)
    }

    /// Saves a `Complete` intent as `Queued` at `queued_at`, with its `Send`
    /// version and no start error, as Send leaves an intent that cannot start
    /// now, and publishes it.
    async fn save_queued(
        &self,
        intent: &mut Intent,
        queued_at: DateTime<Utc>,
    ) -> Result<(), IntentError> {
        intent.state = IntentState::Queued;
        intent.queued_at = Some(queued_at);
        intent.last_start_error = None;
        touch(intent);
        self.inner
            .store
            .save_intent_with_version(intent, &snapshot(intent, VersionAuthor::Send))
            .await?;
        self.publish_updated(intent);
        Ok(())
    }

    /// Plain repositories the queue may not start an intent on now. Their
    /// agents work in the folder itself, so one intent has it at a time: a
    /// plain repository is held while a turn is in flight in it, or while
    /// one of its intents is `Sent` and that intent's agent is not closed.
    /// Resolving the intent or closing its agent frees the folder.
    pub(crate) async fn held_folders(&self) -> Result<HashSet<RepoId>, IntentError> {
        let store = &self.inner.store;
        let plain: HashSet<RepoId> = store
            .list_repos()
            .await?
            .iter()
            .filter(|repo| repo.is_plain())
            .map(|repo| repo.id)
            .collect();
        if plain.is_empty() {
            return Ok(HashSet::new());
        }
        let mut held: HashSet<RepoId> = plain
            .iter()
            .copied()
            .filter(|&repo| self.inner.agents.folder_busy(repo))
            .collect();
        let open: HashSet<AgentId> = store
            .list_agents(false)
            .await?
            .into_iter()
            .map(|agent| agent.id)
            .collect();
        let sent = store
            .list_intents(IntentFilter {
                state: Some(IntentState::Sent),
                repo_id: None,
            })
            .await?;
        held.extend(
            sent.iter()
                .filter(|i| plain.contains(&i.repo_id))
                .filter(|i| i.agent_id.is_some_and(|agent| open.contains(&agent)))
                .map(|i| i.repo_id),
        );
        Ok(held)
    }

    /// Starts the intent's agent now, from `Complete` (writing the `Send`
    /// version, as `send` does) or `Queued`. An intent already `Sent` is
    /// returned unchanged, so repeated or concurrent starts make one agent.
    /// Dependencies, the queue limit and held folders are not checked here:
    /// the scheduler and [`Self::send`] decide when to start. An intent whose
    /// plain folder another agent's turn takes before its agent's first
    /// message is returned `Queued` (a `Complete` one queued as `send`
    /// queues it), with no start error.
    pub async fn start(&self, id: IntentId) -> Result<Intent, IntentError> {
        let svc = self.clone();
        detached(async move {
            let _reservation = svc.reserve(id).await;
            let intent = svc.get(id).await?;
            match intent.state {
                IntentState::Sent => Ok(intent),
                IntentState::Complete | IntentState::Queued => svc.start_reserved(intent).await,
                _ => Err(wrong_state_or_phase(&intent, "complete or queued")),
            }
        })
        .await
    }

    /// The start itself; the caller holds the intent's reservation and has
    /// checked that it is `Complete` or `Queued`. In order: the agent (its
    /// branch and worktree), its first message (the brief), then the intent
    /// saved as `Sent`. On a failure the intent keeps its state, with the
    /// error in `last_start_error`; an agent already made is closed.
    ///
    /// A first message refused with `FolderBusy` is no failed start: another
    /// agent's turn took the plain folder after the decision to start, so the
    /// folder is held. The agent is closed, and the intent waits `Queued`
    /// with no start error for the scheduler, which that turn's end wakes.
    ///
    /// An open agent already attached to the intent (left by a start that
    /// stopped between creating it and saving `Sent`, or by a failed start
    /// whose agent could not be closed) blocks the start until it is closed,
    /// so an intent never has two agents. Closing it wakes the scheduler.
    async fn start_reserved(&self, mut intent: Intent) -> Result<Intent, IntentError> {
        #[cfg(feature = "test-util")]
        self.run_start_hook(intent.id);
        let open = self.inner.store.list_agents(false).await?;
        if let Some(attached) = open.iter().find(|a| a.intent_id == Some(intent.id)) {
            let message = format!(
                "agent {} is already attached to this intent; close it to start again",
                attached.name
            );
            tracing::warn!(intent_id = %intent.id, agent_id = %attached.id, "not starting the intent: {message}");
            self.record_start_error(&mut intent, message.clone()).await;
            return Err(IntentError::Invalid(message));
        }
        let agent_id = match self.launch_agent(&intent).await {
            Ok(id) => id,
            Err(IntentError::Service(ServiceError::FolderBusy)) => {
                tracing::debug!(intent_id = %intent.id, "another agent's turn took the folder; the intent waits");
                if intent.state == IntentState::Complete {
                    self.save_queued(&mut intent, Utc::now()).await?;
                    self.kick_scheduler();
                }
                return Ok(intent);
            }
            Err(e) => {
                tracing::warn!(intent_id = %intent.id, "could not start the intent's agent: {e}");
                self.record_start_error(&mut intent, format!("could not start the agent: {e}"))
                    .await;
                return Err(e);
            }
        };
        let mut sent = intent.clone();
        sent.state = IntentState::Sent;
        sent.agent_id = Some(agent_id);
        sent.sent_at = Some(Utc::now());
        sent.last_start_error = None;
        touch(&mut sent);
        let store = &self.inner.store;
        let saved = if intent.state == IntentState::Complete {
            // Sent straight from `Complete`: the Send version goes in with it.
            store
                .save_intent_with_version(&sent, &snapshot(&sent, VersionAuthor::Send))
                .await
                .map(|_version| ())
        } else {
            store.save_intent(&sent).await
        };
        if let Err(e) = saved {
            tracing::error!(intent_id = %intent.id, agent_id = %agent_id, "started an agent but could not save its intent: {e}");
            self.discard_agent(agent_id).await;
            self.record_start_error(
                &mut intent,
                format!("could not save the intent after starting its agent: {e}"),
            )
            .await;
            return Err(e.into());
        }
        self.publish_updated(&sent);
        Ok(sent)
    }

    /// Creates the intent's agent and sends it the brief. On a repository
    /// with source control the agent works on `intent/<slug>` (or, if that
    /// branch or its worktree folder is taken, `intent/<slug>-<6 hex of the
    /// id>`) from the intent's base branch (else the repository's default
    /// branch). On a plain repository it works in the folder itself, on no
    /// branch, and no git runs; an intent there that names a base branch or
    /// has an origin is `NeedsSourceControl`, before anything is made.
    /// An agent whose first message is refused is closed again.
    async fn launch_agent(&self, intent: &Intent) -> Result<AgentId, IntentError> {
        let repo = self
            .inner
            .store
            .get_repo(intent.repo_id)
            .await?
            .ok_or(IntentError::RepoNotFound)?;
        let in_place = repo.is_plain();
        if in_place && (intent.base_branch.is_some() || intent.origin.is_some()) {
            return Err(IntentError::NeedsSourceControl);
        }
        let base = if in_place {
            None
        } else {
            self.checked_base_branch(intent, &repo).await?
        };
        let brief = brief::compose(intent, &self.dep_summaries(intent).await?, in_place);
        let agents = &self.inner.agents;
        let options = |branch: Option<String>| CreateAgentOptions {
            branch,
            intent_id: Some(intent.id),
            system_prompt_append: Some(brief.system_append.clone()),
            base: base.clone(),
        };
        let created = if in_place {
            agents.create_agent_with(repo.id, options(None)).await
        } else {
            let branch = git::branch_name_for_intent(&intent.slug);
            match agents
                .create_agent_with(repo.id, options(Some(branch.clone())))
                .await
            {
                Err(ServiceError::Git(GitError::BranchExists(_) | GitError::WorktreeExists(_))) => {
                    let hex = intent.id.0.simple().to_string();
                    let suffixed = format!("{branch}-{}", &hex[..BRANCH_SUFFIX_HEX]);
                    agents
                        .create_agent_with(repo.id, options(Some(suffixed)))
                        .await
                }
                other => other,
            }
        };
        let agent = match created {
            Ok(agent) => agent,
            // What git said of a branch the user's repository holds is never
            // kept: the start error is a fixed sentence, the log a code.
            Err(ServiceError::Git(_)) if base.is_some() => {
                let e = IntentError::BaseBranchFailed;
                tracing::warn!(intent_id = %intent.id, code = e.code(), "the agent's worktree could not be made");
                return Err(e);
            }
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = agents.send_message(agent.id, brief.first_message).await {
            self.discard_agent(agent.id).await;
            return Err(e.into());
        }
        Ok(agent.id)
    }

    /// The intent's base branch, once it is known to be a valid branch name
    /// that exists locally; `None` when the intent has none. Checked before
    /// anything is made, and never replaced by the default branch: a name
    /// that is not valid, or a branch that is gone, is `Invalid` with a fixed
    /// sentence. Nothing is fetched, and nothing git prints is kept.
    async fn checked_base_branch(
        &self,
        intent: &Intent,
        repo: &Repo,
    ) -> Result<Option<String>, IntentError> {
        let Some(name) = &intent.base_branch else {
            return Ok(None);
        };
        let git_error = |e: GitError| IntentError::from(ServiceError::Git(e));
        if !git::is_valid_branch_name(&repo.path, name)
            .await
            .map_err(git_error)?
        {
            return Err(IntentError::Invalid(BASE_BRANCH_INVALID.into()));
        }
        if !git::branch_exists(&repo.path, name)
            .await
            .map_err(git_error)?
        {
            return Err(IntentError::Invalid(format!(
                "the base branch {name} no longer exists, so the intent cannot start from it"
            )));
        }
        Ok(Some(name.clone()))
    }

    /// Title, slug and current behavior of each accepted dependency's target.
    async fn dep_summaries(&self, intent: &Intent) -> Result<Vec<DepSummary>, StoreError> {
        let mut deps = Vec::new();
        let targets = intent
            .content
            .depends_on
            .iter()
            .filter(|d| d.status == ItemStatus::Accepted)
            .filter_map(|d| d.intent_ref);
        for target in targets {
            if let Some(t) = self.inner.store.get_intent(target).await? {
                deps.push(DepSummary {
                    slug: t.slug,
                    title: t.content.title,
                    current_behavior: t.content.current_behavior,
                });
            }
        }
        Ok(deps)
    }

    /// Closes the agent of a failed start (its worktree is kept, as for any
    /// closed agent). An agent whose turn already runs cannot be closed
    /// yet; it is closed in the background as soon as that turn ends.
    async fn discard_agent(&self, id: AgentId) {
        // Subscribed before the close, so the end of the turn cannot be missed.
        let live = self.inner.bus.subscribe(None).map(|s| s.live);
        match self.inner.agents.close_agent(id).await {
            Ok(()) => {}
            Err(ServiceError::TurnInProgress) => match live {
                Ok(live) => {
                    tracing::warn!(agent_id = %id, "the agent of a failed start is busy; closing it when its turn ends");
                    tokio::spawn(close_after_turn(self.inner.agents.clone(), id, live));
                }
                Err(e) => {
                    tracing::error!(agent_id = %id, "could not close the agent of a failed start: {e}");
                }
            },
            Err(e) => {
                tracing::error!(agent_id = %id, "could not close the agent of a failed start: {e}");
            }
        }
    }

    /// Saves `message` as the intent's `last_start_error`, leaving its state.
    /// The same error again (a queued start retried on every pass) is not
    /// written or published again.
    async fn record_start_error(&self, intent: &mut Intent, message: String) {
        if intent.last_start_error.as_deref() == Some(message.as_str()) {
            return;
        }
        intent.last_start_error = Some(message);
        touch(intent);
        match self.inner.store.save_intent(intent).await {
            Ok(()) => self.publish_updated(intent),
            Err(e) => {
                tracing::error!(intent_id = %intent.id, "could not record a start failure: {e}");
            }
        }
    }

    /// Changes what an idea intent's origin records about its package
    /// (what "Open pull request" and "Create implementation intent" made)
    /// under the intent's reservation, then saves and publishes the intent;
    /// nothing else of it changes. `Invalid` for an intent that is not an
    /// idea package.
    pub async fn update_idea(
        &self,
        id: IntentId,
        change: impl FnOnce(&mut IdeaPackage),
    ) -> Result<Intent, IntentError> {
        let _reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        let idea = intent
            .origin
            .as_mut()
            .and_then(|origin| origin.idea.as_mut())
            .ok_or_else(|| IntentError::Invalid("this intent is not an idea package".into()))?;
        change(idea);
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        Ok(intent)
    }

    /// Marks a `Sent` intent `Resolved` (whatever its agent is doing), which
    /// may let intents depending on it start.
    pub async fn resolve(&self, id: IntentId) -> Result<Intent, IntentError> {
        let svc = self.clone();
        detached(async move { svc.resolve_now(id).await }).await
    }

    /// Body of [`Self::resolve`], run detached from the caller.
    async fn resolve_now(&self, id: IntentId) -> Result<Intent, IntentError> {
        let reservation = self.reserve(id).await;
        let mut intent = self.get(id).await?;
        if intent.state != IntentState::Sent {
            return Err(wrong_state_or_phase(&intent, "sent"));
        }
        intent.state = IntentState::Resolved;
        intent.resolved_at = Some(Utc::now());
        touch(&mut intent);
        self.inner.store.save_intent(&intent).await?;
        self.publish_updated(&intent);
        drop(reservation);
        self.kick_scheduler();
        Ok(intent)
    }

    /// The stored agent defaults; a queue limit of 0 reads as none.
    pub async fn agent_defaults(&self) -> Result<AgentDefaults, IntentError> {
        let stored: Option<AgentDefaults> =
            self.inner.store.get_setting(AGENT_DEFAULTS_KEY).await?;
        let mut defaults = stored.unwrap_or_default();
        defaults.queue_limit = defaults.queue_limit.filter(|&n| n > 0);
        Ok(defaults)
    }

    /// Stores the agent defaults and wakes the scheduler. A queue limit of 0
    /// is stored as none (no limit); above [`QUEUE_LIMIT_MAX`] is `Invalid`.
    /// Returns what was stored.
    pub async fn set_agent_defaults(
        &self,
        defaults: AgentDefaults,
    ) -> Result<AgentDefaults, IntentError> {
        let svc = self.clone();
        detached(async move {
            let queue_limit = defaults.queue_limit.filter(|&n| n > 0);
            if queue_limit.is_some_and(|n| n > QUEUE_LIMIT_MAX) {
                return Err(IntentError::Invalid(format!(
                    "queue limit must be 0 to {QUEUE_LIMIT_MAX} (0 or none means no limit)"
                )));
            }
            let defaults = AgentDefaults { queue_limit };
            svc.inner
                .store
                .set_setting(AGENT_DEFAULTS_KEY, &defaults)
                .await?;
            svc.kick_scheduler();
            Ok(defaults)
        })
        .await
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
        let request = pass1_request(&intent, &root, self.polish_model());
        let raw = self.inner.harness.run_structured(request).await?.value;
        let mut polished = intent.content.clone();
        apply_pass1(&mut polished, &raw)?;

        let reservation = self.reserve(id).await;
        // Nothing else changes an intent while it polishes; re-read anyway so
        // a superseded job (e.g. failed by `recover`) cannot overwrite it.
        let Some(mut intent) = self.polishing(id, 1).await? else {
            return Ok(());
        };
        let intent_seed_kind = intent.seed.kind;
        let c = &mut intent.content;
        c.title = polished.title;
        c.kind = intent_seed_kind.unwrap_or(polished.kind);
        c.current_behavior = polished.current_behavior;
        c.questions = polished.questions;
        let start_pass2 = c.questions.is_empty();
        intent.phase = if start_pass2 {
            Phase::Polishing { pass: 2 }
        } else {
            Phase::Questions
        };
        touch(&mut intent);
        self.save_with_title_slug(&mut intent, VersionAuthor::Polish1)
            .await?;
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
        let request = pass2_request(&intent, &root, self.polish_model(), &ctx.candidates);
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
        // Every item the model adds is pending (fixed items are accepted), so
        // this is Complete only when the model added none.
        intent.state = if intent.is_complete_content() {
            IntentState::Complete
        } else {
            IntentState::Draft
        };
        touch(&mut intent);
        // The intent and its Polish2 version are stored together or not at all.
        self.inner
            .store
            .save_intent_with_version(&intent, &snapshot(&intent, VersionAuthor::Polish2))
            .await?;
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

    /// Saves the intent together with a version of its content by `author`
    /// (both stored or neither). While it has never been sent, its slug first
    /// follows the title: `slugify(title)`, then `-2`, `-3`... until unique in
    /// the repository. A concurrent writer taking the slug between the check
    /// and the save makes the save conflict, which rolls the version back too,
    /// and the slug is picked again.
    async fn save_with_title_slug(
        &self,
        intent: &mut Intent,
        author: VersionAuthor,
    ) -> Result<(), StoreError> {
        let store = &self.inner.store;
        if intent.sent_at.is_some() {
            store
                .save_intent_with_version(intent, &snapshot(intent, author))
                .await?;
            return Ok(());
        }
        let stored_slug = intent.slug.clone();
        let base = slugify(&intent.content.title);
        let mut attempt = 1;
        loop {
            intent.slug = self.free_slug(intent.repo_id, &base, &stored_slug).await?;
            match store
                .save_intent_with_version(intent, &snapshot(intent, author))
                .await
            {
                Ok(_version) => return Ok(()),
                Err(StoreError::Conflict(_)) if attempt < SLUG_SAVE_ATTEMPTS => attempt += 1,
                Err(e) => return Err(e),
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

/// An item the daemon sets (see [`CreateExtras`]): accepted, and kept as it
/// is by polish and review.
fn fixed_item(text: String, intent_ref: Option<IntentId>) -> Item {
    Item {
        id: ItemId::new(),
        text,
        status: ItemStatus::Accepted,
        edited: false,
        intent_ref,
        fixed: true,
    }
}

/// The intent's current content as a version by `author`; the store numbers it.
fn snapshot(intent: &Intent, author: VersionAuthor) -> IntentVersion {
    IntentVersion {
        intent_id: intent.id,
        version: 0,
        author,
        content: intent.content.clone(),
        at: intent.updated_at,
    }
}

/// Closes an agent whose turn is running once that turn ends. `live` must
/// have been subscribed before the refused close, so the end of the turn
/// cannot be missed; without an event it tries again every [`CLOSE_RETRY`].
async fn close_after_turn(agents: AgentService, id: AgentId, mut live: broadcast::Receiver<Event>) {
    loop {
        let turn_over = async {
            loop {
                match live.recv().await {
                    Ok(Event {
                        kind: EventKind::AgentUpdated(a),
                        ..
                    }) if a.id == id
                        && !matches!(a.state, AgentState::Working | AgentState::Quiet) =>
                    {
                        return;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return,
                    // The bus is gone: fall back to the periodic retry.
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        };
        let _ = tokio::time::timeout(CLOSE_RETRY, turn_over).await;
        match agents.close_agent(id).await {
            Ok(()) => {
                tracing::info!(agent_id = %id, "closed the agent of a failed start");
                return;
            }
            Err(ServiceError::TurnInProgress) => {}
            Err(e) => {
                tracing::error!(agent_id = %id, "could not close the agent of a failed start: {e}");
                return;
            }
        }
    }
}

/// Runs an intent operation on its own task and waits for it, so a caller
/// that stops waiting (a dropped request) cannot cut it short: between its
/// store write of `Polishing{pass}` and the start of that pass, which would
/// leave the intent polishing with no job until a restart, or between
/// creating an agent and saving the intent that owns it.
async fn detached<T, F>(op: F) -> Result<T, IntentError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, IntentError>> + Send + 'static,
{
    match tokio::spawn(op).await {
        Ok(result) => result,
        Err(e) => match e.try_into_panic() {
            // Surface a panic to the caller as if it had run inline.
            Ok(payload) => std::panic::resume_unwind(payload),
            // The handle is never aborted, so only a runtime shutdown cancels
            // the task, and that drops this caller too.
            Err(e) => panic!("intent operation cancelled: {e}"),
        },
    }
}

/// `InvalidState` for an intent outside `expected_phase`: names the phase
/// for a draft, the state otherwise (phases only matter in Draft).
fn wrong_state_or_phase(intent: &Intent, expected_phase: &'static str) -> IntentError {
    let draft = intent.state == IntentState::Draft;
    IntentError::InvalidState {
        expected: expected_phase,
        actual: intent.state,
        phase: draft.then(|| intent.phase.clone()),
    }
}

/// `Queued`, or `in the polishing phase (pass 2)` for a draft in a phase.
fn state_text(actual: &IntentState, phase: &Option<Phase>) -> String {
    match phase {
        None => format!("{actual:?}"),
        Some(Phase::Polishing { pass }) => format!("in the polishing phase (pass {pass})"),
        Some(Phase::Questions) => "in the questions phase".into(),
        Some(Phase::Review) => "in the review phase".into(),
        Some(Phase::Failed { pass, .. }) => format!("in the failed phase (pass {pass})"),
    }
}

/// `Complete` iff everything is decided, else `Draft`; other states stay.
fn recompute_state(intent: &mut Intent) {
    if matches!(intent.state, IntentState::Draft | IntentState::Complete) {
        intent.state = if intent.is_complete_content() {
            IntentState::Complete
        } else {
            IntentState::Draft
        };
    }
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
            PassFailure::Harness(HarnessError::MissingCwd(_)) => format!(
                "polish pass {pass} could not find the repository's folder; check that it is still there, then retry"
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

pub(crate) fn panic_text(payload: &(dyn Any + Send)) -> &str {
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
