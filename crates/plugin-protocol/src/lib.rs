//! The contract between the SmithAgents daemon and its plugins.
//!
//! Plugins receive a [`SyncRequest`], reach the network only through
//! [`HostApi::http`], and answer with a [`SyncPage`]. Everything that crosses a
//! host boundary (dylib, WebAssembly, process) is one of these serialisable types.

#[cfg(feature = "dylib")]
pub mod dylib;
mod envelope;
mod manifest;
mod types;
#[cfg(feature = "wasm-guest")]
#[doc(hidden)]
pub mod wasm_guest;

pub use envelope::{HostCall, HostReply, PluginCall, PluginReply};
pub use manifest::{
    HostMode, Manifest, ManifestError, PluginId, PluginIdError, ResourceDecl, SecretDecl,
    SettingDecl, SettingKind, SettingsError, ToolDecl, ToolSource,
};
pub use types::{
    Auth, ContractError, HostApi, HttpError, HttpMethod, HttpRequest, HttpResponse, Item, LogLevel,
    Plugin, PluginError, SyncPage, SyncRequest,
};

/// The contract version this build speaks.
pub const CONTRACT_VERSION: u32 = 1;
/// The oldest contract version this build still loads.
pub const MIN_CONTRACT_VERSION: u32 = 1;
