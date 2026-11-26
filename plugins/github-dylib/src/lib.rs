//! The GitHub plugin as a native library, packed as a built-in plugin. All
//! the logic lives in `smithagents-plugin-github`; this crate only exports it.

smithagents_plugin_protocol::export_dylib_plugin!(smithagents_plugin_github::GitHub);
