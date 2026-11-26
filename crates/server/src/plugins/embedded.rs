//! Built-in plugins compiled into the daemon: a manifest embedded in the
//! binary and a runtime that never syncs. Such a plugin has no resources; it
//! declares the command-line tools the daemon's own code runs for it (see
//! [`PluginManager::run_tool`](super::PluginManager::run_tool)), and its
//! state says whether they are there.
//!
//! No directory can ask for this: discovery refuses the `in-process` host
//! mode, and refuses a directory in either plugin folder that uses one of
//! these ids, before anything in it is loaded. The daemon also lists these
//! before the plugins it discovers ([`embedded_first`]), and the manager
//! keeps the first of two equal ids.

use std::sync::Arc;

use async_trait::async_trait;
use smithagents_plugin_host::{CallError, CallHandle, PluginRuntime};
use smithagents_plugin_protocol::{HostMode, Manifest, SyncPage, SyncRequest};

use super::{LoadedPlugin, Origin};

/// The feedback tool's plugin: `gh` for issues and pull requests,
/// `agent-browser` for an idea's demo video.
pub const FEEDBACK_PLUGIN_ID: &str = "feedback";

/// Each embedded plugin's id and manifest text. The id is written here as
/// well, so discovery reserves it without parsing the manifest.
const EMBEDDED: [(&str, &str); 1] = [(FEEDBACK_PLUGIN_ID, include_str!("embedded/feedback.toml"))];

/// The ids of the embedded plugins: no plugin directory may use one.
pub(super) fn embedded_ids() -> impl Iterator<Item = &'static str> {
    EMBEDDED.iter().map(|(id, _)| *id)
}

/// Every embedded plugin, built in and enabled when first seen. Today only
/// [`FEEDBACK_PLUGIN_ID`].
pub fn embedded_plugins() -> Vec<LoadedPlugin> {
    EMBEDDED
        .iter()
        .filter_map(|(id, text)| match Manifest::parse(text) {
            Ok(manifest) if manifest.id.as_ref() == *id => Some(LoadedPlugin {
                manifest,
                origin: Origin::Builtin,
                runtime: Ok(Arc::new(NothingToSync)),
            }),
            // A test parses every embedded manifest; neither can ship.
            Ok(_) => {
                tracing::error!(plugin = %id, "an embedded plugin manifest has another id");
                None
            }
            Err(e) => {
                tracing::error!(plugin = %id, "an embedded plugin manifest is not valid: {e}");
                None
            }
        })
        .collect()
}

/// The plugins the daemon starts with: the embedded ones, then
/// `discovered`. Listed first, an embedded plugin keeps its id even from a
/// directory that discovery let through, since the manager keeps the first
/// of two equal ids.
pub fn embedded_first(discovered: Vec<LoadedPlugin>) -> Vec<LoadedPlugin> {
    let mut plugins = embedded_plugins();
    plugins.extend(discovered);
    plugins
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
