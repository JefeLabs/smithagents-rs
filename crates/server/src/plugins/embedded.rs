//! Built-in plugins compiled into the daemon: a manifest embedded in the
//! binary and a runtime that never syncs. Such a plugin has no resources; it
//! declares the command-line tools the daemon's own code runs for it (see
//! [`PluginManager::run_tool`](super::PluginManager::run_tool)), and its
//! state says whether they are there.
//!
//! No directory can ask for this: discovery refuses the `in-process` host
//! mode. The daemon lists these before the plugins it discovers, so a
//! directory with the same id cannot replace one (the manager keeps the
//! first of two equal ids).

use std::sync::Arc;

use async_trait::async_trait;
use smithagents_plugin_host::{CallError, CallHandle, PluginRuntime};
use smithagents_plugin_protocol::{HostMode, Manifest, SyncPage, SyncRequest};

use super::{LoadedPlugin, Origin};

/// The feedback tool's plugin: `gh` for issues and pull requests,
/// `agent-browser` for an idea's demo video.
pub const FEEDBACK_PLUGIN_ID: &str = "feedback";

const FEEDBACK_MANIFEST: &str = include_str!("embedded/feedback.toml");

/// Every embedded plugin, built in and enabled when first seen. Today only
/// [`FEEDBACK_PLUGIN_ID`].
pub fn embedded_plugins() -> Vec<LoadedPlugin> {
    [FEEDBACK_MANIFEST]
        .into_iter()
        .filter_map(|text| match Manifest::parse(text) {
            Ok(manifest) => Some(LoadedPlugin {
                manifest,
                origin: Origin::Builtin,
                runtime: Ok(Arc::new(NothingToSync)),
            }),
            // A test parses every embedded manifest; this cannot ship.
            Err(e) => {
                tracing::error!("an embedded plugin manifest is not valid: {e}");
                None
            }
        })
        .collect()
}

/// The runtime of a plugin with no resources: the manager never calls it,
/// and a call would be a contract error.
struct NothingToSync;

#[async_trait]
impl PluginRuntime for NothingToSync {
    fn mode(&self) -> HostMode {
        HostMode::InProcess
    }

    async fn sync(&self, _call: CallHandle, _req: SyncRequest) -> Result<SyncPage, CallError> {
        Err(CallError::Contract {
            message: "this plugin has nothing to sync".to_string(),
        })
    }
}
