//! What every plugin runtime offers the daemon.

use async_trait::async_trait;
use smithagents_plugin_protocol::{ContractError, HostMode, PluginError, SyncPage, SyncRequest};

use crate::services::CallHandle;

/// Why a call into a plugin did not produce a page.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CallError {
    /// The plugin ran and reported a failure.
    #[error("{0}")]
    Plugin(PluginError),
    #[error("the plugin call timed out")]
    Timeout,
    #[error("the plugin crashed: {message}")]
    Crashed { message: String },
    #[error("the plugin exceeded a limit: {what}")]
    Limit { what: String },
    #[error("the plugin broke the contract: {message}")]
    Contract { message: String },
}

/// Why a plugin could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read the plugin: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a valid plugin: {message}")]
    Invalid { message: String },
    /// The plugin needs an interface the host does not provide, such as the
    /// filesystem or sockets.
    #[error("the plugin imports what the host does not provide: {message}")]
    MissingImport { message: String },
    #[error("cannot load the native plugin: {message}")]
    Dylib { message: String },
    #[error("refused: {message}")]
    Refused { message: String },
}

impl From<ContractError> for CallError {
    fn from(err: ContractError) -> CallError {
        CallError::Contract {
            message: err.to_string(),
        }
    }
}

#[async_trait]
pub trait PluginRuntime: Send + Sync {
    fn mode(&self) -> HostMode;

    /// Runs one sync call within the call timeout; the page is already validated.
    async fn sync(&self, call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError>;
}
