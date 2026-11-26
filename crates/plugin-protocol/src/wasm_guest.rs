//! The guest side of the WebAssembly contract: bindings for `wit/plugin.wit`
//! and the adapter behind [`export_wasm_plugin!`](crate::export_wasm_plugin).
//!
//! JSON travels as strings and times as RFC 3339 strings; the adapter turns
//! them into the contract's types, so a plugin is the same [`Plugin`] in
//! every host mode.

use chrono::SecondsFormat;
use serde_json::Value;

use crate::types::{
    Auth, HostApi, HttpError, HttpMethod, HttpRequest, HttpResponse, Item, LogLevel, Plugin,
    PluginError, SyncPage, SyncRequest,
};

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit/plugin.wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "export",
    });
}

use bindings::smithagents::plugin::{host, types as wit};

#[doc(hidden)]
pub use bindings::Guest;
#[doc(hidden)]
pub use wit::{
    PluginError as WitPluginError, SyncPage as WitSyncPage, SyncRequest as WitSyncRequest,
};

/// Runs one `sync` export call: a fresh `P` per call, as each call gets a
/// fresh instance anyway.
#[doc(hidden)]
pub fn sync<P: Plugin + Default>(req: WitSyncRequest) -> Result<WitSyncPage, WitPluginError> {
    let settings: Value = serde_json::from_str(&req.settings).map_err(|e| {
        WitPluginError::Other(format!("the host sent settings that are not JSON: {e}"))
    })?;
    let req = SyncRequest {
        resource: req.resource,
        cursor: req.cursor,
        settings,
    };
    P::default()
        .sync(&WasmHost, req)
        .map(page_to_wit)
        .map_err(error_to_wit)
}

/// The host as a guest sees it: the `host` imports.
struct WasmHost;

impl HostApi for WasmHost {
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        host::http_request(&request_to_wit(req))
            .map(|resp| HttpResponse {
                status: resp.status,
                headers: resp.headers,
                body: resp.body,
            })
            .map_err(http_error_from_wit)
    }

    fn log(&self, level: LogLevel, message: &str) {
        let level = match level {
            LogLevel::Debug => wit::LogLevel::Debug,
            LogLevel::Info => wit::LogLevel::Info,
            LogLevel::Warn => wit::LogLevel::Warn,
            LogLevel::Error => wit::LogLevel::Error,
        };
        host::log(level, message);
    }
}

fn request_to_wit(req: HttpRequest) -> wit::HttpRequest {
    wit::HttpRequest {
        method: match req.method {
            HttpMethod::Get => wit::HttpMethod::Get,
            HttpMethod::Head => wit::HttpMethod::Head,
            HttpMethod::Post => wit::HttpMethod::Post,
            HttpMethod::Put => wit::HttpMethod::Put,
            HttpMethod::Patch => wit::HttpMethod::Patch,
            HttpMethod::Delete => wit::HttpMethod::Delete,
        },
        url: req.url,
        headers: req.headers,
        body: req.body,
        auth: req.auth.map(|auth| match auth {
            Auth::Bearer { secret } => wit::Auth::Bearer(secret),
            Auth::Basic {
                user_secret,
                password_secret,
            } => wit::Auth::Basic((user_secret, password_secret)),
            Auth::Header { name, secret } => wit::Auth::Header((name, secret)),
        }),
    }
}

fn http_error_from_wit(err: wit::HttpError) -> HttpError {
    match err {
        wit::HttpError::Denied(reason) => HttpError::Denied { reason },
        wit::HttpError::SecretMissing(key) => HttpError::SecretMissing { key },
        wit::HttpError::Timeout => HttpError::Timeout,
        wit::HttpError::TooLarge => HttpError::TooLarge,
        wit::HttpError::Transport(message) => HttpError::Transport { message },
    }
}

fn page_to_wit(page: SyncPage) -> WitSyncPage {
    WitSyncPage {
        upserts: page.upserts.into_iter().map(item_to_wit).collect(),
        deletes: page.deletes,
        next_cursor: page.next_cursor,
        done: page.done,
        snapshot_complete: page.snapshot_complete,
    }
}

fn item_to_wit(item: Item) -> wit::Item {
    wit::Item {
        id: item.id,
        title: item.title,
        url: item.url,
        updated_at: item
            .updated_at
            .map(|t| t.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
        version: item.version,
        fields: item.fields.to_string(),
    }
}

fn error_to_wit(err: PluginError) -> WitPluginError {
    match err {
        PluginError::Auth { message } => WitPluginError::Auth(message),
        PluginError::RateLimited {
            retry_after_secs,
            message,
        } => WitPluginError::RateLimited((retry_after_secs, message)),
        PluginError::Settings { message } => WitPluginError::Settings(message),
        PluginError::Http { message } => WitPluginError::Http(message),
        PluginError::Other { message } => WitPluginError::Other(message),
    }
}

/// Exports `$plugin` (a type implementing [`Plugin`](crate::Plugin) and
/// `Default`) as the `sync` function of a WebAssembly component built for
/// `wasm32-wasip2`. Use it once, in a `cdylib` crate that enables this
/// crate's `wasm-guest` feature.
#[macro_export]
macro_rules! export_wasm_plugin {
    ($plugin:ty) => {
        const _: () = {
            struct SmithagentsWasmPlugin;

            impl $crate::wasm_guest::Guest for SmithagentsWasmPlugin {
                fn sync(
                    req: $crate::wasm_guest::WitSyncRequest,
                ) -> ::core::result::Result<
                    $crate::wasm_guest::WitSyncPage,
                    $crate::wasm_guest::WitPluginError,
                > {
                    $crate::wasm_guest::sync::<$plugin>(req)
                }
            }

            $crate::wasm_guest::bindings::export!(
                SmithagentsWasmPlugin with_types_in $crate::wasm_guest::bindings
            );
        };
    };
}
