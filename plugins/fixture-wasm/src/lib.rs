//! The fixture plugin as a WebAssembly component (`wasm32-wasip2`). Empty on
//! native targets.
#![cfg(target_arch = "wasm32")]

smithagents_plugin_protocol::export_wasm_plugin!(smithagents_plugin_fixture::Fixture);
