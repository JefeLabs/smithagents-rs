//! What crosses the dylib boundary, as JSON. Byte bodies are base64 strings.

use serde::{Deserialize, Serialize};

use crate::types::{
    HttpError, HttpRequest, HttpResponse, LogLevel, PluginError, SyncPage, SyncRequest,
};

/// Host to plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginCall {
    Sync { call: u64, request: SyncRequest },
}

/// Plugin to host, answering a [`PluginCall`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginReply {
    Sync(Result<SyncPage, PluginError>),
    Panicked { message: String },
    BadCall { message: String },
}

/// Plugin to host, while a call runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostCall {
    Http {
        call: u64,
        request: HttpRequest,
    },
    Log {
        call: u64,
        level: LogLevel,
        message: String,
    },
}

/// Host to plugin, answering a [`HostCall`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostReply {
    Http(Result<HttpResponse, HttpError>),
    Done,
    BadCall { message: String },
}
