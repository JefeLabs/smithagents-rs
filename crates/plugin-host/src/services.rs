//! What one plugin's calls reach: HTTP under the policy, secret injection and logging.

use std::collections::HashSet;
use std::fmt::{self, Write as _};
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
    secret_keys: HashSet<String>,
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
            secret_keys: manifest.secrets.iter().map(|s| s.key.clone()).collect(),
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
        }))
    }

    fn check_declared(&self, key: &str) -> Result<(), HttpError> {
        if self.secret_keys.contains(key) {
            Ok(())
        } else {
            Err(denied(format!(
                "secret {key:?} is not declared in the plugin manifest"
            )))
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

    /// The header `auth` asks for, with the secret values filled in by the host.
    async fn auth_header(&self, auth: &Auth) -> Result<(HeaderName, HeaderValue), HttpError> {
        match auth {
            Auth::Bearer { secret } => {
                self.check_declared(secret)?;
                let value = self.secret(secret).await?;
                let value = sensitive(format!("Bearer {}", value.expose()), secret)?;
                Ok((AUTHORIZATION, value))
            }
            Auth::Basic {
                user_secret,
                password_secret,
            } => {
                self.check_declared(user_secret)?;
                self.check_declared(password_secret)?;
                let user = self.secret(user_secret).await?;
                let password = self.secret(password_secret).await?;
                let pair = format!("{}:{}", user.expose(), password.expose());
                let value = sensitive(format!("Basic {}", STANDARD.encode(pair)), user_secret)?;
                Ok((AUTHORIZATION, value))
            }
            Auth::Header { name, secret } => {
                let name = http::auth_header_name(name)?;
                self.check_declared(secret)?;
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

/// One call into a plugin. It counts the call's requests against the budget;
/// clones are cheap and share it.
#[derive(Clone)]
pub struct CallHandle(Arc<Call>);

struct Call {
    services: Arc<HostServices>,
    purpose: CallPurpose,
    requests: AtomicU64,
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
        let url = http::check_url(&req.url, &services.hosts, policy)?;
        let method = http::check_method(call.purpose, req.method)?;
        let mut headers = http::plugin_headers(&req.headers)?;
        if req
            .body
            .as_ref()
            .is_some_and(|b| b.len() > policy.max_request_bytes)
        {
            return Err(denied(format!(
                "request body larger than {} bytes",
                policy.max_request_bytes
            )));
        }
        if let Some(auth) = &req.auth {
            let (name, value) = services.auth_header(auth).await?;
            headers.insert(name, value);
        }
        http::send(&services.client, policy, method, url, headers, req.body).await
    }

    /// Logs a plugin message as a tracing event carrying the plugin id. The
    /// message is cut to 2 KiB and its control characters are escaped.
    pub fn log(&self, level: LogLevel, message: &str) {
        let plugin = &self.0.services.plugin;
        let message = Escaped(truncate(message, MAX_LOG_BYTES));
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

/// The longest prefix of `s` of at most `max` bytes that ends on a character boundary.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Plugin text with control characters escaped, so it cannot forge log lines
/// or send terminal escapes.
struct Escaped<'a>(&'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            if c.is_control() {
                write!(f, "{}", c.escape_default())?;
            } else {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_whole_characters() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 4), "abcd");
        // 'é' is two bytes; cutting inside it backs off to before it.
        assert_eq!(truncate("aé", 2), "a");
    }

    #[test]
    fn escaped_neutralises_control_characters() {
        assert_eq!(Escaped("a\nb\u{1b}[0m").to_string(), "a\\nb\\u{1b}[0m");
    }
}
