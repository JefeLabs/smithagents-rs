pub mod issue;

use std::sync::Arc;

use axum::http::StatusCode;
use smithagents_core::feedback::FeedbackSettings;
use smithagents_core::ids::RepoId;

use crate::intents::IntentService;
use crate::paths::Paths;
use crate::plugins::PluginManager;
use crate::service::AgentService;
use crate::store::{Store, StoreError};

/// Settings key of [`FeedbackSettings`].
const FEEDBACK_KEY: &str = "feedback";

#[derive(Debug, thiserror::Error)]
pub enum FeedbackError {
    #[error("repo not found")]
    RepoNotFound,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl FeedbackError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RepoNotFound => "repo_not_found",
            Self::Store(_) => "store_error",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::RepoNotFound => StatusCode::NOT_FOUND,
            Self::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
