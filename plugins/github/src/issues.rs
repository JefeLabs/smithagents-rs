//! The `issue` resource: newest update first, bounded by `since`.
//!
//! Descending order is deliberate. Listing oldest first by page number, an
//! issue updated mid-walk moves to the end and shifts every later entry up
//! one place, so the first unread entry of the next page is skipped for good.
//! Newest first, the same shift only repeats a boundary entry, and the updated
//! issue shows up at the top of the next run. `since` is inclusive and already
//! bounds the listing, so the walk follows `next` links to the end.

use serde_json::{json, Value};
use smithagents_plugin_protocol::{HostApi, Item, PluginError, SyncPage};

use crate::cursor::{keep_newest, normalize_ts, Cursor};
use crate::http::{refused_next, Next};
use crate::{
    http, labels, logins, nested, opt_str, parse_time, plausible, scalar, truncate_title, web_url,
    Upserts, MAX_ENTRIES,
};

fn item(repo: &str, entry: &Value) -> Option<Item> {
    let number = entry.get("number")?.as_u64()?;
    let updated = opt_str(entry, "updated_at");
    Some(Item {
        id: format!("{repo}#{number}"),
        title: truncate_title(opt_str(entry, "title").unwrap_or("(no title)")),
        url: web_url(entry),
        updated_at: updated.and_then(parse_time),
        version: updated.map(str::to_string).filter(|v| v.len() <= 1024),
        fields: json!({
            "number": number,
            "state": scalar(entry, "state", 1024),
            "state_reason": scalar(entry, "state_reason", 1024),
            "author": nested(entry, "user", "login"),
            "labels": labels(entry),
            "assignees": logins(entry, "assignees"),
            "comments": scalar(entry, "comments", 0),
            "created_at": scalar(entry, "created_at", 1024),
            "closed_at": scalar(entry, "closed_at", 1024),
            "repository": repo,
        }),
    })
}

pub fn sync(
    host: &dyn HostApi,
    repos: &[String],
    raw_cursor: Option<&str>,
) -> Result<SyncPage, PluginError> {
    let mut cursor = Cursor::load(raw_cursor, repos);
    let repo = repos[cursor.repo].clone();
    let url = match &cursor.next {
        Some(next) => next.clone(),
        None => {
            let mut url = format!(
                "{}repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=100",
                http::API_PREFIX
            );
            if let Some(since) = cursor.mark(&repo) {
                url.push_str("&since=");
                url.push_str(since);
            }
            url
        }
    };
    let fetched = http::get(host, &url, &repo)?;
    let entries = fetched.body.as_array().ok_or_else(|| PluginError::Other {
        message: format!("{repo}: GitHub sent an unexpected answer"),
    })?;
    let mut newest = cursor.pending.take();
    let mut upserts = Upserts::default();
    for entry in entries.iter().take(MAX_ENTRIES) {
        if let Some(ts) = opt_str(entry, "updated_at")
            .filter(|t| plausible(t, fetched.now))
            .and_then(normalize_ts)
        {
            keep_newest(&mut newest, ts);
        }
        // The issues endpoint also lists pull requests; they have their own resource.
        if entry.get("pull_request").is_some() {
            continue;
        }
        upserts.push(item(&repo, entry));
    }
    let done = match fetched.next {
        Next::Url(next) => {
            cursor.continue_repo(next, newest);
            false
        }
        Next::Refused => return Err(refused_next(&repo)),
        Next::None => cursor.finish_repo(&repo, repos.len(), newest),
    };
    Ok(SyncPage {
        upserts: upserts.items,
        deletes: Vec::new(),
        next_cursor: Some(cursor.encode()?),
        done,
        snapshot_complete: false,
    })
}
