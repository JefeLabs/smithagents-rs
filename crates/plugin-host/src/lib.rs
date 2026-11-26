//! Hosts SmithAgents plugins.
//!
//! [`HostServices`] is the one place a plugin's network access and credentials
//! pass through: every request is checked against [`HttpPolicy`], and secrets
//! named by a request's `auth` are resolved from a [`SecretSource`] and added
//! by the host, so a plugin never sees a secret value.

mod dylib;
mod http;
mod linked;
mod runtime;
mod secrets;
mod services;
mod wasm;

pub use dylib::DylibRuntime;
pub use http::HttpPolicy;
pub use linked::LinkedRuntime;
pub use runtime::{CallError, LoadError, PluginRuntime};
pub use secrets::{ConfiguredSecrets, Secret, SecretError, SecretSource, StaticSecrets};
pub use services::{CallHandle, CallPurpose, HostServices, ServicesError};
pub use wasm::{WasmEngine, WasmLimits, WasmRuntime};
