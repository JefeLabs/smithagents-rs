//! The cursor: where a listing is, and how far each repository has been read.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use smithagents_plugin_protocol::PluginError;

use crate::http;

const VERSION: u32 = 1;
const MAX_CURSOR_BYTES: usize = 4 * 1024;

/// `{ "v": 1, "repo": <index>, "next": <url or null>, "marks": {...},
/// "pending": <timestamp or null> }`, plus `sig`, a fingerprint of the
/// configured repositories so an index is never read against another list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    v: u32,
    pub repo: usize,
    pub next: Option<String>,
    pub marks: BTreeMap<String, String>,
    pub pending: Option<String>,
    #[serde(default)]
    sig: String,
}

/// A timestamp as `YYYY-MM-DDTHH:MM:SSZ`, so two of them compare as text.
pub fn normalize_ts(raw: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(raw).ok().map(|d| {
        d.with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    })
}

/// FNV-1a over the repository names: a fingerprint, not a secret.
fn signature(repos: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in repos.iter().flat_map(|r| r.bytes().chain([0u8])) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl Cursor {
    fn fresh(repos: &[String]) -> Self {
        Cursor {
            v: VERSION,
            repo: 0,
            next: None,
            marks: BTreeMap::new(),
            pending: None,
            sig: signature(repos),
        }
    }

    /// Read a cursor against the sorted, non-empty repository list. Anything
    /// unusable restarts from the beginning; a repository that is no longer
    /// configured loses its mark.
    pub fn load(raw: Option<&str>, repos: &[String]) -> Self {
        let Some(mut cur) = raw
            .and_then(|r| serde_json::from_str::<Cursor>(r).ok())
            .filter(|c| c.v == VERSION)
        else {
            return Self::fresh(repos);
        };
        cur.marks
            .retain(|k, v| repos.contains(k) && normalize_ts(v).as_deref() == Some(v.as_str()));
        let sig = signature(repos);
        if cur.sig != sig || cur.repo >= repos.len() {
            cur.repo = 0;
            cur.next = None;
            cur.pending = None;
            cur.sig = sig;
        }
        if cur.next.as_deref().is_some_and(|n| !http::is_api_url(n)) {
            cur.next = None;
            cur.pending = None;
        }
        if cur
            .pending
            .as_deref()
            .is_some_and(|p| normalize_ts(p).as_deref() != Some(p))
        {
            cur.pending = None;
        }
        cur
    }

    pub fn mark(&self, repo: &str) -> Option<&str> {
        self.marks.get(repo).map(String::as_str)
    }

    /// Stay on this repository: more pages follow at `next`.
    pub fn continue_repo(&mut self, next: String, pending: Option<String>) {
        self.next = Some(next);
        self.pending = pending;
    }

    /// This repository is exhausted: keep its newest timestamp as its mark and
    /// move on. True when that was the last repository, after which the cursor
    /// points at the first one so the next call starts a fresh listing.
    pub fn finish_repo(&mut self, repo: &str, repos_len: usize, newest: Option<String>) -> bool {
        if let Some(ts) = newest {
            let keep_old = self.marks.get(repo).is_some_and(|old| *old >= ts);
            if !keep_old {
                self.marks.insert(repo.to_string(), ts);
            }
        }
        self.next = None;
        self.pending = None;
        self.repo += 1;
        if self.repo >= repos_len {
            self.repo = 0;
            true
        } else {
            false
        }
    }

    pub fn encode(&self) -> Result<String, PluginError> {
        let text = serde_json::to_string(self).map_err(|e| PluginError::Other {
            message: format!("could not write the cursor: {e}"),
        })?;
        if text.len() > MAX_CURSOR_BYTES {
            return Err(PluginError::Other {
                message: "the sync position is too large to store; configure fewer or shorter repository names".into(),
            });
        }
        Ok(text)
    }
}

/// Keep the larger of two normalized timestamps in `slot`.
pub fn keep_newest(slot: &mut Option<String>, ts: String) {
    if slot.as_ref().is_none_or(|cur| *cur < ts) {
        *slot = Some(ts);
    }
}
