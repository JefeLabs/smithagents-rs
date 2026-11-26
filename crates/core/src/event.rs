//! Events broadcast by the daemon.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, ChatMessage};
use crate::ids::AgentId;
use crate::intent::Intent;
use crate::plugin::PluginStatus;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

/// What happened. On the wire: `{"type": "<snake_case kind>", "data": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    AgentCreated(Agent),
    AgentUpdated(Agent),
    AgentClosed {
        agent_id: AgentId,
    },
    MessageAppended(ChatMessage),
    TurnDelta {
        agent_id: AgentId,
        text: String,
    },
    IntentCreated(Intent),
    IntentUpdated(Intent),
    PluginUpdated(PluginStatus),
    ResourceSynced {
        plugin_id: String,
        resource: String,
        upserted: u32,
        deleted: u32,
        at: DateTime<Utc>,
    },
}

impl EventKind {
    /// The `type` of every kind this version knows. A client meeting another
    /// one (sent by a newer daemon) can skip that event. Tests check that it
    /// matches [`EventKind::type_name`] of every kind.
    pub const TYPES: [&'static str; 9] = [
        "agent_created",
        "agent_updated",
        "agent_closed",
        "message_appended",
        "turn_delta",
        "intent_created",
        "intent_updated",
        "plugin_updated",
        "resource_synced",
    ];

    /// The `type` this kind is sent with.
    pub fn type_name(&self) -> &'static str {
        match self {
            EventKind::AgentCreated(_) => "agent_created",
            EventKind::AgentUpdated(_) => "agent_updated",
            EventKind::AgentClosed { .. } => "agent_closed",
            EventKind::MessageAppended(_) => "message_appended",
            EventKind::TurnDelta { .. } => "turn_delta",
            EventKind::IntentCreated(_) => "intent_created",
            EventKind::IntentUpdated(_) => "intent_updated",
            EventKind::PluginUpdated(_) => "plugin_updated",
            EventKind::ResourceSynced { .. } => "resource_synced",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::Value;

    use super::*;
    use crate::agent::{AgentState, Role};
    use crate::ids::{MessageId, RepoId};
    use crate::intent::{Content, IntentKind, IntentState, Phase, Seed, SeedSource};
    use crate::palette::AgentColor;
    use crate::plugin::{PluginState, ResourceStatus, SettingField};

    const AGENT_ID: &str = "6f1d3a52-8c4e-4b7a-9e21-0d5c7f3b9a10";
    const REPO_ID: &str = "2b7e9c41-5d3a-4f68-8a1c-7e4b0d9f2c35";
    const MESSAGE_ID: &str = "9c3f5a17-2e8d-4b61-a4f0-3d7e1b6c8a92";

    fn time(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn agent() -> Agent {
        Agent {
            id: AGENT_ID.parse().unwrap(),
            name: "Mercury".into(),
            color: AgentColor {
                l: 0.5,
                c: 0.25,
                h: 180.0,
            },
            repo_id: REPO_ID.parse::<RepoId>().unwrap(),
            worktree_path: "/tmp/wt/agent-mercury".into(),
            branch: "agent/mercury-6f1d3a52".into(),
            harness_session_id: Some("s1".into()),
            state: AgentState::Working,
            created_at: time("2026-11-24T09:00:00Z"),
            closed_at: None,
            intent_id: None,
        }
    }

    const AGENT_JSON: &str = r#"{"id":"6f1d3a52-8c4e-4b7a-9e21-0d5c7f3b9a10","name":"Mercury","color":{"l":0.5,"c":0.25,"h":180.0},"repo_id":"2b7e9c41-5d3a-4f68-8a1c-7e4b0d9f2c35","worktree_path":"/tmp/wt/agent-mercury","branch":"agent/mercury-6f1d3a52","harness_session_id":"s1","state":"working","created_at":"2026-11-24T09:00:00Z","closed_at":null,"intent_id":null}"#;

    const INTENT_ID: &str = "4a8d2f60-1b3c-4e57-b9d4-6c0e8a2f7d13";

    fn intent() -> Intent {
        Intent {
            id: INTENT_ID.parse().unwrap(),
            repo_id: REPO_ID.parse().unwrap(),
            slug: "t".into(),
            state: IntentState::Draft,
            phase: Phase::Questions,
            seed: Seed {
                source: SeedSource::Text,
                text: "s".into(),
            },
            content: Content {
                title: "T".into(),
                kind: IntentKind::Task,
                current_behavior: String::new(),
                questions: vec![],
                preconditions: vec![],
                criteria: vec![],
                stories: vec![],
                depends_on: vec![],
            },
            agent_id: None,
            created_at: time("2026-11-24T09:00:00Z"),
            updated_at: time("2026-11-24T09:00:00Z"),
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: None,
        }
    }

    const INTENT_JSON: &str = r#"{"id":"4a8d2f60-1b3c-4e57-b9d4-6c0e8a2f7d13","repo_id":"2b7e9c41-5d3a-4f68-8a1c-7e4b0d9f2c35","slug":"t","state":"draft","phase":{"kind":"questions"},"seed":{"source":"text","text":"s"},"content":{"title":"T","kind":"task","current_behavior":"","questions":[],"preconditions":[],"criteria":[],"stories":[],"depends_on":[]},"agent_id":null,"created_at":"2026-11-24T09:00:00Z","updated_at":"2026-11-24T09:00:00Z","queued_at":null,"sent_at":null,"resolved_at":null,"last_start_error":null}"#;

    fn plugin_status() -> PluginStatus {
        PluginStatus {
            id: "github".into(),
            name: "GitHub".into(),
            version: "1.0.0".into(),
            host: "wasm".into(),
            builtin: true,
            enabled: true,
            state: PluginState::NeedsAuth {
                missing: vec!["token".into()],
                message: "add a token".into(),
            },
            hosts: vec!["api.github.com".into()],
            secrets: vec!["token".into()],
            settings: vec![SettingField {
                key: "org".into(),
                label: "Org".into(),
                kind: "string".into(),
                required: false,
            }],
            resources: vec![ResourceStatus {
                kind: "issues".into(),
                label: "Issues".into(),
                item_count: 4,
                last_run_at: Some(time("2026-11-26T08:00:00Z")),
                last_ok_at: None,
                next_due_at: None,
                last_error: None,
                syncing: false,
            }],
        }
    }

    const PLUGIN_JSON: &str = r#"{"id":"github","name":"GitHub","version":"1.0.0","host":"wasm","builtin":true,"enabled":true,"state":{"kind":"needs_auth","missing":["token"],"message":"add a token"},"hosts":["api.github.com"],"secrets":["token"],"settings":[{"key":"org","label":"Org","kind":"string","required":false}],"resources":[{"kind":"issues","label":"Issues","item_count":4,"last_run_at":"2026-11-26T08:00:00Z","last_ok_at":null,"next_due_at":null,"last_error":null,"syncing":false}]}"#;

    /// One event of every kind, with the exact JSON the daemon sends for it.
    fn every_kind() -> Vec<(EventKind, String)> {
        let message = ChatMessage {
            id: MESSAGE_ID.parse::<MessageId>().unwrap(),
            agent_id: AGENT_ID.parse().unwrap(),
            seq: 3,
            role: Role::Assistant,
            text: "Hi".into(),
            created_at: time("2026-11-24T09:30:00Z"),
        };
        vec![
            (
                EventKind::AgentCreated(agent()),
                format!(r#"{{"type":"agent_created","data":{AGENT_JSON}}}"#),
            ),
            (
                EventKind::AgentUpdated(agent()),
                format!(r#"{{"type":"agent_updated","data":{AGENT_JSON}}}"#),
            ),
            (
                EventKind::AgentClosed {
                    agent_id: AGENT_ID.parse().unwrap(),
                },
                format!(r#"{{"type":"agent_closed","data":{{"agent_id":"{AGENT_ID}"}}}}"#),
            ),
            (
                EventKind::MessageAppended(message),
                format!(
                    r#"{{"type":"message_appended","data":{{"id":"{MESSAGE_ID}","agent_id":"{AGENT_ID}","seq":3,"role":"assistant","text":"Hi","created_at":"2026-11-24T09:30:00Z"}}}}"#
                ),
            ),
            (
                EventKind::TurnDelta {
                    agent_id: AGENT_ID.parse().unwrap(),
                    text: "Hel".into(),
                },
                format!(
                    r#"{{"type":"turn_delta","data":{{"agent_id":"{AGENT_ID}","text":"Hel"}}}}"#
                ),
            ),
            (
                EventKind::IntentCreated(intent()),
                format!(r#"{{"type":"intent_created","data":{INTENT_JSON}}}"#),
            ),
            (
                EventKind::IntentUpdated(intent()),
                format!(r#"{{"type":"intent_updated","data":{INTENT_JSON}}}"#),
            ),
            (
                EventKind::PluginUpdated(plugin_status()),
                format!(r#"{{"type":"plugin_updated","data":{PLUGIN_JSON}}}"#),
            ),
            (
                EventKind::ResourceSynced {
                    plugin_id: "github".into(),
                    resource: "issues".into(),
                    upserted: 2,
                    deleted: 1,
                    at: time("2026-11-26T08:00:00Z"),
                },
                r#"{"type":"resource_synced","data":{"plugin_id":"github","resource":"issues","upserted":2,"deleted":1,"at":"2026-11-26T08:00:00Z"}}"#.into(),
            ),
        ]
    }

    #[test]
    fn every_kind_has_its_pinned_json_and_round_trips() {
        for (kind, json) in every_kind() {
            let event = Event {
                seq: 7,
                at: time("2026-11-24T10:00:00Z"),
                kind,
            };
            let expected = format!(r#"{{"seq":7,"at":"2026-11-24T10:00:00Z","kind":{json}}}"#);
            assert_eq!(serde_json::to_string(&event).unwrap(), expected);
            assert_eq!(serde_json::from_str::<Event>(&expected).unwrap(), event);
        }
    }

    #[test]
    fn types_lists_exactly_the_type_of_every_kind() {
        let mut seen = HashSet::new();
        for (kind, _) in every_kind() {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(value["type"], Value::from(kind.type_name()));
            assert!(EventKind::TYPES.contains(&kind.type_name()), "{kind:?}");
            seen.insert(kind.type_name());
        }
        assert_eq!(seen.len(), EventKind::TYPES.len());
    }

    #[test]
    fn unknown_type_does_not_decode() {
        let json = r#"{"type":"agent_renamed","data":{"agent_id":"x"}}"#;
        assert!(serde_json::from_str::<EventKind>(json).is_err());
    }
}
