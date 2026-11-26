//! The `repository` resource: one repository per call.

use serde_json::{json, Value};
use smithagents_plugin_protocol::{HostApi, Item, PluginError, SyncPage};

use crate::cursor::{normalize_ts, Cursor};
use crate::{http, opt_str, parse_time, truncate_title, web_url};

fn item(repo: &str, body: &Value) -> Item {
    let updated = opt_str(body, "updated_at");
    Item {
        id: repo.to_string(),
        title: truncate_title(repo),
        url: web_url(body),
        updated_at: updated.and_then(parse_time),
        version: updated.and_then(normalize_ts),
        fields: json!({
            "description": body.get("description").cloned().unwrap_or(Value::Null),
            "private": body.get("private").cloned().unwrap_or(Value::Null),
            "default_branch": body.get("default_branch").cloned().unwrap_or(Value::Null),
            "open_issues_count": body.get("open_issues_count").cloned().unwrap_or(Value::Null),
            "stargazers_count": body.get("stargazers_count").cloned().unwrap_or(Value::Null),
            "pushed_at": body.get("pushed_at").cloned().unwrap_or(Value::Null),
            "archived": body.get("archived").cloned().unwrap_or(Value::Null),
        }),
    }
}

pub fn sync(
    host: &dyn HostApi,
    repos: &[String],
    raw_cursor: Option<&str>,
) -> Result<SyncPage, PluginError> {
    let mut cursor = Cursor::load(raw_cursor, repos);
    let repo = &repos[cursor.repo];
    let fetched = http::get(host, &format!("{}repos/{repo}", http::API_PREFIX), repo)?;
    if !fetched.body.is_object() {
        return Err(PluginError::Other {
            message: format!("{repo}: GitHub sent an unexpected answer"),
        });
    }
    let upserts = vec![item(repo, &fetched.body)];
    let done = cursor.finish_repo(repo, repos.len(), None);
    Ok(SyncPage {
        upserts,
        deletes: Vec::new(),
        next_cursor: Some(cursor.encode()?),
        done,
        // Every configured repository was seen since the listing began, which
        // is at the first one: the cursor only moves forward from there.
        snapshot_complete: done,
    })
}
