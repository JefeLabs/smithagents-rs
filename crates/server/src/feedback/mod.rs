pub mod github;
pub mod idea;
pub mod issue;
pub mod repo;
pub mod upload;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use smithagents_core::agent::Repo;
use smithagents_core::feedback::{
    effective_meta_repo, FeedbackFailure, FeedbackOrigin, FeedbackOutcome, FeedbackRequest,
    FeedbackSettings, FeedbackStep, FeedbackTarget, FeedbackTargets, SendAs, SubmissionId,
};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{CreateIntentRequest, Intent, SeedInput};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use self::github::{
    body_failure, create_issue, find_issue, gh_readiness, is_public, label_for, not_ready_sentence,
    tool_readiness, Gh, GithubRepo, NotCreated, AGENT_BROWSER, GH_TIMEOUT, NOT_INSTALLED,
    VISIBILITY_TIMEOUT,
};
use self::issue::{
    file_names, issue_body, marker, no_mentions, permalink_base, render_issue, seed_tail,
    seed_text, IssueInput,
};
use self::repo::{find_committed, place_and_commit, FileSource, FolderFile};
use self::upload::{Staged, StagingDir, UploadLimits};
use crate::git::{self, exit_status_text};
use crate::intents::{CreateExtras, IntentError, IntentService};
use crate::paths::{ensure_private_real_dir, Paths};
use crate::plugins::{clean_message, PluginManager};
use crate::polish::MAX_SEED_BYTES;
use crate::service::AgentService;
use crate::store::{Store, StoreError, Submission};

/// Settings key of [`FeedbackSettings`].
const FEEDBACK_KEY: &str = "feedback";
const ISSUE_MD: &str = "issue.md";
/// The most of `issue.md` read back for the issue's body (far more than a
/// maximal one; the body is cut to 60,000 characters anyway).
const ISSUE_MD_MAX_BYTES: usize = 2 * 1024 * 1024;
/// The issue's body in its scratch directory.
const BODY_FILE: &str = "body.md";
/// How long an answer of [`is_public`] is used again.
const VISIBILITY_TTL: Duration = Duration::from_secs(10 * 60);
const ORIGIN_STEP: &str = "read the repository's origin";
/// How long after `gh issue create` ran without a usable answer a search
/// that finds nothing does not lead to another create: GitHub's search can
/// take a while to find a new issue.
const UNCONFIRMED_WAIT_SECS: i64 = 60;

/// Where the service reads the time it writes into `issue.md`, the names and
/// the submission.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Every message is a fixed sentence: nothing git prints is in one.
#[derive(Debug, thiserror::Error)]
pub enum FeedbackError {
    #[error("repo not found")]
    RepoNotFound,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the repository's default branch {0} does not exist locally")]
    DefaultBranchMissing(String),
    #[error("git has no user.name and user.email set for this repository")]
    GitIdentityMissing,
    /// `step` is `create the worktree`, `commit the files` or `read the
    /// repository's origin`.
    #[error("git could not {step} ({})", exit_status_text(.status))]
    Git {
        step: &'static str,
        status: Option<i32>,
    },
    #[error("99 issue folders with this name already exist")]
    NoFreeName,
    /// The branch or folder `place` chose was created by someone else before
    /// the commit could use it.
    #[error("an issue folder or branch of this name was created meanwhile; submit again")]
    NameTaken,
    /// `docs` or `docs/issues` is a file (or anything but a folder) on the
    /// default branch.
    #[error("{0} is a file in this repository, so feedback cannot be stored there.")]
    FolderBlocked(String),
    /// A branch named like the first part of the branch to create
    /// (`feedback`, `idea`) exists, so git cannot create one under it.
    #[error(
        "a branch named {0} exists in this repository, so no branch can be created under {0}/"
    )]
    BranchBlocked(String),
    /// A file of the issue folder could not be read or written, or a name in
    /// it was not safe to use (`InvalidInput`).
    #[error("could not prepare the issue folder: {0}")]
    Io(#[from] std::io::Error),
    /// The request or the upload's shape (a part missing, unexpected or
    /// twice) is not valid.
    #[error("{0}")]
    Invalid(String),
    /// A part is not the type its attachment must be.
    #[error("{0}")]
    UnsupportedFile(String),
    /// A part, the request or the whole submission is over its limit.
    #[error("{0}")]
    TooLarge(String),
    /// A command-line tool the submission needs is not ready.
    #[error("{0}")]
    ToolMissing(String),
    /// No submission is recorded under the id.
    #[error("no such submission")]
    SubmissionNotFound,
    /// An issue submission's repository has no github.com `origin`.
    #[error("the repository's origin is not a GitHub repository")]
    NoGithubRemote,
    /// `gh` cannot make an issue: one of [`not_ready_sentence`]'s.
    #[error("{0}")]
    GhNotReady(String),
    /// The intent service refused; its own code and status are kept.
    #[error(transparent)]
    Intent(IntentError),
    /// An idea action on an intent that is no idea package.
    #[error("this intent is not an idea package")]
    NotAnIdea,
    /// An idea action before the idea intent's agent can be asked for its
    /// work: a fixed sentence.
    #[error("{0}")]
    NotReady(String),
    /// The idea agent's branch holds more than the idea package, so it is
    /// not pushed: a fixed sentence.
    #[error("{0}")]
    OutOfScope(&'static str),
    /// A step of "Open pull request" failed: the push or `gh`, with the
    /// failure's own code and fixed sentence.
    #[error("{}", .0.message)]
    Step(FeedbackFailure),
    /// A push would publish more than the feedback (the default branch has
    /// commits `origin` has not got, or nothing is known of `origin`): one
    /// of [`repo::UNPUSHED_SENTENCE`] and [`repo::NEVER_FETCHED_SENTENCE`].
    #[error("{0}")]
    Unpublished(&'static str),
    /// An idea action cannot go ahead until something changes.
    #[error("{}", .0.sentence())]
    IdeaBlocked(idea::IdeaBlocked),
    /// A GitHub issue or pull request for a repository that is (or, when
    /// `gh` cannot tell, may be) public, without the user's acknowledgement:
    /// [`PUBLIC_SENTENCE`] or [`MAYBE_PUBLIC_SENTENCE`].
    #[error("{0}")]
    PublicNotAcknowledged(&'static str),
}

/// Why a GitHub issue or pull request was refused for a public repository.
pub const PUBLIC_SENTENCE: &str =
    "This repository is public. The screenshots will be visible to anyone. Confirm that before sending.";
/// The same, for a repository `gh` could not tell the visibility of.
pub const MAYBE_PUBLIC_SENTENCE: &str = "This repository may be public. The screenshots will be visible to anyone. Confirm that before sending.";

impl FeedbackError {
    pub fn code(&self) -> &str {
        match self {
            Self::RepoNotFound => "repo_not_found",
            Self::Store(_) => "store_error",
            Self::DefaultBranchMissing(_) => "default_branch_missing",
            Self::GitIdentityMissing => "git_identity_missing",
            Self::Git { .. } => "git_failed",
            Self::NoFreeName => "no_free_name",
            Self::NameTaken => "name_taken",
            Self::FolderBlocked(_) => "folder_blocked",
            Self::BranchBlocked(_) => "branch_blocked",
            Self::Io(_) => "io_error",
            Self::Invalid(_) => "invalid",
            Self::UnsupportedFile(_) => "unsupported_file",
            Self::TooLarge(_) => "payload_too_large",
            Self::ToolMissing(_) => "tool_missing",
            Self::SubmissionNotFound => "submission_not_found",
            Self::NoGithubRemote => "no_github_remote",
            Self::GhNotReady(_) => "gh_not_ready",
            Self::Intent(e) => e.status_and_code().1,
            Self::NotAnIdea => "not_an_idea",
            Self::NotReady(_) => "invalid_state",
            Self::OutOfScope(_) => "idea_out_of_scope",
            Self::Step(failure) => &failure.code,
            Self::Unpublished(_) => repo::UNPUSHED_CODE,
            Self::IdeaBlocked(blocked) => blocked.code(),
            Self::PublicNotAcknowledged(_) => "public_not_acknowledged",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::RepoNotFound | Self::SubmissionNotFound => StatusCode::NOT_FOUND,
            Self::DefaultBranchMissing(_)
            | Self::GitIdentityMissing
            | Self::NoFreeName
            | Self::NameTaken
            | Self::FolderBlocked(_)
            | Self::BranchBlocked(_)
            | Self::ToolMissing(_)
            | Self::NoGithubRemote
            | Self::GhNotReady(_)
            | Self::NotAnIdea
            | Self::NotReady(_)
            | Self::OutOfScope(_)
            | Self::Unpublished(_)
            | Self::IdeaBlocked(_)
            | Self::PublicNotAcknowledged(_) => StatusCode::CONFLICT,
            Self::Invalid(_) | Self::UnsupportedFile(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Store(_) | Self::Git { .. } | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Intent(e) => e.status_and_code().0,
            Self::Step(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

/// The feedback tool's daemon side: the meta repository setting, the
/// targets Submit can choose from, submissions, and an idea intent's "Open
/// pull request" and "Create implementation intent" (see [`idea`]).
///
/// A submission is staged by the route (see [`upload::stage`]) and handed to
/// [`FeedbackService::submit`]. One submission id runs once at a time: a
/// second request with the same id waits for the first and then answers with
/// what it recorded, so one id never makes a second commit, push, intent or
/// issue. Commits are made one at a time; the steps after a commit (the
/// intent, or the push and the issue, which can take minutes) hold up no
/// other submission. The commit names its submission (`Feedback-Id`), so a
/// commit a stopped daemon left without its record is taken up again, not
/// made twice. [`FeedbackService::retry`] runs what did not succeed.
#[derive(Clone)]
pub struct FeedbackService {
    store: Store,
    limits: UploadLimits,
    clock: Clock,
    /// The limit of each `gh` call that makes or looks for an issue.
    gh_timeout: Duration,
    /// The limit of `gh auth status` and `gh repo view`.
    check_timeout: Duration,
    parts: Arc<Parts>,
}

struct Parts {
    intents: IntentService,
    /// Whether an idea intent's agent is still working.
    agents: AgentService,
    plugins: PluginManager,
    paths: Paths,
    /// Where `gh` runs: the daemon's home, absolute.
    gh_dir: PathBuf,
    /// Held while a submission is checked, committed and recorded.
    submitting: AsyncMutex<()>,
    /// Held by the one run of each submission id.
    ids: IdLocks<SubmissionId>,
    /// Held by the one "Open pull request" or "Create implementation
    /// intent" of each idea intent (see [`idea`]).
    ideas: IdLocks<IntentId>,
    /// Submissions in [`FeedbackService::submit`] or
    /// [`FeedbackService::retry`], and idea actions, waiting or running.
    running: AtomicUsize,
    /// Pull request addresses `gh` gave whose record could not be saved,
    /// by idea intent: the next "Open pull request" takes them from here.
    pull_requests: Mutex<HashMap<IntentId, (String, Option<String>)>>,
    /// When `gh pr create` last ran for an idea intent without a usable
    /// answer: as for issues ([`UNCONFIRMED_WAIT_SECS`]).
    unconfirmed_prs: Mutex<HashMap<IntentId, DateTime<Utc>>>,
    /// Issue addresses `gh` gave whose record could not be saved: a retry
    /// takes them from here instead of looking (GitHub's search can take a
    /// while to find a new issue) or creating another.
    issues: Mutex<HashMap<SubmissionId, String>>,
    /// [`is_public`]'s answers by [`GithubRepo::key`], with when they came.
    visibility: Mutex<HashMap<String, (bool, DateTime<Utc>)>>,
    /// When `gh issue create` last ran for a submission without a usable
    /// answer (it may have made the issue): for [`UNCONFIRMED_WAIT_SECS`]
    /// after that, a search that finds nothing creates nothing.
    unconfirmed: Mutex<HashMap<SubmissionId, DateTime<Utc>>>,
    #[cfg(feature = "test-util")]
    faults: Mutex<Faults>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One lock per id (a submission's, an idea intent's) while a run of it
/// waits or runs.
struct IdLocks<K>(Mutex<HashMap<K, Arc<AsyncMutex<()>>>>);

impl<K> Default for IdLocks<K> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<K: Copy + Eq + std::hash::Hash> IdLocks<K> {
    /// Waits until no other run of `id` holds its lock, and holds it.
    async fn lock(&self, id: K) -> IdGuard<'_, K> {
        let lock = Arc::clone(locked(&self.0).entry(id).or_default());
        // Made before waiting, so a run dropped while it waits lets go too.
        let mut guard = IdGuard {
            locks: self,
            id,
            lock: Arc::clone(&lock),
            held: None,
        };
        guard.held = Some(lock.lock_owned().await);
        guard
    }
}

struct IdGuard<'a, K: Copy + Eq + std::hash::Hash> {
    locks: &'a IdLocks<K>,
    id: K,
    lock: Arc<AsyncMutex<()>>,
    held: Option<OwnedMutexGuard<()>>,
}

impl<K: Copy + Eq + std::hash::Hash> Drop for IdGuard<'_, K> {
    fn drop(&mut self) {
        self.held.take();
        let mut map = locked(&self.locks.0);
        // Only the map's and this one's: no other run waits for it.
        if Arc::strong_count(&self.lock) == 2 {
            map.remove(&self.id);
        }
    }
}

/// What the next steps fail with (tests only).
#[cfg(feature = "test-util")]
#[derive(Default)]
struct Faults {
    intent: Option<IntentError>,
    inserts: u32,
    saves: u32,
    /// Saves of an idea intent's package that succeed before
    /// `idea_saves_failed` fail.
    idea_saves_kept: u32,
    idea_saves_failed: u32,
    /// Runs after an idea action read its agent's branch.
    after_head: Option<HeadHook>,
}

/// A test hook an idea action awaits right after reading its agent's branch.
#[cfg(feature = "test-util")]
pub type HeadHook = Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// What [`FeedbackService::precheck`] found for a new submission, for
/// [`FeedbackService::check_publication`].
struct Precheck {
    /// The github.com repository `origin` names; `None` when it names none.
    github: Option<GithubRepo>,
    /// [`FeedbackService::acknowledgement`]'s answer for a request whose
    /// files are published (an issue; a plain intent to a GitHub
    /// repository); `Ok` for any other.
    acknowledged: Result<(), FeedbackError>,
}

/// Counts a submission in [`Parts::running`] while it lives.
struct Running<'a>(&'a AtomicUsize);

impl<'a> Running<'a> {
    fn start(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Running(count)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FeedbackService {
    pub fn new(
        store: Store,
        intents: IntentService,
        agents: AgentService,
        plugins: PluginManager,
        paths: Paths,
    ) -> Self {
        // A tool runs only in an absolute directory. The home is one unless
        // no home directory was found; then it is made absolute here.
        let gh_dir = std::path::absolute(&paths.home).unwrap_or_else(|_| PathBuf::from("/"));
        Self {
            store,
            limits: UploadLimits::default(),
            clock: Arc::new(Utc::now),
            gh_timeout: GH_TIMEOUT,
            check_timeout: VISIBILITY_TIMEOUT,
            parts: Arc::new(Parts {
                intents,
                agents,
                plugins,
                paths,
                gh_dir,
                submitting: AsyncMutex::new(()),
                ids: IdLocks::default(),
                ideas: IdLocks::default(),
                running: AtomicUsize::new(0),
                pull_requests: Mutex::default(),
                unconfirmed_prs: Mutex::default(),
                issues: Mutex::default(),
                visibility: Mutex::default(),
                unconfirmed: Mutex::default(),
                #[cfg(feature = "test-util")]
                faults: Mutex::default(),
            }),
        }
    }

    /// Test hook: replaces [`GH_TIMEOUT`] (a real process ignores tokio's
    /// paused clock).
    #[cfg(feature = "test-util")]
    pub fn with_gh_timeout_for_test(mut self, timeout: Duration) -> Self {
        self.gh_timeout = timeout;
        self
    }

    /// Test hook: replaces [`VISIBILITY_TIMEOUT`], the limit of `gh auth
    /// status` and `gh repo view`.
    #[cfg(feature = "test-util")]
    pub fn with_check_timeout_for_test(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
        self
    }

    /// Test hook: replaces the upload limits (before the router is built,
    /// which reads its body limit from them).
    #[cfg(feature = "test-util")]
    pub fn with_limits(mut self, limits: UploadLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Test hook: replaces the clock.
    #[cfg(feature = "test-util")]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Test hook: the next intent step fails with `error`, once, before the
    /// intent service is asked.
    #[cfg(feature = "test-util")]
    pub fn fail_next_intent_for_test(&self, error: IntentError) {
        self.faults().intent = Some(error);
    }

    /// Test hook: the next store insert of a submission fails, once.
    #[cfg(feature = "test-util")]
    pub fn fail_next_insert_for_test(&self) {
        self.faults().inserts = 1;
    }

    /// Test hook: the next `n` store saves of a submission fail.
    #[cfg(feature = "test-util")]
    pub fn fail_next_saves_for_test(&self, n: u32) {
        self.faults().saves = n;
    }

    /// Test hook: of the next saves of an idea intent's package, `kept`
    /// succeed, then `failed` fail.
    #[cfg(feature = "test-util")]
    pub fn fail_idea_saves_for_test(&self, kept: u32, failed: u32) {
        let mut faults = self.faults();
        faults.idea_saves_kept = kept;
        faults.idea_saves_failed = failed;
    }

    /// Test hook: every idea action awaits `hook` right after it read its
    /// agent's branch, before it checks the agent's turn again.
    #[cfg(feature = "test-util")]
    pub fn after_reading_the_head_for_test(&self, hook: HeadHook) {
        self.faults().after_head = Some(hook);
    }

    #[cfg(feature = "test-util")]
    async fn after_reading_the_head(&self) {
        let hook = self.faults().after_head.clone();
        if let Some(hook) = hook {
            hook().await;
        }
    }

    #[cfg(feature = "test-util")]
    fn faults(&self) -> MutexGuard<'_, Faults> {
        locked(&self.parts.faults)
    }

    /// Whether this save of an idea intent's package fails (tests only).
    #[cfg(feature = "test-util")]
    fn idea_save_fails(&self) -> bool {
        let mut faults = self.faults();
        if faults.idea_saves_kept > 0 {
            faults.idea_saves_kept -= 1;
            false
        } else if faults.idea_saves_failed > 0 {
            faults.idea_saves_failed -= 1;
            true
        } else {
            false
        }
    }

    /// Submissions and idea actions being made now (waiting for their turn
    /// or running): the daemon is not idle while there is one.
    pub fn running_submissions(&self) -> usize {
        self.parts.running.load(Ordering::SeqCst)
    }

    pub fn limits(&self) -> UploadLimits {
        self.limits
    }

    /// Where submissions are staged: each in a directory of its own directly
    /// under this one.
    pub fn staging_root(&self) -> PathBuf {
        self.parts.paths.feedback_tmp_dir()
    }

    /// The stored setting; an id whose repository is gone reads as `None`.
    pub async fn settings(&self) -> Result<FeedbackSettings, FeedbackError> {
        let stored: Option<FeedbackSettings> = self.store.get_setting(FEEDBACK_KEY).await?;
        let mut settings = stored.unwrap_or_default();
        if let Some(id) = settings.meta_repo_id {
            if self.store.get_repo(id).await?.is_none() {
                settings.meta_repo_id = None;
            }
        }
        Ok(settings)
    }

    /// Stores the setting. A repository id that does not exist is refused and
    /// the stored value stays. Returns what was stored.
    pub async fn set_settings(
        &self,
        settings: FeedbackSettings,
    ) -> Result<FeedbackSettings, FeedbackError> {
        if let Some(id) = settings.meta_repo_id {
            self.require_repo(id).await?;
        }
        self.store.set_setting(FEEDBACK_KEY, &settings).await?;
        Ok(settings)
    }

    async fn require_repo(&self, id: RepoId) -> Result<(), FeedbackError> {
        match self.store.get_repo(id).await? {
            Some(_) => Ok(()),
            None => Err(FeedbackError::RepoNotFound),
        }
    }

    /// One target per repository (in the store's order), the meta
    /// repository, and whether `gh` (signed in) and `agent-browser` are
    /// ready. The configured `origin` is only read, and no URL is returned.
    /// While `gh` is ready, it is asked whether each repository with a
    /// GitHub remote is public, all at once; an answer is used again for 10
    /// minutes.
    pub async fn targets(&self) -> Result<FeedbackTargets, FeedbackError> {
        let repos = self.store.list_repos().await?;
        let gh = self.gh();
        let ready = gh_readiness(&gh).await;
        let targets =
            futures::future::join_all(repos.iter().map(|repo| self.target(repo, &gh, ready.ready)))
                .await;
        let stored: Option<FeedbackSettings> = self.store.get_setting(FEEDBACK_KEY).await?;
        Ok(FeedbackTargets {
            repos: targets,
            meta_repo_id: effective_meta_repo(stored.and_then(|s| s.meta_repo_id), &repos),
            gh: ready,
            agent_browser: tool_readiness(&self.parts.plugins, AGENT_BROWSER).await,
        })
    }

    async fn target(&self, repo: &Repo, gh: &Gh<'_>, gh_ready: bool) -> FeedbackTarget {
        let github = match self.github_of(repo).await {
            Ok(github) => github,
            Err(e) => {
                tracing::warn!(repo_id = %repo.id, code = e.code(), "{e}");
                None
            }
        };
        let is_public = match &github {
            Some(github) if gh_ready => self.visibility(gh, github).await,
            _ => None,
        };
        FeedbackTarget {
            repo_id: repo.id,
            has_github_remote: github.is_some(),
            is_public,
        }
    }

    /// [`is_public`], or its answer of the last 10 minutes; a failure is not
    /// kept, so the next call asks again.
    async fn visibility(&self, gh: &Gh<'_>, github: &GithubRepo) -> Option<bool> {
        let key = github.key();
        let now = (self.clock)();
        let fresh = |at: DateTime<Utc>| (now - at).to_std().is_ok_and(|age| age < VISIBILITY_TTL);
        if let Some(&(public, at)) = locked(&self.parts.visibility).get(&key) {
            if fresh(at) {
                return Some(public);
            }
        }
        let public = is_public(gh, github).await?;
        locked(&self.parts.visibility).insert(key, (public, now));
        Some(public)
    }

    /// How `gh` runs for this service.
    fn gh(&self) -> Gh<'_> {
        Gh {
            plugins: &self.parts.plugins,
            cwd: &self.parts.gh_dir,
            timeout: self.gh_timeout,
            check_timeout: self.check_timeout,
        }
    }

    /// The github.com repository `origin` names, as configured; `None` when
    /// there is no `origin` or it is elsewhere.
    async fn github_of(&self, repo: &Repo) -> Result<Option<GithubRepo>, FeedbackError> {
        let url = git::origin_url(&repo.path).await.map_err(|e| {
            let status = match e {
                git::GitError::Failed { status } => status,
                _ => None,
            };
            FeedbackError::Git {
                step: ORIGIN_STEP,
                status,
            }
        })?;
        Ok(url.as_deref().and_then(GithubRepo::from_remote))
    }

    /// Removes everything under [`Paths::feedback_tmp_dir`]: what a run that
    /// crashed left. Called at daemon start, before any submission. A link
    /// in its place is never followed.
    pub async fn remove_leftovers(&self) {
        let root = self.staging_root();
        if let Err(e) = ensure_private_real_dir(&root) {
            tracing::warn!("the feedback temp directory cannot be used: {e}");
            return;
        }
        let mut entries = match tokio::fs::read_dir(&root).await {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("could not read the feedback temp directory: {e}");
                return;
            }
        };
        let mut removed = 0u32;
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("could not read the feedback temp directory: {e}");
                    break;
                }
            };
            match remove_entry(&entry.path()).await {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("could not remove a feedback leftover: {e}"),
            }
        }
        if removed > 0 {
            tracing::info!(
                count = removed,
                "removed feedback leftovers of an earlier run"
            );
        }
    }

    /// Commits the staged submission's issue folder on a new branch
    /// (`idea/<slug>` for an idea package), then creates the intent, or
    /// pushes the branch and creates the GitHub issue. A submission already
    /// recorded under this id is continued instead (its uploads are not
    /// used): only a step that has not succeeded runs again. So is one whose
    /// commit is on a branch without its record (the daemon stopped in
    /// between): that commit is recorded, not made again. A record that
    /// cannot be stored takes its new branch back. Before anything is
    /// written or taken up, a new submission is checked for what it
    /// publishes ([`Self::check_publication`]): an issue is refused when
    /// `origin` is not on github.com or `gh` is not ready. A failure of a
    /// step after the commit is in the answer's `failed`, not an error, and
    /// undoes nothing.
    pub async fn submit(&self, staged: Staged) -> Result<FeedbackOutcome, FeedbackError> {
        let _running = Running::start(&self.parts.running);
        let id = staged.request.submission_id;
        let _own = self.parts.ids.lock(id).await;
        // Before the turn, since `gh` can take seconds. A recorded
        // submission is continued, never refused.
        let recorded = self.store.get_submission(id).await?.is_some();
        let precheck = if recorded {
            None
        } else {
            Some(self.precheck(&staged.request).await?)
        };
        let gh_checked = !recorded && staged.request.send_as == SendAs::GithubIssue;
        let submission = {
            let _turn = self.parts.submitting.lock().await;
            self.record(staged, precheck).await?
        };
        self.finish(submission, gh_checked).await
    }

    /// Runs the steps of the recorded submission `id` that have not
    /// succeeded: the intent, or the push and then the issue. A complete
    /// submission is answered as it is; nothing is committed again.
    pub async fn retry(&self, id: SubmissionId) -> Result<FeedbackOutcome, FeedbackError> {
        let _running = Running::start(&self.parts.running);
        let _own = self.parts.ids.lock(id).await;
        let recorded = self
            .store
            .get_submission(id)
            .await?
            .ok_or(FeedbackError::SubmissionNotFound)?;
        tracing::info!(submission_id = %id, "retrying a feedback submission");
        self.finish(recorded, false).await
    }

    /// The submission's record: the stored one, the one of a commit made
    /// before, or a new commit's. `precheck`: what [`Self::precheck`] found
    /// for a submission not recorded then; a commit is made or taken up only
    /// once [`Self::check_publication`] lets it.
    async fn record(
        &self,
        staged: Staged,
        precheck: Option<Precheck>,
    ) -> Result<Submission, FeedbackError> {
        let id = staged.request.submission_id;
        if let Some(recorded) = self.store.get_submission(id).await? {
            drop(staged);
            tracing::info!(submission_id = %id, "continuing a recorded feedback submission");
            return Ok(recorded);
        }
        let request = &staged.request;
        let repo = self
            .store
            .get_repo(request.repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)?;
        let precheck = match precheck {
            Some(precheck) => precheck,
            None => self.precheck(request).await?,
        };
        // Before a commit is taken up as well: a request re-sent after a
        // stop meets the same checks as a new one, and a refusal leaves the
        // commit as it was, unrecorded.
        self.check_publication(request, &repo, precheck)
            .await
            .inspect_err(|e| {
                tracing::warn!(submission_id = %id, step = "commit", code = e.code(), "feedback was not committed");
            })?;
        // A run that stopped after its commit and before its record.
        if let Some(found) = find_committed(&repo, id).await? {
            tracing::info!(submission_id = %id, step = "commit", "taking up a feedback commit made before");
            let now = (self.clock)();
            let submission = Submission {
                id,
                repo_id: repo.id,
                request: staged.request.clone(),
                title: staged.title.clone(),
                branch: found.branch,
                commit: found.commit,
                folder: found.folder,
                pushed: false,
                intent_id: None,
                issue_url: None,
                created_at: now,
                updated_at: now,
            };
            drop(staged);
            // Nothing to undo when it fails: the commit stays findable.
            self.insert_record(&submission).await?;
            return Ok(submission);
        }
        if request.idea.is_some_and(|idea| idea.demo_video) {
            self.require_tool(AGENT_BROWSER).await?;
        }

        let created = (self.clock)();
        let files = folder_files(&staged, created)?;
        // An idea package goes on `idea/<slug>`.
        let (placement, commit) = place_and_commit(
            &repo,
            &staged.title,
            created.date_naive(),
            request.idea.is_some(),
            &files,
            id,
            staged.dir.path(),
        )
        .await
        .inspect_err(|e| {
            tracing::warn!(submission_id = %id, step = "commit", code = e.code(), "feedback was not committed");
        })?;
        tracing::info!(submission_id = %id, step = "commit", "feedback committed");
        let submission = Submission {
            id,
            repo_id: repo.id,
            request: staged.request.clone(),
            title: staged.title.clone(),
            branch: placement.branch,
            commit,
            folder: placement.folder,
            pushed: false,
            intent_id: None,
            issue_url: None,
            created_at: created,
            updated_at: created,
        };
        drop(staged);
        if let Err(e) = self.insert_record(&submission).await {
            // No branch without its record: take back the one just made,
            // unless it has moved on since.
            if git::delete_branch_at(&repo.path, &submission.branch, &submission.commit)
                .await
                .is_err()
            {
                tracing::error!(submission_id = %id, step = "record", code = "branch_kept", "a feedback branch without its record was left in place");
            }
            return Err(e);
        }
        Ok(submission)
    }

    /// What a new submission's request needs to know before anything is
    /// committed, asked before its turn (`gh` can take seconds): for an
    /// issue, [`Self::require_github`] (refused at once) and the
    /// acknowledgement; for an intent, whether `origin` is a GitHub
    /// repository and, for a plain intent (whose agent opens a pull request
    /// there), the acknowledgement. An idea package is not asked: nothing
    /// of it is published before "Open pull request", which asks itself.
    async fn precheck(&self, request: &FeedbackRequest) -> Result<Precheck, FeedbackError> {
        let acknowledged = request.public_acknowledged;
        if request.send_as == SendAs::GithubIssue {
            let github = self.require_github(request.repo_id).await?;
            let answer = self.acknowledgement(&github, acknowledged).await;
            return Ok(Precheck {
                github: Some(github),
                acknowledged: answer,
            });
        }
        let repo = self
            .store
            .get_repo(request.repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)?;
        let github = self.github_of(&repo).await?;
        let answer = match &github {
            Some(github) if request.idea.is_none() => {
                self.acknowledgement(github, acknowledged).await
            }
            _ => Ok(()),
        };
        Ok(Precheck {
            github,
            acknowledged: answer,
        })
    }

    /// Whether a commit may be made (or taken up) for `request`, by what it
    /// publishes, now or through an agent later:
    ///
    /// - an issue (pushed now): nothing it is built on may publish the
    ///   user's unpushed work ([`repo::require_published_default_branch`];
    ///   checked again before every push), and the acknowledgement;
    /// - a plain intent to a GitHub repository (its agent's pull request
    ///   carries the folder): the acknowledgement;
    /// - an idea package to a GitHub repository: the default branch must be
    ///   on `origin` once this repository has fetched from it
    ///   ([`repo::require_published_default_branch_once_fetched`]), so the
    ///   agent's work is not wasted on a push that must be refused.
    async fn check_publication(
        &self,
        request: &FeedbackRequest,
        repo: &Repo,
        precheck: Precheck,
    ) -> Result<(), FeedbackError> {
        match (request.send_as, request.idea) {
            (SendAs::GithubIssue, _) => {
                repo::require_published_default_branch(repo).await?;
                precheck.acknowledged
            }
            (SendAs::Intent, Some(_)) if precheck.github.is_some() => {
                repo::require_published_default_branch_once_fetched(repo).await
            }
            (SendAs::Intent, Some(_)) => Ok(()),
            (SendAs::Intent, None) => precheck.acknowledged,
        }
    }

    /// The github.com repository `origin` names; `NoGithubRemote` when it is
    /// none, then `GhNotReady` unless `gh` is ready and signed in.
    async fn require_github(&self, repo_id: RepoId) -> Result<GithubRepo, FeedbackError> {
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)?;
        let github = self
            .github_of(&repo)
            .await?
            .ok_or(FeedbackError::NoGithubRemote)?;
        match gh_readiness(&self.gh()).await.reason {
            None => Ok(github),
            Some(reason) => Err(FeedbackError::GhNotReady(not_ready_sentence(&reason))),
        }
    }

    /// The one rule for sending the screenshots to GitHub (an issue, a plain
    /// intent whose agent opens a pull request, an idea's pull request, an
    /// idea's implementation intent): `Ok` when the user acknowledged that
    /// they may be public (then `gh` is not asked at all), or `github` is
    /// private ([`Self::visibility`]: `gh repo view`, or its answer of the
    /// last 10 minutes); else `PublicNotAcknowledged`, with
    /// [`PUBLIC_SENTENCE`] for a public repository and
    /// [`MAYBE_PUBLIC_SENTENCE`] when `gh` cannot tell (not installed, not
    /// signed in, no answer).
    async fn acknowledgement(
        &self,
        github: &GithubRepo,
        acknowledged: bool,
    ) -> Result<(), FeedbackError> {
        if acknowledged {
            return Ok(());
        }
        match self.visibility(&self.gh(), github).await {
            Some(false) => Ok(()),
            Some(true) => Err(FeedbackError::PublicNotAcknowledged(PUBLIC_SENTENCE)),
            None => Err(FeedbackError::PublicNotAcknowledged(MAYBE_PUBLIC_SENTENCE)),
        }
    }

    /// Stores a new submission; a failure is logged with its step and code.
    async fn insert_record(&self, s: &Submission) -> Result<(), FeedbackError> {
        #[cfg(feature = "test-util")]
        if std::mem::take(&mut self.faults().inserts) > 0 {
            tracing::warn!(submission_id = %s.id, step = "record", code = "store_error", "the feedback submission was not recorded");
            return Err(StoreError::Conflict("a test failed this insert".into()).into());
        }
        self.store.insert_submission(s).await.map_err(|e| {
            let e = FeedbackError::from(e);
            tracing::warn!(submission_id = %s.id, step = "record", code = e.code(), "the feedback submission was not recorded");
            e
        })
    }

    /// Saves what a submission got to; a failure is logged with `step` and
    /// its code.
    async fn save_record(&self, s: &Submission, step: &'static str) -> Result<(), FeedbackError> {
        #[cfg(feature = "test-util")]
        {
            let mut faults = self.faults();
            if faults.saves > 0 {
                faults.saves -= 1;
                tracing::warn!(submission_id = %s.id, step, code = "store_error", "the feedback submission was not saved");
                return Err(StoreError::Conflict("a test failed this save".into()).into());
            }
        }
        self.store.save_submission(s).await.map_err(|e| {
            let e = FeedbackError::from(e);
            tracing::warn!(submission_id = %s.id, step, code = e.code(), "the feedback submission was not saved");
            e
        })
    }

    /// [`Self::save_record`], tried twice: for a result that exists either
    /// way, so not saving it must not turn into a failure. Whether it was
    /// saved.
    async fn save_result(&self, s: &Submission, step: &'static str) -> bool {
        self.save_record(s, step).await.is_ok() || self.save_record(s, step).await.is_ok()
    }

    /// `ToolMissing` unless the feedback plugin's `tool` is ready.
    async fn require_tool(&self, tool: &str) -> Result<(), FeedbackError> {
        match tool_readiness(&self.parts.plugins, tool).await.reason {
            None => Ok(()),
            Some(reason) if reason == NOT_INSTALLED => Err(FeedbackError::ToolMissing(format!(
                "{tool} is not installed"
            ))),
            Some(reason) => Err(FeedbackError::ToolMissing(format!(
                "{tool} is not ready: {reason}"
            ))),
        }
    }

    /// The steps after the commit, each skipped when its result is stored:
    /// for an intent, creating it; for an issue, the push and then the
    /// issue. `gh_checked`: `gh` was found ready a moment ago.
    async fn finish(
        &self,
        mut s: Submission,
        gh_checked: bool,
    ) -> Result<FeedbackOutcome, FeedbackError> {
        let mut failed = None;
        let intent = match (s.request.send_as, s.intent_id) {
            (SendAs::Intent, Some(intent_id)) => Some(
                self.parts
                    .intents
                    .get(intent_id)
                    .await
                    .map_err(FeedbackError::Intent)?,
            ),
            (SendAs::Intent, None) => match self.create_intent(&s).await {
                Ok(intent) => {
                    tracing::info!(submission_id = %s.id, step = "create_intent", "feedback intent created");
                    s.intent_id = Some(intent.id);
                    s.updated_at = (self.clock)();
                    // The intent exists either way, so it is the answer (a
                    // 500 now would invite a second one).
                    if !self.save_result(&s, "record_intent").await {
                        tracing::error!(submission_id = %s.id, step = "record_intent", code = "store_error", "the feedback intent was made but not recorded");
                    }
                    Some(intent)
                }
                Err(e) => {
                    let code = e.status_and_code().1;
                    tracing::warn!(submission_id = %s.id, step = "create_intent", code, "the feedback intent was not created");
                    failed = Some(FeedbackFailure {
                        step: FeedbackStep::CreateIntent,
                        code: code.to_owned(),
                        message: clean_message(&e.to_string()),
                    });
                    None
                }
            },
            (SendAs::GithubIssue, _) => {
                failed = self.send_issue(&mut s, gh_checked).await?;
                None
            }
        };
        Ok(FeedbackOutcome {
            submission_id: s.id,
            repo_id: s.repo_id,
            branch: s.branch,
            commit: s.commit,
            folder: s.folder,
            pushed: s.pushed,
            intent,
            issue_url: s.issue_url,
            failed,
        })
    }

    /// An intent of the submission's kind, seeded from its text and the
    /// folder's whereabouts, starting from its branch. For an idea package,
    /// an idea intent: its origin records the parts asked for, and its
    /// criteria are fixed ([`idea::fixed_criteria`]).
    async fn create_intent(&self, s: &Submission) -> Result<Intent, IntentError> {
        #[cfg(feature = "test-util")]
        if let Some(e) = self.faults().intent.take() {
            return Err(e);
        }
        let request = &s.request;
        let input = IssueInput {
            title: &s.title,
            main_idea: &request.main_idea,
            kind: request.kind,
            created: s.created_at,
            app_version: &request.app_version,
            platform: &request.platform,
            attachments: &request.attachments,
            // The seed names no file.
            names: &[],
        };
        // The intent service refuses a longer seed; the tail is not counted.
        let tail = seed_tail(&s.folder, &s.branch, &s.commit).len();
        let budget = MAX_SEED_BYTES.saturating_sub(tail);
        let seed = seed_text(&input, &s.folder, &s.branch, &s.commit, budget);
        let req = CreateIntentRequest {
            repo_id: Some(s.repo_id),
            kind: Some(request.kind.intent_kind()),
            seed: SeedInput::Text(seed),
        };
        let extras = CreateExtras {
            base_branch: Some(s.branch.clone()),
            origin: Some(FeedbackOrigin {
                folder: s.folder.clone(),
                branch: s.branch.clone(),
                commit: s.commit.clone(),
                idea: request.idea.map(idea::package),
            }),
            fixed_criteria: request.idea.map(idea::fixed_criteria).unwrap_or_default(),
            fixed_dependency: None,
        };
        self.parts.intents.create_with(req, extras).await
    }

    /// For an issue: `git push origin <branch>:<branch>`, then the issue,
    /// each skipped when its result is stored. The first that fails is the
    /// answer's `failed`; nothing is undone. The issue's address is kept in
    /// memory until its record is saved, so a retry never makes a second one
    /// while the daemon runs (after a restart, `gh`'s search finds it).
    async fn send_issue(
        &self,
        s: &mut Submission,
        gh_checked: bool,
    ) -> Result<Option<FeedbackFailure>, FeedbackError> {
        let id = s.id;
        if s.issue_url.is_none() {
            let remembered = locked(&self.parts.issues).get(&id).cloned();
            if let Some(url) = remembered {
                // An issue exists only once its branch was pushed.
                s.pushed = true;
                s.issue_url = Some(url);
                s.updated_at = (self.clock)();
                if self.save_result(s, "record_issue").await {
                    locked(&self.parts.issues).remove(&id);
                }
            }
        }
        if s.issue_url.is_some() {
            return Ok(None);
        }
        let repo = self
            .store
            .get_repo(s.repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)?;
        let step = if s.pushed {
            FeedbackStep::CreateIssue
        } else {
            FeedbackStep::Push
        };
        let github = match self.github_of(&repo).await {
            Ok(Some(github)) => github,
            Ok(None) => return Ok(Some(step_failure(step, &FeedbackError::NoGithubRemote))),
            Err(e) => return Ok(Some(step_failure(step, &e))),
        };

        if !s.pushed {
            // The recorded commit itself, whatever the branch is now; never
            // when what it is built on would publish the user's unpushed
            // work.
            if let Err(e) = repo::push_commit(&repo, &s.commit, &s.commit, &s.branch).await {
                let failure = match e {
                    FeedbackError::Step(failure) => failure,
                    e => step_failure(FeedbackStep::Push, &e),
                };
                tracing::warn!(submission_id = %id, step = "push", code = %failure.code, "the feedback branch was not pushed");
                return Ok(Some(failure));
            }
            tracing::info!(submission_id = %id, step = "push", "feedback branch pushed");
            s.pushed = true;
            s.updated_at = (self.clock)();
            // Not saved, the next retry pushes the same commit again, which
            // changes nothing.
            let _ = self.save_record(s, "record_push").await;
        }

        match self.make_issue(s, &repo, &github, gh_checked).await {
            Ok(url) => {
                tracing::info!(submission_id = %id, step = "create_issue", "feedback issue created or found");
                locked(&self.parts.issues).insert(id, url.clone());
                s.issue_url = Some(url);
                s.updated_at = (self.clock)();
                if self.save_result(s, "record_issue").await {
                    locked(&self.parts.issues).remove(&id);
                } else {
                    tracing::error!(submission_id = %id, step = "record_issue", code = "store_error", "the feedback issue was made but not recorded");
                }
                Ok(None)
            }
            Err(failure) => {
                tracing::warn!(submission_id = %id, step = "create_issue", code = %failure.code, "the feedback issue was not created");
                Ok(Some(failure))
            }
        }
    }

    /// The issue of a pushed submission: `gh` checked (unless it was a
    /// moment ago); one the signed-in user made before ([`find_issue`]);
    /// else, unless a create ran without a usable answer less than a minute
    /// ago (`issue_unconfirmed`), a new one ([`create_issue`]) from a body
    /// written to a scratch directory of its own (removed afterwards).
    /// GitHub gets the title and the body mentioning no one
    /// ([`no_mentions`]).
    async fn make_issue(
        &self,
        s: &Submission,
        repo: &Repo,
        github: &GithubRepo,
        gh_checked: bool,
    ) -> Result<String, FeedbackFailure> {
        let gh = self.gh();
        if !gh_checked {
            if let Some(reason) = gh_readiness(&gh).await.reason {
                return Err(FeedbackFailure {
                    step: FeedbackStep::CreateIssue,
                    code: "gh_not_ready".to_owned(),
                    message: not_ready_sentence(&reason),
                });
            }
        }
        if let Some(url) = find_issue(&gh, github, s.id).await? {
            locked(&self.parts.unconfirmed).remove(&s.id);
            return Ok(url);
        }
        let last = locked(&self.parts.unconfirmed).get(&s.id).copied();
        if let Some(at) = last {
            // A clock that went back counts as no time passed.
            if (self.clock)() - at < chrono::TimeDelta::seconds(UNCONFIRMED_WAIT_SECS) {
                return Err(FeedbackFailure {
                    step: FeedbackStep::CreateIssue,
                    code: "issue_unconfirmed".to_owned(),
                    message: "GitHub has not confirmed whether the issue was created. Try again in a minute.".to_owned(),
                });
            }
        }
        let body = issue_text(s, repo, github).await?;
        let scratch = StagingDir::scratch(&self.staging_root(), s.id).map_err(|e| {
            tracing::warn!(submission_id = %s.id, step = "create_issue", kind = ?e.kind(), "could not write the issue's body");
            body_failure()
        })?;
        let body_file = scratch.path().join(BODY_FILE);
        tokio::fs::write(&body_file, body).await.map_err(|e| {
            tracing::warn!(submission_id = %s.id, step = "create_issue", kind = ?e.kind(), "could not write the issue's body");
            body_failure()
        })?;
        let title = no_mentions(&s.title);
        let label = label_for(s.request.kind);
        match create_issue(&gh, github, &title, &body_file, label).await {
            Ok(url) => {
                locked(&self.parts.unconfirmed).remove(&s.id);
                Ok(url)
            }
            Err(NotCreated {
                failure,
                unconfirmed,
            }) => {
                if unconfirmed {
                    locked(&self.parts.unconfirmed).insert(s.id, (self.clock)());
                }
                Err(failure)
            }
        }
    }
}

/// The body of a submission's issue: its `issue.md` as committed, without
/// front matter, links to the folder's files as permalinks to the commit,
/// mentioning no one ([`no_mentions`]: `issue.md` itself keeps the user's
/// text), and the submission's marker last.
async fn issue_text(
    s: &Submission,
    repo: &Repo,
    github: &GithubRepo,
) -> Result<String, FeedbackFailure> {
    let path = format!("{}/{ISSUE_MD}", s.folder);
    let issue_md = git::show_file(&repo.path, &s.commit, &path, ISSUE_MD_MAX_BYTES).await;
    let files = git::list_folder(&repo.path, &s.commit, &s.folder).await;
    match (issue_md, files) {
        (Ok(Some(issue_md)), Ok(files)) => {
            let base = permalink_base(&github.owner, &github.name, &s.commit, &s.folder);
            // Before the body is cut to its length, so what is added counts.
            let text = no_mentions(&issue_md);
            Ok(issue_body(&text, &files, &base, Some(&marker(s.id))))
        }
        _ => {
            tracing::warn!(submission_id = %s.id, step = "create_issue", code = "git_failed", "could not read the committed issue folder");
            Err(FeedbackFailure {
                step: FeedbackStep::CreateIssue,
                code: "git_failed".to_owned(),
                message: "git could not read the committed issue folder".to_owned(),
            })
        }
    }
}

/// A refusal at `step` after the commit, as a failure.
fn step_failure(step: FeedbackStep, e: &FeedbackError) -> FeedbackFailure {
    FeedbackFailure {
        step,
        code: e.code().to_owned(),
        message: e.to_string(),
    }
}

/// `issue.md`, then each attachment's files under the daemon's names.
fn folder_files(staged: &Staged, created: DateTime<Utc>) -> Result<Vec<FolderFile>, FeedbackError> {
    let request = &staged.request;
    let kinds: Vec<_> = request
        .attachments
        .iter()
        .enumerate()
        .map(|(at, a)| {
            let file = staged
                .files
                .iter()
                .find(|f| f.attachment == at + 1 && !f.annotated)
                .ok_or_else(|| missing_upload(at + 1))?;
            Ok((a.kind, file.kind))
        })
        .collect::<Result<_, FeedbackError>>()?;
    let names = file_names(&kinds);
    let issue_md = render_issue(&IssueInput {
        title: &staged.title,
        main_idea: &request.main_idea,
        kind: request.kind,
        created,
        app_version: &request.app_version,
        platform: &request.platform,
        attachments: &request.attachments,
        names: &names,
    });
    let mut files = vec![FolderFile {
        name: ISSUE_MD.to_owned(),
        source: FileSource::Text(issue_md),
    }];
    for file in &staged.files {
        let names = names
            .get(file.attachment - 1)
            .ok_or_else(|| missing_upload(file.attachment))?;
        let name = if file.annotated {
            names
                .annotated
                .clone()
                .ok_or_else(|| missing_upload(file.attachment))?
        } else {
            names.file.clone()
        };
        files.push(FolderFile {
            name,
            source: FileSource::Staged(file.path.clone()),
        });
    }
    Ok(files)
}

/// A staged submission that does not match its request (a bug: `stage`
/// checks every part).
fn missing_upload(attachment: usize) -> FeedbackError {
    FeedbackError::Invalid(format!("attachment {attachment} was not sent"))
}

/// Removes a directory with everything in it, or a file or link (never
/// followed).
async fn remove_entry(path: &Path) -> io::Result<()> {
    let meta = tokio::fs::symlink_metadata(path).await?;
    if meta.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    }
}
