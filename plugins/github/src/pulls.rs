//! The `pull_request` resource: newest update first, stopping at the mark.

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
            "draft": entry.get("draft").cloned().unwrap_or(Value::Null),
            "merged_at": entry.get("merged_at").cloned().unwrap_or(Value::Null),
            "author": entry.get("user").and_then(|u| opt_str(u, "login")),
            "labels": labels(entry),
            "assignees": logins(entry, "assignees"),
            "requested_reviewers": logins(entry, "requested_reviewers"),
            "head_ref": entry.get("head").and_then(|h| opt_str(h, "ref")),
            "base_ref": entry.get("base").and_then(|b| opt_str(b, "ref")),
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
    let url = cursor.next.clone().unwrap_or_else(|| {
        format!(
            "{}repos/{repo}/pulls?state=all&sort=updated&direction=desc&per_page=100",
            http::API_PREFIX
        )
    });
    let fetched = http::get(host, &url, &repo)?;
    let entries = fetched.body.as_array().ok_or_else(|| PluginError::Other {
        message: format!("{repo}: GitHub sent an unexpected answer"),
    })?;
    let mark = cursor.mark(&repo).map(str::to_string);
    let mut newest = cursor.pending.take();
    let mut upserts = Vec::new();
    let mut reached_mark = false;
    for entry in entries {
        let Some(ts) = opt_str(entry, "updated_at").and_then(normalize_ts) else {
            continue;
        };
        if mark.as_deref().is_some_and(|m| ts.as_str() <= m) {
            reached_mark = true;
            break;
        }
        keep_newest(&mut newest, ts);
        upserts.extend(item(&repo, entry));
    }
    let next = if reached_mark { None } else { fetched.next };
    let done = match next {
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
