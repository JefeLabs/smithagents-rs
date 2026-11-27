//! The contract between the SmithAgents daemon and its plugins.
//!
//! Plugins receive a [`SyncRequest`], reach the network only through
//! [`HostApi::http`], and answer with a [`SyncPage`]. Everything that crosses a
//! host boundary (dylib, WebAssembly, process) is one of these serialisable types.
//!
//! # What can change, and when
//!
//! - The types on the wire ([`SyncRequest`], [`Item`], [`SyncPage`],
//!   [`PluginError`], the HTTP types) have the shape the contract version
//!   fixes ([`CONTRACT_VERSION`]). They change only with it.
//! - What only the host side uses can grow without a new contract version:
//!   [`Manifest`] can gain fields, and [`ManifestError`], [`SettingsError`]
//!   and [`ContractError`] can gain variants. They are `#[non_exhaustive]`,
//!   so code outside this crate matches them with a wildcard arm.
//! - [`HostApi`] is implemented by hosts. A method added to it later gets a
//!   default body, so a host written against an older build still compiles.
//!
//! The crates whose types appear in this contract are re-exported
//! ([`chrono`], [`semver`], [`serde_json`]): name those types through this
//! crate and a plugin always has the versions the contract was built with.

#![warn(missing_debug_implementations)]

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
// The crates whose types appear in the contract, at the versions it uses:
// `Item::updated_at`, `Manifest::version`, `SyncRequest::settings`.
pub use {chrono, semver, serde_json};

/// The contract version this build speaks.
pub const CONTRACT_VERSION: u32 = 1;
/// The oldest contract version this build still loads.
pub const MIN_CONTRACT_VERSION: u32 = 1;
