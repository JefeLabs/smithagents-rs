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
const MAX_URL_BYTES: usize = 4 * 1024;
const MAX_VERSION_BYTES: usize = 4 * 1024;

/// One sync call: which resource to fetch, from which cursor, with which settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    /// The kind of resource to list, as the manifest declares it.
    pub resource: String,
    /// The `next_cursor` of this plugin's last page for the resource, also
    /// when that page was `done`; `None` when there has been none. It can
    /// have been written under older settings: a plugin must accept that,
    /// for instance by noticing it no longer fits and starting over.
    pub cursor: Option<String>,
    /// The user's settings with the manifest's defaults filled in: a JSON
    /// object.
    pub settings: Value,
}

/// One record a plugin reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// What identifies the record within its resource: not empty, at most
    /// 4 KiB, and once per page.
    pub id: String,
    /// At most 512 characters.
    pub title: String,
    /// Where the record can be opened: an absolute `http` or `https` URL
    /// with a host, written plainly (no spaces or control characters), at
    /// most 4 KiB.
    pub url: Option<String>,
    /// When the record last changed, if the source says.
    pub updated_at: Option<DateTime<Utc>>,
    /// Any string that changes when the record does; at most 4 KiB.
    pub version: Option<String>,
    /// Whatever else the plugin reports: a JSON object of at most 64 KiB.
    pub fields: Value,
}

/// One page of a sync. A page that breaks a limit or a shape rule
/// ([`SyncPage::validate`]) is refused as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncPage {
    /// Items to insert or replace: at most 500.
    pub upserts: Vec<Item>,
    /// Ids of items to remove: at most 500.
    pub deletes: Vec<String>,
    /// Kept by the daemon and passed back as [`SyncRequest::cursor`] on the
    /// next call, also after a `done` page: a plugin can keep what it needs
    /// between listings there. `None` starts over. At most 4 KiB. A page
    /// that is not `done` and returns the cursor it was given makes no
    /// progress, which ends the run with an error.
    pub next_cursor: Option<String>,
    /// The listing is finished: the run ends, and the next run, one interval
    /// later, begins a new listing.
    pub done: bool,
    /// With `done`: this listing was the whole truth, so the daemon deletes
    /// every stored item of the resource that no page of it returned. Set it
    /// only when the plugin really listed everything. A listing whose
    /// beginning the daemon did not see deletes nothing, whatever this says.
    pub snapshot_complete: bool,
}

/// A page that breaks the contract's size or shape limits.
///
/// A later build may refuse a page for a reason this one does not have: the
/// set of variants can grow.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
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
    #[error("item {0}: url longer than 4 KiB")]
    UrlTooLong(String),
    #[error("item {0}: url is not an absolute http or https URL")]
    BadUrl(String),
    #[error("item {0}: version longer than 4 KiB")]
    VersionTooLong(String),
    #[error("duplicate id {0}")]
    DuplicateId(String),
    #[error("empty id")]
    EmptyId,
    #[error("id longer than 4 KiB")]
    IdTooLong,
    #[error("cursor longer than 4 KiB")]
    CursorTooLong,
}

/// An absolute `http` or `https` URL with a host, written out plainly: no
/// spaces or control characters, which a URL parser would silently drop.
fn is_web_url(raw: &str) -> bool {
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    url::Url::parse(raw).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
    })
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
    /// Checks the page against the contract's limits and shape rules.
    ///
    /// # Errors
    ///
    /// The first rule the page breaks: more than 500 upserts or deletes; a
    /// cursor, id, url or version over 4 KiB; an empty or repeated id; a
    /// title over 512 characters; a url that is not a plain absolute `http`
    /// or `https` address; `fields` that is not an object or exceeds 64 KiB.
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
            if let Some(url) = &item.url {
                if url.len() > MAX_URL_BYTES {
                    return Err(ContractError::UrlTooLong(item.id.clone()));
                }
                if !is_web_url(url) {
                    return Err(ContractError::BadUrl(item.id.clone()));
                }
            }
            if item
                .version
                .as_ref()
                .is_some_and(|v| v.len() > MAX_VERSION_BYTES)
            {
                return Err(ContractError::VersionTooLong(item.id.clone()));
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

/// Why a sync failed, as the plugin reports it. Each variant tells the
/// daemon what to do next, so choose the one that fits. A message is shown
/// to the user and must not hold a secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginError {
    /// The source refused the credentials. The daemon shows the plugin as
    /// needing sign-in, drops the secret values it cached for it, and stops
    /// its runs until the settings change or the user asks for a sync.
    #[error("auth: {message}")]
    Auth { message: String },
    /// The source asked to wait. The next run waits for the longer of
    /// `retry_after_secs` and the resource's interval.
    #[error("rate limited, retry in {retry_after_secs}s: {message}")]
    RateLimited {
        retry_after_secs: u32,
        message: String,
    },
    /// The settings are wrong: the plugin waits for new ones.
    #[error("settings: {message}")]
    Settings { message: String },
    /// A request failed in a way that may pass. The run fails and is tried
    /// again with a back-off from 30 seconds, doubling, up to 30 minutes.
    #[error("http: {message}")]
    Http { message: String },
    /// Anything else; retried like [`PluginError::Http`].
    #[error("{message}")]
    Other { message: String },
}

/// The method of an [`HttpRequest`]. During a sync the host allows only
/// `Get` and `Head`.
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
    /// `Authorization: Bearer <value of secret>`.
    Bearer { secret: String },
    /// `Authorization: Basic`, from the values of two secrets.
    Basic {
        user_secret: String,
        password_secret: String,
    },
    /// The header `name` with the value of `secret`.
    Header { name: String, secret: String },
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

/// A request the host makes on the plugin's behalf ([`HostApi::http`]): the
/// plugin has no network of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    /// An `https` address on port 443 of a host the manifest declares.
    pub url: String,
    /// Extra headers. Credential and connection headers (`Authorization`,
    /// `Cookie`, `Host` and the like) are refused here: a credential comes
    /// only from `auth`.
    pub headers: Vec<(String, String)>,
    /// At most 1 MiB; refused during a sync.
    #[serde(with = "base64_opt")]
    pub body: Option<Vec<u8>>,
    /// Which secret the host adds, by name. The plugin never sees its value.
    pub auth: Option<Auth>,
}

/// What came back for an [`HttpRequest`]. Redirects are not followed: a 3xx
/// answer is returned as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// At most 5 MiB.
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

/// Why the host made no request, or got no answer, for an [`HttpRequest`].
/// A status such as 404 or 500 is not an error: it is an [`HttpResponse`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpError {
    /// The host refused the request (an address the manifest does not
    /// declare, a header or method it does not allow, too many requests in
    /// one call). The reason never holds a secret.
    #[error("denied: {reason}")]
    Denied { reason: String },
    /// The secret `auth` names has no value: the user has not signed in.
    #[error("secret missing: {key}")]
    SecretMissing { key: String },
    /// No answer within 30 seconds.
    #[error("timeout")]
    Timeout,
    /// The answer's body is over 5 MiB.
    #[error("response too large")]
    TooLarge,
    /// The connection failed.
    #[error("transport: {message}")]
    Transport { message: String },
}

/// How much a line a plugin logs ([`HostApi::log`]) matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// What the host offers a plugin during a call. Hosts implement it; a
/// plugin only calls it.
pub trait HostApi {
    /// Makes one request and returns the answer, whatever its status. At
    /// most 50 requests per call into the plugin.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the host refuses the request or no answer comes;
    /// see its variants.
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
    /// Writes one line to the daemon's log, under the plugin's name. The
    /// host cleans the text and caps how much one call may log; never log
    /// a secret.
    fn log(&self, level: LogLevel, message: &str);
}

/// What a plugin implements.
pub trait Plugin: Send + Sync {
    /// Returns one page of `req.resource`, starting at `req.cursor`. The
    /// daemon calls again with the page's `next_cursor` until a page is
    /// `done`, at most 20 pages per run. The network is reached only
    /// through `host`.
    ///
    /// A call must answer within 60 seconds, and a WebAssembly plugin also
    /// within 10 seconds of its own CPU time. A call that does not, or that
    /// panics, fails the run, which is tried again later; a native plugin
    /// is then stopped until the daemon restarts.
    ///
    /// # Errors
    ///
    /// A [`PluginError`] whose variant says what the daemon should do next:
    /// ask the user to sign in, wait, ask for new settings, or try again.
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError>;
}
