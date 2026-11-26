pub mod github;
pub mod issue;
pub mod repo;
pub mod upload;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "test-util")]
use std::sync::{Mutex, PoisonError};

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use smithagents_core::feedback::{
    effective_meta_repo, FeedbackFailure, FeedbackOrigin, FeedbackOutcome, FeedbackSettings,
    FeedbackStep, FeedbackTarget, FeedbackTargets, SendAs,
};
use smithagents_core::ids::RepoId;
use smithagents_core::intent::{CreateIntentRequest, Intent, SeedInput};
use tokio::sync::Mutex as AsyncMutex;

use self::github::{tool_readiness, AGENT_BROWSER, GH, NOT_INSTALLED};
use self::issue::{file_names, github_repo, render_issue, seed_text, IssueInput};
use self::repo::{place_and_commit, FileSource, FolderFile};
use self::upload::{Staged, UploadLimits};
use crate::api::intent_error_parts;
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
    /// `step` is `create the worktree` or `commit the files`.
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
    /// The intent service refused; its own code and status are kept.
    #[error(transparent)]
    Intent(IntentError),
}

impl FeedbackError {
    pub fn code(&self) -> &'static str {
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
            Self::Intent(e) => intent_error_parts(e).1,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::RepoNotFound => StatusCode::NOT_FOUND,
            Self::DefaultBranchMissing(_)
            | Self::GitIdentityMissing
            | Self::NoFreeName
            | Self::NameTaken
            | Self::FolderBlocked(_)
            | Self::BranchBlocked(_)
            | Self::ToolMissing(_) => StatusCode::CONFLICT,
            Self::Invalid(_) | Self::UnsupportedFile(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Store(_) | Self::Git { .. } | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Intent(e) => intent_error_parts(e).0,
        }
    }
}

/// The feedback tool's daemon side: the meta repository setting, the
/// targets Submit can choose from, and submissions.
///
/// A submission is staged by the route (see [`upload::stage`]) and handed to
/// [`FeedbackService::submit`], which runs one submission at a time: a second
/// request with the same submission id waits for the first and then answers
/// with what it recorded, so one id never makes a second commit or intent.
#[derive(Clone)]
pub struct FeedbackService {
    store: Store,
    limits: UploadLimits,
    clock: Clock,
    parts: Arc<Parts>,
}

struct Parts {
    intents: IntentService,
    /// For the idea package's steps.
    #[allow(dead_code)]
    agents: AgentService,
    plugins: PluginManager,
    paths: Paths,
    /// Held for the whole of one submission.
    submitting: AsyncMutex<()>,
    /// The error the next intent step fails with.
    #[cfg(feature = "test-util")]
    fail_next_intent: Mutex<Option<IntentError>>,
}

impl FeedbackService {
    pub fn new(
        store: Store,
        intents: IntentService,
        agents: AgentService,
        plugins: PluginManager,
        paths: Paths,
    ) -> Self {
        Self {
            store,
            limits: UploadLimits::default(),
            clock: Arc::new(Utc::now),
            parts: Arc::new(Parts {
                intents,
                agents,
                plugins,
                paths,
                submitting: AsyncMutex::new(()),
                #[cfg(feature = "test-util")]
                fail_next_intent: Mutex::new(None),
            }),
        }
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
        *self
            .parts
            .fail_next_intent
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(error);
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
    /// repository, and whether `gh` and `agent-browser` are ready. Only the
    /// configured `origin` is read, never contacted, and no URL is returned.
    pub async fn targets(&self) -> Result<FeedbackTargets, FeedbackError> {
        let repos = self.store.list_repos().await?;
        let mut targets = Vec::with_capacity(repos.len());
        for repo in &repos {
            let url = match git::origin_url(&repo.path).await {
                Ok(url) => url,
                Err(e) => {
                    tracing::warn!(repo_id = %repo.id, "could not read the repository's origin: {}", git_failure(&e));
                    None
                }
            };
            targets.push(FeedbackTarget {
                repo_id: repo.id,
                has_github_remote: url.as_deref().and_then(github_repo).is_some(),
                is_public: None,
            });
        }
        let stored: Option<FeedbackSettings> = self.store.get_setting(FEEDBACK_KEY).await?;
        Ok(FeedbackTargets {
            repos: targets,
            meta_repo_id: effective_meta_repo(stored.and_then(|s| s.meta_repo_id), &repos),
            gh: tool_readiness(&self.parts.plugins, GH).await,
            agent_browser: tool_readiness(&self.parts.plugins, AGENT_BROWSER).await,
        })
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

    /// Commits the staged submission's issue folder on a new branch and, for
    /// an intent, creates the intent. A submission already recorded under
    /// this id is continued instead (its uploads are not used): only a step
    /// that has not succeeded runs again. A failure of the intent step after
    /// the commit is in the answer's `failed`, not an error.
    pub async fn submit(&self, staged: Staged) -> Result<FeedbackOutcome, FeedbackError> {
        let _turn = self.parts.submitting.lock().await;
        let id = staged.request.submission_id;
        if let Some(recorded) = self.store.get_submission(id).await? {
            drop(staged);
            tracing::info!(submission_id = %id, "continuing a recorded feedback submission");
            return self.finish(recorded).await;
        }
        let request = &staged.request;
        let repo = self
            .store
            .get_repo(request.repo_id)
            .await?
            .ok_or(FeedbackError::RepoNotFound)?;
        if request.send_as == SendAs::GithubIssue {
            return Err(FeedbackError::Invalid(
                "sending feedback as a GitHub issue is not available yet".into(),
            ));
        }
        if request.idea.is_some_and(|idea| idea.demo_video) {
            self.require_tool(AGENT_BROWSER).await?;
        }

        let created = (self.clock)();
        let files = folder_files(&staged, created)?;
        let (placement, commit) = place_and_commit(
            &repo,
            &staged.title,
            created.date_naive(),
            false,
            &files,
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
        self.store.insert_submission(&submission).await?;
        drop(staged);
        self.finish(submission).await
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
    /// for an intent, creating it.
    async fn finish(&self, mut s: Submission) -> Result<FeedbackOutcome, FeedbackError> {
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
                    s.intent_id = Some(intent.id);
                    s.updated_at = (self.clock)();
                    self.store.save_submission(&s).await?;
                    tracing::info!(submission_id = %s.id, step = "create_intent", "feedback intent created");
                    Some(intent)
                }
                Err(e) => {
                    let code = intent_error_parts(&e).1;
                    tracing::warn!(submission_id = %s.id, step = "create_intent", code, "the feedback intent was not created");
                    failed = Some(FeedbackFailure {
                        step: FeedbackStep::CreateIntent,
                        code: code.to_owned(),
                        message: clean_message(&e.to_string()),
                    });
                    None
                }
            },
            (SendAs::GithubIssue, _) => None,
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
    /// folder's whereabouts, starting from its branch.
    async fn create_intent(&self, s: &Submission) -> Result<Intent, IntentError> {
        #[cfg(feature = "test-util")]
        if let Some(e) = self
            .parts
            .fail_next_intent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
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
        let seed = seed_text(&input, &s.folder, &s.branch, &s.commit, MAX_SEED_BYTES);
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
                idea: None,
            }),
            ..CreateExtras::default()
        };
        self.parts.intents.create_with(req, extras).await
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

/// What a failed git command is called in a log: its exit status at most.
fn git_failure(e: &git::GitError) -> String {
    match e {
        git::GitError::Failed { status } => exit_status_text(status),
        git::GitError::TimedOut => "timed out".to_owned(),
        _ => "git failed".to_owned(),
    }
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
