//! Plugin status and resource item types served by the API and carried by events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where a plugin stands. On the wire: `{"kind": "<snake_case state>", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginState {
    Disabled,
    Ready,
    NeedsAuth {
        missing: Vec<String>,
        message: String,
    },
    NeedsSettings {
        message: String,
    },
    Failed {
        message: String,
    },
    LoadFailed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceStatus {
    pub kind: String,
    pub label: String,
    pub item_count: u64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub syncing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub host: String,
    pub builtin: bool,
    pub enabled: bool,
    pub state: PluginState,
    pub hosts: Vec<String>,
    pub secrets: Vec<String>,
    pub settings: Vec<SettingField>,
    pub resources: Vec<ResourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceItem {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub version: Option<String>,
    pub fields: serde_json::Value,
    pub synced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsPage {
    pub items: Vec<ResourceItem>,
    pub next: Option<String>,
}
