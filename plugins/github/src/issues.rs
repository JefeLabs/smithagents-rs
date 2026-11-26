//! The `issue` resource: oldest update first, resumable with `since`.

use serde_json::{json, Value};
use smithagents_plugin_protocol::{HostApi, Item, PluginError, SyncPage};

use crate::cursor::{keep_newest, normalize_ts, Cursor};
use crate::{http, labels, logins, opt_str, parse_time, truncate_title, web_url};

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
            "state": entry.get("state").cloned().unwrap_or(Value::Null),
            "state_reason": entry.get("state_reason").cloned().unwrap_or(Value::Null),
            "author": entry.get("user").and_then(|u| opt_str(u, "login")),
            "labels": labels(entry),
            "assignees": logins(entry, "assignees"),
            "comments": entry.get("comments").cloned().unwrap_or(Value::Null),
            "created_at": entry.get("created_at").cloned().unwrap_or(Value::Null),
            "closed_at": entry.get("closed_at").cloned().unwrap_or(Value::Null),
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
                "{}repos/{repo}/issues?state=all&sort=updated&direction=asc&per_page=100",
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
    let mut upserts = Vec::new();
    for entry in entries {
        if let Some(ts) = opt_str(entry, "updated_at").and_then(normalize_ts) {
            keep_newest(&mut newest, ts);
        }
        // The issues endpoint also lists pull requests; they have their own resource.
        if entry.get("pull_request").is_some() {
            continue;
        }
        upserts.extend(item(&repo, entry));
    }
    let done = match fetched.next {
        Some(next) => {
            cursor.continue_repo(next, newest);
            false
        }
        None => cursor.finish_repo(&repo, repos.len(), newest),
    };
    Ok(SyncPage {
        upserts,
        deletes: Vec::new(),
        next_cursor: Some(cursor.encode()?),
        done,
        snapshot_complete: false,
    })
}
