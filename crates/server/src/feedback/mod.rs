pub mod issue;
pub mod repo;

use std::sync::Arc;

use axum::http::StatusCode;
use smithagents_core::feedback::FeedbackSettings;
use smithagents_core::ids::RepoId;

use crate::git::exit_status_text;
use crate::intents::IntentService;
use crate::paths::Paths;
use crate::plugins::PluginManager;
use crate::service::AgentService;
use crate::store::{Store, StoreError};

/// Settings key of [`FeedbackSettings`].
const FEEDBACK_KEY: &str = "feedback";

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
            | Self::BranchBlocked(_) => StatusCode::CONFLICT,
            Self::Store(_) | Self::Git { .. } | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The feedback tool's daemon side. Later tasks add submission to it.
#[derive(Clone)]
pub struct FeedbackService {
    store: Store,
    // Used by the submission steps added in later tasks.
    #[allow(dead_code)]
    parts: Arc<Parts>,
}

#[allow(dead_code)]
struct Parts {
    intents: IntentService,
    agents: AgentService,
    plugins: PluginManager,
    paths: Paths,
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
            parts: Arc::new(Parts {
                intents,
                agents,
                plugins,
                paths,
            }),
        }
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
}
