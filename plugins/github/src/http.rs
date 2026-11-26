//! One authenticated GET against the GitHub API, and what its answer means.

use chrono::{DateTime, Utc};
use serde_json::Value;
use smithagents_plugin_protocol::{
    Auth, HostApi, HttpError, HttpMethod, HttpRequest, HttpResponse, PluginError,
};

pub const API_PREFIX: &str = "https://api.github.com/";

/// What a `Link` header says about the next page.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// No `rel="next"`: the listing is finished.
    None,
    /// A page on the API host.
    Url(String),
    /// A `rel="next"` this plugin will not follow; the listing is not finished.
    Refused,
}

/// A successful, parsed answer and the next page it points to, if any.
pub struct Fetched {
    pub body: Value,
    pub next: Next,
    /// The response's `date` header, when it parses.
    pub now: Option<DateTime<Utc>>,
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

/// The `rel="next"` entry of a `Link` header, followed only on the API host.
pub fn next_link(header: &str) -> Next {
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
    let next_url = entries.iter().find_map(|entry| {
        let rest = entry.trim().strip_prefix('<')?;
        let (url, params) = rest.split_once('>')?;
        let is_next = params.split(';').any(|p| {
            p.trim()
                .strip_prefix("rel=")
                .is_some_and(|v| v.trim_matches('"').split_whitespace().any(|r| r == "next"))
        });
        is_next.then(|| url.to_string())
    });
    match next_url {
        None => Next::None,
        Some(url) if is_api_url(&url) => Next::Url(url),
        Some(_) => Next::Refused,
    }
}

/// The error for a next-page link that is not followed: stopping quietly
/// would commit a mark over pages that were never read.
pub fn refused_next(repo: &str) -> PluginError {
    other(format!(
        "{repo}: GitHub returned a next page link this plugin does not follow"
    ))
}

fn rate_limit_wait(resp: &HttpResponse) -> Option<u32> {
    if let Some(retry) = resp.header("retry-after") {
        let secs = retry
            .trim()
            .parse::<u64>()
            .map_or(60, |n| i64::try_from(n).unwrap_or(i64::MAX));
        return Some(clamp_wait(secs));
    }
    if resp.header("x-ratelimit-remaining").map(str::trim) != Some("0") {
        return None;
    }
    let reset = resp
        .header("x-ratelimit-reset")
        .and_then(|v| v.trim().parse::<i64>().ok());
    let wait = match (reset, response_date(resp)) {
        (Some(reset), Some(now)) => clamp_wait(reset.saturating_sub(now.timestamp())),
        _ => 60,
    };
    Some(wait)
}

/// Seconds to wait, kept between a minute and an hour.
fn clamp_wait(secs: i64) -> u32 {
    u32::try_from(secs.clamp(60, 3600)).unwrap_or(60)
}

fn response_date(resp: &HttpResponse) -> Option<DateTime<Utc>> {
    resp.header("date")
        .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.with_timezone(&Utc))
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
    let next = resp.header("link").map_or(Next::None, next_link);
    Ok(Fetched {
        body,
        next,
        now: response_date(&resp),
    })
}
