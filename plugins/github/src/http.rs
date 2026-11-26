//! One authenticated GET against the GitHub API, and what its answer means.

use chrono::DateTime;
use serde_json::Value;
use smithagents_plugin_protocol::{
    Auth, HostApi, HttpError, HttpMethod, HttpRequest, HttpResponse, PluginError,
};

pub const API_PREFIX: &str = "https://api.github.com/";

/// A successful, parsed answer and the next page it points to, if any.
pub struct Fetched {
    pub body: Value,
    pub next: Option<String>,
}

fn other(message: impl Into<String>) -> PluginError {
    PluginError::Other {
        message: message.into(),
    }
}

/// True for an absolute URL on the GitHub API host, written out plainly.
pub fn is_api_url(url: &str) -> bool {
    url.len() > API_PREFIX.len()
        && url.starts_with(API_PREFIX)
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The `rel="next"` URL of a `Link` header, only when it is on the API host.
pub fn next_link(header: &str) -> Option<String> {
    // Split on commas that start a new `<url>` entry, so a comma inside a URL
    // does not cut it in two.
    let mut entries: Vec<String> = Vec::new();
    for part in header.split(',') {
        match entries.last_mut() {
            Some(last) if !part.trim_start().starts_with('<') => {
                last.push(',');
                last.push_str(part);
            }
            _ => entries.push(part.to_string()),
        }
    }
    entries.iter().find_map(|entry| {
        let entry = entry.trim();
        let rest = entry.strip_prefix('<')?;
        let (url, params) = rest.split_once('>')?;
        let is_next = params.split(';').any(|p| {
            p.trim()
                .strip_prefix("rel=")
                .is_some_and(|v| v.trim_matches('"').split_whitespace().any(|r| r == "next"))
        });
        (is_next && is_api_url(url)).then(|| url.to_string())
    })
}

fn rate_limit_wait(resp: &HttpResponse) -> Option<u32> {
    if let Some(retry) = resp.header("retry-after") {
        let secs = retry.trim().parse::<u64>().unwrap_or(60);
        return Some(u32::try_from(secs).unwrap_or(u32::MAX));
    }
    if resp.header("x-ratelimit-remaining").map(str::trim) != Some("0") {
        return None;
    }
    let reset = resp
        .header("x-ratelimit-reset")
        .and_then(|v| v.trim().parse::<i64>().ok());
    let now = resp
        .header("date")
        .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.timestamp());
    let wait = match (reset, now) {
        (Some(reset), Some(now)) => reset.saturating_sub(now).clamp(60, 3600),
        _ => 60,
    };
    Some(u32::try_from(wait).unwrap_or(60))
}

fn shorten(text: &str) -> String {
    text.chars().take(80).collect()
}

/// Map a transport failure. Messages say what failed, never carry secrets.
fn transport_error(err: HttpError) -> PluginError {
    match err {
        HttpError::SecretMissing { .. } => PluginError::Auth {
            message: "no GitHub token is available; sign in with `gh auth login`".into(),
        },
        HttpError::Timeout => PluginError::Http {
            message: "the request to GitHub timed out".into(),
        },
        HttpError::TooLarge => PluginError::Http {
            message: "GitHub's response was too large".into(),
        },
        HttpError::Transport { message } => PluginError::Http {
            message: format!("could not reach GitHub: {}", shorten(&message)),
        },
        HttpError::Denied { reason } => other(format!(
            "the host refused the request: {}",
            shorten(&reason)
        )),
    }
}

/// GET `url` as JSON. `repo` names the repository for error messages.
pub fn get(host: &dyn HostApi, url: &str, repo: &str) -> Result<Fetched, PluginError> {
    let req = HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: vec![
            ("Accept".into(), "application/vnd.github+json".into()),
            ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
            ("User-Agent".into(), "smithagents".into()),
        ],
        body: None,
        auth: Some(Auth::Bearer {
            secret: "token".into(),
        }),
    };
    let resp = host.http(req).map_err(transport_error)?;
    match resp.status {
        200..=299 => {}
        401 => {
            return Err(PluginError::Auth {
                message: "GitHub rejected the token".into(),
            })
        }
        403 | 429 => {
            return Err(match rate_limit_wait(&resp) {
                Some(secs) => PluginError::RateLimited {
                    retry_after_secs: secs,
                    message: "GitHub rate limit reached".into(),
                },
                None => PluginError::Http {
                    message: format!("GitHub returned status {}", resp.status),
                },
            })
        }
        404 => {
            return Err(PluginError::Settings {
                message: format!("{repo}: not found or not visible to this token"),
            })
        }
        301 => {
            return Err(PluginError::Settings {
                message: format!("{repo}: repository moved"),
            })
        }
        status => {
            return Err(PluginError::Http {
                message: format!("GitHub returned status {status}"),
            })
        }
    }
    let body: Value = serde_json::from_slice(&resp.body)
        .map_err(|_| other("GitHub sent a response that is not valid JSON"))?;
    let next = resp.header("link").and_then(next_link);
    Ok(Fetched { body, next })
}
