//! Plugin status and resource item types served by the API and carried by events.

use std::collections::BTreeMap;

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
    /// A command-line tool the plugin depends on is missing or too old, or
    /// (for an installed plugin) its tools are not approved.
    NeedsSetup {
        tools: Vec<MissingTool>,
        message: String,
    },
    Failed {
        message: String,
    },
    LoadFailed {
        message: String,
    },
}

/// A tool a plugin needs, and how the user can get it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingTool {
    /// The executable's name, such as `gh`.
    pub name: String,
    pub label: String,
    /// Why it cannot be used, such as `not installed`.
    pub reason: String,
    /// A page that explains how to install it.
    pub install_url: Option<String>,
    /// Installer name to package spec, such as `brew` to `gh`. The daemon can
    /// install a tool with a `mise` entry when asked to.
    pub install: BTreeMap<String, String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_setup_on_the_wire() {
        let state = PluginState::NeedsSetup {
            tools: vec![MissingTool {
                name: "gh".into(),
                label: "GitHub CLI".into(),
                reason: "not installed".into(),
                install_url: Some("https://cli.github.com".into()),
                install: [("brew".to_string(), "gh".to_string())].into(),
            }],
            message: "needs GitHub CLI (gh): not installed".into(),
        };
        let json = r#"{"kind":"needs_setup","tools":[{"name":"gh","label":"GitHub CLI","reason":"not installed","install_url":"https://cli.github.com","install":{"brew":"gh"}}],"message":"needs GitHub CLI (gh): not installed"}"#;
        assert_eq!(serde_json::to_string(&state).unwrap(), json);
        assert_eq!(serde_json::from_str::<PluginState>(json).unwrap(), state);
    }
}
