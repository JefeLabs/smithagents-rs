use std::collections::HashSet;

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_ITEMS: usize = 500;
const MAX_FIELDS_BYTES: usize = 64 * 1024;
const MAX_TITLE_CHARS: usize = 512;
const MAX_ID_BYTES: usize = 4 * 1024;
const MAX_CURSOR_BYTES: usize = 4 * 1024;

/// One sync call: which resource to fetch, from which cursor, with which settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    pub resource: String,
    pub cursor: Option<String>,
    pub settings: Value,
}

/// One record a plugin reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub version: Option<String>,
    pub fields: Value,
}

/// One page of a sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncPage {
    pub upserts: Vec<Item>,
    pub deletes: Vec<String>,
    pub next_cursor: Option<String>,
    pub done: bool,
    pub snapshot_complete: bool,
}

/// A page that breaks the contract's size or shape limits.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ContractError {
    #[error("too many upserts: {0} (at most 500)")]
    TooManyUpserts(usize),
    #[error("too many deletes: {0} (at most 500)")]
    TooManyDeletes(usize),
    #[error("item {0}: fields larger than 64 KiB")]
    FieldsTooLarge(String),
    #[error("item {0}: fields is not an object")]
    FieldsNotObject(String),
    #[error("item {0}: title longer than 512 characters")]
    TitleTooLong(String),
    #[error("duplicate id {0}")]
    DuplicateId(String),
    #[error("empty id")]
    EmptyId,
    #[error("id longer than 4 KiB")]
    IdTooLong,
    #[error("cursor longer than 4 KiB")]
    CursorTooLong,
}

fn check_id(id: &str) -> Result<(), ContractError> {
    if id.is_empty() {
        Err(ContractError::EmptyId)
    } else if id.len() > MAX_ID_BYTES {
        Err(ContractError::IdTooLong)
    } else {
        Ok(())
    }
}

impl SyncPage {
    /// Check the page against the contract's limits.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.upserts.len() > MAX_ITEMS {
            return Err(ContractError::TooManyUpserts(self.upserts.len()));
        }
        if self.deletes.len() > MAX_ITEMS {
            return Err(ContractError::TooManyDeletes(self.deletes.len()));
        }
        if self
            .next_cursor
            .as_ref()
            .is_some_and(|c| c.len() > MAX_CURSOR_BYTES)
        {
            return Err(ContractError::CursorTooLong);
        }
        let mut seen = HashSet::new();
        for item in &self.upserts {
            check_id(&item.id)?;
            if !seen.insert(item.id.as_str()) {
                return Err(ContractError::DuplicateId(item.id.clone()));
            }
            if item.title.chars().count() > MAX_TITLE_CHARS {
                return Err(ContractError::TitleTooLong(item.id.clone()));
            }
            if !item.fields.is_object() {
                return Err(ContractError::FieldsNotObject(item.id.clone()));
            }
            // Serialising a `Value` cannot fail.
            let size = serde_json::to_vec(&item.fields).map_or(usize::MAX, |v| v.len());
            if size > MAX_FIELDS_BYTES {
                return Err(ContractError::FieldsTooLarge(item.id.clone()));
            }
        }
        let mut seen = HashSet::new();
        for id in &self.deletes {
            check_id(id)?;
            if !seen.insert(id.as_str()) {
                return Err(ContractError::DuplicateId(id.clone()));
            }
        }
        Ok(())
    }
}

/// Why a sync failed, as the plugin reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginError {
    #[error("auth: {message}")]
    Auth { message: String },
    #[error("rate limited, retry in {retry_after_secs}s: {message}")]
    RateLimited {
        retry_after_secs: u32,
        message: String,
    },
    #[error("settings: {message}")]
    Settings { message: String },
    #[error("http: {message}")]
    Http { message: String },
    #[error("{message}")]
    Other { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

/// How the host authenticates a request. The fields name secrets; the host injects the values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    Bearer {
        secret: String,
    },
    Basic {
        user_secret: String,
        password_secret: String,
    },
    Header {
        name: String,
        secret: String,
    },
}

mod base64_opt {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().map(|b| STANDARD.encode(b)).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| STANDARD.decode(s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

mod base64_bytes {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(with = "base64_opt")]
    pub body: Option<Vec<u8>>,
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpError {
    #[error("denied: {reason}")]
    Denied { reason: String },
    #[error("secret missing: {key}")]
    SecretMissing { key: String },
    #[error("timeout")]
    Timeout,
    #[error("response too large")]
    TooLarge,
    #[error("transport: {message}")]
    Transport { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// What the host offers a plugin.
pub trait HostApi {
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
    fn log(&self, level: LogLevel, message: &str);
}

/// What a plugin implements.
pub trait Plugin: Send + Sync {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError>;
}
