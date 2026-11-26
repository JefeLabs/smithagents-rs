//! What one plugin's calls reach: HTTP under the policy, secret injection and logging.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION};
use smithagents_plugin_protocol::{
    Auth, HttpError, HttpRequest, HttpResponse, LogLevel, Manifest, PluginId,
};

use crate::http::{self, denied, HttpPolicy};
use crate::secrets::{Secret, SecretSource};

const MAX_LOG_BYTES: usize = 2 * 1024;
/// Lines one call may log; the rest are dropped, so a plugin cannot flood the
/// daemon's log.
const MAX_LOG_LINES: u64 = 100;

/// Why a call runs. It decides which HTTP methods the plugin may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPurpose {
    /// Reading for a sync: GET and HEAD only.
    Sync,
}

#[derive(Debug, thiserror::Error)]
pub enum ServicesError {
    #[error("manifest host {0:?} is not a DNS name (a URL parser reads it as an IP address)")]
    BadHost(String),
    #[error("cannot build the HTTP client: {0}")]
    Client(String),
}

/// One plugin's host services, shared by all of its calls.
pub struct HostServices {
    plugin: PluginId,
    hosts: Vec<String>,
    /// Each declared secret and the hosts it may be sent to.
    secret_hosts: HashMap<String, Vec<String>>,
    secrets: Arc<dyn SecretSource>,
    policy: HttpPolicy,
    client: reqwest::Client,
}

impl HostServices {
    pub fn new(
        manifest: &Manifest,
        secrets: Arc<dyn SecretSource>,
        policy: HttpPolicy,
    ) -> Result<HostServices, ServicesError> {
        // `Manifest::parse` refuses dotted quads; this also catches the spellings
        // a URL parser still reads as an address, such as `0x7f.0x0.0x0.0x1`.
        let hosts = manifest
            .hosts
            .iter()
            .map(|host| {
                if http::is_ip_host(host) {
                    Err(ServicesError::BadHost(host.clone()))
                } else {
                    Ok(host.to_ascii_lowercase())
                }
            })
            .collect::<Result<_, _>>()?;
        let client = http::client().map_err(|e| ServicesError::Client(e.to_string()))?;
        Ok(HostServices {
            plugin: manifest.id.clone(),
            hosts,
            secret_hosts: manifest
                .secrets
                .iter()
                .map(|s| (s.key.clone(), s.hosts.clone()))
                .collect(),
            secrets,
            policy,
            client,
        })
    }

    /// Starts one call with its own request budget.
    pub fn begin_call(self: &Arc<Self>, purpose: CallPurpose) -> CallHandle {
        CallHandle(Arc::new(Call {
            services: Arc::clone(self),
            purpose,
            requests: AtomicU64::new(0),
            log_lines: AtomicU64::new(0),
        }))
    }

    /// The secret must be declared, and the request must go to one of its
    /// hosts. `host` is `None` only under the loopback exemption (tests), where
    /// any declared secret may be sent.
    fn check_secret(&self, key: &str, host: Option<&str>) -> Result<(), HttpError> {
        let Some(allowed) = self.secret_hosts.get(key) else {
            return Err(denied(format!(
                "secret {key:?} is not declared in the plugin manifest"
            )));
        };
        match host {
            Some(host) if !allowed.iter().any(|h| h.eq_ignore_ascii_case(host)) => Err(denied(
                format!("secret {key:?} is not allowed for host {host:?}"),
            )),
            _ => Ok(()),
        }
    }

    async fn secret(&self, key: &str) -> Result<Secret, HttpError> {
        let missing = || HttpError::SecretMissing {
            key: key.to_string(),
        };
        match self.secrets.get(&self.plugin, key).await {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(missing()),
            Err(err) => {
                tracing::warn!(
                    plugin = %self.plugin,
                    key,
                    error = %err,
                    "cannot resolve a plugin secret"
                );
                Err(missing())
            }
        }
    }

    /// The header `auth` asks for, with the secret values filled in by the
    /// host. Every check runs before any secret is resolved.
    async fn auth_header(
        &self,
        auth: &Auth,
        host: Option<&str>,
        purpose: CallPurpose,
    ) -> Result<(HeaderName, HeaderValue), HttpError> {
        match auth {
            Auth::Bearer { secret } => {
                self.check_secret(secret, host)?;
                let value = self.secret(secret).await?;
                let value = sensitive(format!("Bearer {}", value.expose()), secret)?;
                Ok((AUTHORIZATION, value))
            }
            Auth::Basic {
                user_secret,
                password_secret,
            } => {
                self.check_secret(user_secret, host)?;
                self.check_secret(password_secret, host)?;
                let user = self.secret(user_secret).await?;
                let password = self.secret(password_secret).await?;
                let pair = format!("{}:{}", user.expose(), password.expose());
                let value = sensitive(format!("Basic {}", STANDARD.encode(pair)), user_secret)?;
                Ok((AUTHORIZATION, value))
            }
            Auth::Header { name, secret } => {
                let name = http::auth_header_name(name, purpose)?;
                self.check_secret(secret, host)?;
                let value = self.secret(secret).await?;
                Ok((name, sensitive(value.expose().to_string(), secret)?))
            }
        }
    }
}

impl fmt::Debug for HostServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostServices")
            .field("plugin", &self.plugin)
            .field("hosts", &self.hosts)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// A header value holding a secret. The error names the key, never the value.
fn sensitive(text: String, key: &str) -> Result<HeaderValue, HttpError> {
    let mut value = HeaderValue::try_from(text)
        .map_err(|_| denied(format!("secret {key:?} is not a valid header value")))?;
    value.set_sensitive(true);
    Ok(value)
}

/// One call into a plugin. It counts the call's requests and log lines
/// against their budgets; clones are cheap and share them.
#[derive(Clone)]
pub struct CallHandle(Arc<Call>);

struct Call {
    services: Arc<HostServices>,
    purpose: CallPurpose,
    requests: AtomicU64,
    log_lines: AtomicU64,
}

impl CallHandle {
    /// Sends `req` if the policy allows it. Every check runs before anything
    /// leaves the host; secrets are resolved last.
    pub async fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let call = &*self.0;
        let services = &*call.services;
        let policy = &services.policy;

        let count = call.requests.fetch_add(1, Ordering::Relaxed) + 1;
        if count > u64::from(policy.max_requests_per_call) {
            return Err(denied(format!(
                "more than {} requests in one call",
                policy.max_requests_per_call
            )));
        }
        let target = http::check_url(&req.url, &services.hosts, policy)?;
        let method = http::check_method(call.purpose, req.method)?;
        let mut headers = http::plugin_headers(&req.headers, call.purpose)?;
        // An empty body is no body.
        let body = req.body.filter(|b| !b.is_empty());
        if let Some(body) = &body {
            if body.len() > policy.max_request_bytes {
                return Err(denied(format!(
                    "request body larger than {} bytes",
                    policy.max_request_bytes
                )));
            }
            if call.purpose == CallPurpose::Sync {
                return Err(denied("a sync may not send a request body"));
            }
        }
        if let Some(auth) = &req.auth {
            let host = target.host.as_deref();
            let (name, value) = services.auth_header(auth, host, call.purpose).await?;
            headers.insert(name, value);
        }
        http::send(&services.client, policy, method, target.url, headers, body).await
    }

    /// Logs a plugin message as a tracing event carrying the plugin id. Control
    /// characters and bidi marks are escaped first, then the text is cut to 2 KiB.
    /// After 100 lines in one call, one warning says the rest are dropped.
    pub fn log(&self, level: LogLevel, message: &str) {
        let plugin = &self.0.services.plugin;
        let line = self.0.log_lines.fetch_add(1, Ordering::Relaxed) + 1;
        if line > MAX_LOG_LINES {
            if line == MAX_LOG_LINES + 1 {
                tracing::warn!(plugin = %plugin, "further log lines from this call were dropped");
            }
            return;
        }
        let message = log_text(message, MAX_LOG_BYTES);
        match level {
            LogLevel::Debug => tracing::debug!(plugin = %plugin, "{message}"),
            LogLevel::Info => tracing::info!(plugin = %plugin, "{message}"),
            LogLevel::Warn => tracing::warn!(plugin = %plugin, "{message}"),
            LogLevel::Error => tracing::error!(plugin = %plugin, "{message}"),
        }
    }
}

impl fmt::Debug for CallHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallHandle")
            .field("plugin", &self.0.services.plugin)
            .field("purpose", &self.0.purpose)
            .field("requests", &self.0.requests.load(Ordering::Relaxed))
            .finish()
    }
}

/// Plugin text made safe to log: control characters, line and paragraph
/// separators and bidi embedding, override and isolate marks become `\u{..}`
/// escapes (`\n` and the like for the common ones), so a plugin cannot forge
/// log lines, send terminal escapes or reorder what a reader sees. The escaped
/// text is cut to at most `max` bytes, never inside a character or an escape.
fn log_text(message: &str, max: usize) -> String {
    let mut out = String::new();
    let mut piece = String::new();
    for c in message.chars() {
        piece.clear();
        if c.is_control() || is_separator_or_bidi(c) {
            piece.extend(c.escape_default());
        } else {
            piece.push(c);
        }
        if out.len() + piece.len() > max {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// U+2028 and U+2029 (line and paragraph separators), U+202A to U+202E (bidi
/// embeddings and overrides) and U+2066 to U+2069 (bidi isolates).
fn is_separator_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_text_escapes_controls_and_bidi_marks() {
        assert_eq!(log_text("a\nb\u{1b}[0m", 100), "a\\nb\\u{1b}[0m");
        assert_eq!(
            log_text("\u{2028}\u{2029}\u{202a}\u{202e}\u{2066}\u{2069}", 100),
            "\\u{2028}\\u{2029}\\u{202a}\\u{202e}\\u{2066}\\u{2069}"
        );
        assert_eq!(log_text("é \u{2065}", 100), "é \u{2065}");
    }

    #[test]
    fn log_text_cuts_after_escaping_on_whole_characters_and_escapes() {
        assert_eq!(log_text("abc", 5), "abc");
        assert_eq!(log_text("abcdef", 4), "abcd");
        // 'é' is two bytes; it does not fit in the second byte.
        assert_eq!(log_text("aé", 2), "a");
        // `\n` escapes to two bytes: three fit in 7, and no escape is split.
        assert_eq!(log_text("\n\n\n\n", 7), "\\n\\n\\n");
    }
}
