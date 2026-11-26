//! The GitHub plugin: repositories, issues and pull requests, read-only.
//!
//! Pure logic over the plugin contract. It sees only a [`HostApi`]; the token
//! never reaches it, requests ask the host to attach the `token` secret. Each
//! `sync` call makes one HTTP request and returns one page.

mod cursor;
mod http;
mod issues;
mod pulls;
mod repos;

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use cursor::Cursor;
use serde_json::{json, Value};
use smithagents_plugin_protocol::{HostApi, Item, Plugin, PluginError, SyncPage, SyncRequest};

/// A sanity cap; what a cursor can actually hold (a mark per repository in
/// 4 KiB) is checked against the configured names by `Cursor::check_fits`.
const MAX_REPOSITORIES: usize = 50;
const MAX_TITLE_CHARS: usize = 512;
const MAX_URL_BYTES: usize = 4 * 1024;
/// Entries taken from one response, however many it holds.
pub(crate) const MAX_ENTRIES: usize = 500;
const MAX_FIELDS_BYTES: usize = 64 * 1024;
const MAX_TEXT: usize = 1024;
pub(crate) const MAX_DESCRIPTION: usize = 2048;
const MAX_LIST: usize = 100;

/// One resource's sync over the validated repositories and the saved cursor.
type ResourceSync = fn(&dyn HostApi, &[String], Option<&str>) -> Result<SyncPage, PluginError>;

#[derive(Debug, Default, Clone, Copy)]
pub struct GitHub;

fn settings_error(message: String) -> PluginError {
    PluginError::Settings { message }
}

fn valid_part(part: &str, max: usize) -> bool {
    !part.is_empty()
        && part.len() <= max
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// The configured repositories, validated, de-duplicated and sorted.
fn repositories(settings: &Value) -> Result<Vec<String>, PluginError> {
    let list = settings
        .get("repositories")
        .and_then(Value::as_array)
        .ok_or_else(|| settings_error("repositories must be a list of owner/name".into()))?;
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let text = entry
            .as_str()
            .ok_or_else(|| settings_error("repositories must be a list of owner/name".into()))?;
        let valid = text
            .split_once('/')
            .is_some_and(|(owner, name)| valid_part(owner, 39) && valid_part(name, 100));
        if !valid {
            let shown: String = text.chars().take(60).collect();
            return Err(settings_error(format!(
                "{shown:?} is not a repository: write it as owner/name"
            )));
        }
        out.push(text.to_string());
    }
    // GitHub names are case-insensitive: keep the first spelling given.
    let mut seen = HashSet::new();
    out.retain(|name| seen.insert(name.to_ascii_lowercase()));
    out.sort();
    if out.len() > MAX_REPOSITORIES {
        return Err(settings_error(format!(
            "too many repositories: {} (at most {MAX_REPOSITORIES})",
            out.len()
        )));
    }
    Cursor::check_fits(&out)?;
    Ok(out)
}

impl Plugin for GitHub {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError> {
        let sync_fn: ResourceSync = match req.resource.as_str() {
            "repository" => repos::sync,
            "issue" => issues::sync,
            "pull_request" => pulls::sync,
            other => {
                return Err(PluginError::Other {
                    message: format!("unknown resource {other:?}"),
                })
            }
        };
        let repos = repositories(&req.settings)?;
        if repos.is_empty() {
            // The user configured nothing: for repositories that is a complete
            // (empty) listing; the others never delete.
            return Ok(SyncPage {
                upserts: Vec::new(),
                deletes: Vec::new(),
                next_cursor: None,
                done: true,
                snapshot_complete: req.resource == "repository",
            });
        }
        sync_fn(host, &repos, req.cursor.as_deref())
    }
}

// Small readers shared by the resources.

pub(crate) fn opt_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The entry's `html_url` when it is a plain absolute http(s) URL the contract
/// accepts; otherwise none, so an odd value never invalidates a whole page.
pub(crate) fn web_url(entry: &Value) -> Option<String> {
    let raw = opt_str(entry, "html_url")?;
    let plain = !raw.chars().any(|c| c.is_control() || c.is_whitespace());
    let web = url::Url::parse(raw)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
    (plain && web && raw.len() <= MAX_URL_BYTES).then(|| raw.to_string())
}

pub(crate) fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub(crate) fn truncate_title(title: &str) -> String {
    title.chars().take(MAX_TITLE_CHARS).collect()
}

/// `s` cut to at most `max` bytes on a character boundary.
pub(crate) fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `entry[key]` when it is a scalar (strings capped at `max` bytes); any
/// object or array becomes null, so nothing nested is ever copied.
pub(crate) fn scalar(entry: &Value, key: &str, max: usize) -> Value {
    match entry.get(key) {
        Some(Value::String(s)) => json!(cap(s, max)),
        Some(v @ (Value::Bool(_) | Value::Number(_))) => v.clone(),
        _ => Value::Null,
    }
}

/// A capped string from `entry[key]`, or null.
pub(crate) fn text(entry: &Value, key: &str) -> Value {
    opt_str(entry, key).map_or(Value::Null, |s| json!(cap(s, MAX_TEXT)))
}

/// Label names; GitHub sends objects, older shapes send plain strings.
pub(crate) fn labels(entry: &Value) -> Value {
    let names: Vec<String> = entry
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str().or_else(|| opt_str(l, "name")))
        .take(MAX_LIST)
        .map(|n| cap(n, MAX_TEXT))
        .collect();
    json!(names)
}

/// The `login` of each user under `key`.
pub(crate) fn logins(entry: &Value, key: &str) -> Value {
    let names: Vec<String> = entry
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|u| opt_str(u, "login"))
        .take(MAX_LIST)
        .map(|n| cap(n, MAX_TEXT))
        .collect();
    json!(names)
}

/// A nested `entry[outer][inner]` string, capped.
pub(crate) fn nested(entry: &Value, outer: &str, inner: &str) -> Value {
    entry.get(outer).map_or(Value::Null, |o| text(o, inner))
}

/// The items of one page: no repeated id, nothing over the contract's size.
#[derive(Default)]
pub(crate) struct Upserts {
    pub items: Vec<Item>,
    seen: HashSet<String>,
}

impl Upserts {
    pub fn push(&mut self, item: Option<Item>) {
        let Some(item) = item else { return };
        let size = serde_json::to_vec(&item.fields).map_or(usize::MAX, |v| v.len());
        if size > MAX_FIELDS_BYTES || !self.seen.insert(item.id.clone()) {
            return;
        }
        self.items.push(item);
    }
}

/// Whether a timestamp may move the mark: not more than a day ahead of the
/// response's own clock. Without a clock there is nothing to compare with.
pub(crate) fn plausible(ts: &str, now: Option<DateTime<Utc>>) -> bool {
    match (parse_time(ts), now) {
        (Some(ts), Some(now)) => ts <= now + Duration::hours(24),
        (Some(_), None) => true,
        _ => false,
    }
}
