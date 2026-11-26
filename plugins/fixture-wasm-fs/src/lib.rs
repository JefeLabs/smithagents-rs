//! A guest that reads a file, so its component imports `wasi:filesystem`.
//! The host must refuse to load it. Empty on native targets.
#![cfg(target_arch = "wasm32")]

use smithagents_plugin_protocol::{HostApi, Plugin, PluginError, SyncPage, SyncRequest};

#[derive(Default)]
struct ReadsFiles;

impl Plugin for ReadsFiles {
    fn sync(&self, _host: &dyn HostApi, _req: SyncRequest) -> Result<SyncPage, PluginError> {
        let bytes = std::fs::read("x").map_err(|e| PluginError::Other {
            message: e.to_string(),
        })?;
        Err(PluginError::Other {
            message: format!("read {} bytes", bytes.len()),
        })
    }
}

smithagents_plugin_protocol::export_wasm_plugin!(ReadsFiles);
