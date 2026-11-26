//! The `pull_request` resource: newest update first, stopping at the mark.

use serde_json::{json, Value};
use smithagents_plugin_protocol::{HostApi, Item, PluginError, SyncPage};

use crate::cursor::{keep_newest, normalize_ts, Cursor};
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
            "draft": scalar(entry, "draft", 0),
            "merged_at": scalar(entry, "merged_at", 1024),
            "author": nested(entry, "user", "login"),
            "labels": labels(entry),
            "assignees": logins(entry, "assignees"),
            "requested_reviewers": logins(entry, "requested_reviewers"),
            "head_ref": nested(entry, "head", "ref"),
            "base_ref": nested(entry, "base", "ref"),
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
    let mut upserts = Upserts::default();
    let mut reached_mark = false;
    for entry in entries.iter().take(MAX_ENTRIES) {
        let updated = opt_str(entry, "updated_at");
        if let Some(ts) = updated.and_then(normalize_ts) {
            // Strictly older than the mark: everything after it is too. Equal
            // entries are read again, so an update in the mark's own second
            // is never lost.
            if mark.as_deref().is_some_and(|m| ts.as_str() < m) {
                reached_mark = true;
                break;
            }
            if plausible(&ts, fetched.now) {
                keep_newest(&mut newest, ts);
            }
        }
        // Without a usable timestamp the entry is kept but moves nothing.
        upserts.push(item(&repo, entry));
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
        upserts: upserts.items,
        deletes: Vec::new(),
        next_cursor: Some(cursor.encode()?),
        done,
        snapshot_complete: false,
    })
}
