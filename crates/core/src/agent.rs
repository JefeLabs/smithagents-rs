//! Agent, repository and chat domain types, plus naming helpers.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, IntentId, MessageId, RepoId};
pub use crate::palette::next_color;
use crate::palette::AgentColor;

pub const PLANETS: [&str; 8] = [
    "Mercury", "Venus", "Earth", "Mars", "Jupiter", "Saturn", "Uranus", "Neptune",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Working,
    Quiet,
    Done,
    Blocked,
    Waiting,
    Stopped,
    Dead,
}

/// The source control a repository is under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceControl {
    Git { default_branch: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub id: RepoId,
    pub path: PathBuf,
    /// `None`: a plain folder.
    pub source_control: Option<SourceControl>,
    pub created_at: DateTime<Utc>,
}

impl Repo {
    /// The branch agents start from; `None` for a plain folder.
    pub fn default_branch(&self) -> Option<&str> {
        match &self.source_control {
            Some(SourceControl::Git { default_branch }) => Some(default_branch),
            None => None,
        }
    }

    /// Whether this repository is a plain folder, with no source control.
    pub fn is_plain(&self) -> bool {
        self.source_control.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub color: AgentColor,
    pub repo_id: RepoId,
    /// The folder the agent works in: its worktree, or for an agent on a
    /// plain repository the repository's own folder.
    pub worktree_path: PathBuf,
    /// `None`: the agent works in place, on a plain repository.
    pub branch: Option<String>,
    pub harness_session_id: Option<String>,
    pub state: AgentState,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// The intent this agent was started for, if any.
    #[serde(default)]
    pub intent_id: Option<IntentId>,
}

impl Agent {
    /// Whether the agent works in its repository's own folder, on no branch.
    pub fn works_in_place(&self) -> bool {
        self.branch.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub agent_id: AgentId,
    pub seq: u64,
    pub role: Role,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// First name in `pool` not in `open`. When all are taken, appends ` 2`,
/// ` 3`, ... at the first suffix level that has a free name.
pub fn next_agent_name(pool: &[&str], open: &HashSet<String>) -> String {
    for level in 1u32.. {
        for base in pool {
            let name = if level == 1 {
                (*base).to_string()
            } else {
                format!("{base} {level}")
            };
            if !open.contains(&name) {
                return name;
            }
        }
    }
    unreachable!("an empty pool is a caller bug")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::palette;
    use serde_json::{json, Value};
    use std::collections::HashSet;

    fn open(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn next_agent_name_takes_first_unused() {
        assert_eq!(
            next_agent_name(&PLANETS, &open(&["Mercury", "Venus"])),
            "Earth"
        );
    }

    #[test]
    fn next_agent_name_fills_gaps() {
        assert_eq!(next_agent_name(&PLANETS, &open(&["Venus"])), "Mercury");
    }

    #[test]
    fn next_agent_name_exhausted_appends_suffix() {
        assert_eq!(next_agent_name(&PLANETS, &open(&PLANETS)), "Mercury 2");
        let mut names = open(&PLANETS);
        names.insert("Mercury 2".into());
        assert_eq!(next_agent_name(&PLANETS, &names), "Venus 2");
    }

    #[test]
    fn agent_serde_round_trip() {
        let agent = Agent {
            id: AgentId::new(),
            name: "Earth".into(),
            color: palette()[0],
            repo_id: RepoId::new(),
            worktree_path: "/tmp/wt".into(),
            branch: Some("agent/earth".into()),
            harness_session_id: Some("s1".into()),
            state: AgentState::Working,
            created_at: chrono::Utc::now(),
            closed_at: None,
            intent_id: None,
        };
        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("\"working\""));
        let back: Agent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, agent);
    }

    fn repo(source_control: Option<SourceControl>) -> Repo {
        Repo {
            id: RepoId::new(),
            path: "/r".into(),
            source_control,
            created_at: chrono::Utc::now(),
        }
    }

    fn agent(branch: Option<&str>) -> Agent {
        Agent {
            id: AgentId::new(),
            name: "Earth".into(),
            color: palette()[0],
            repo_id: RepoId::new(),
            worktree_path: "/r".into(),
            branch: branch.map(Into::into),
            harness_session_id: None,
            state: AgentState::Idle,
            created_at: chrono::Utc::now(),
            closed_at: None,
            intent_id: None,
        }
    }

    #[test]
    fn a_repository_says_its_source_control_on_the_wire() {
        let plain = repo(None);
        let git = repo(Some(SourceControl::Git {
            default_branch: "main".into(),
        }));
        assert_eq!(
            serde_json::to_value(&plain).unwrap()["source_control"],
            Value::Null
        );
        assert_eq!(
            serde_json::to_value(&git).unwrap()["source_control"],
            json!({"git": {"default_branch": "main"}})
        );
        assert_eq!((plain.default_branch(), plain.is_plain()), (None, true));
        assert_eq!(
            (git.default_branch(), git.is_plain()),
            (Some("main"), false)
        );
        for r in [plain, git] {
            let back: Repo = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn an_agent_that_works_in_place_has_no_branch() {
        let json = serde_json::to_value(agent(None)).unwrap();
        assert_eq!(json["branch"], Value::Null);
        assert_eq!(serde_json::from_value::<Agent>(json).unwrap().branch, None);
        assert!(agent(None).works_in_place() && !agent(Some("agent/x")).works_in_place());
    }

    #[test]
    fn agent_without_intent_id_still_deserializes() {
        let json = r#"{"id":"6f1d3a52-8c4e-4b7a-9e21-0d5c7f3b9a10","name":"Mercury","color":{"l":0.5,"c":0.25,"h":180.0},"repo_id":"2b7e9c41-5d3a-4f68-8a1c-7e4b0d9f2c35","worktree_path":"/tmp/wt","branch":"b","harness_session_id":null,"state":"idle","created_at":"2026-11-24T09:00:00Z","closed_at":null}"#;
        let agent: Agent = serde_json::from_str(json).unwrap();
        assert_eq!(agent.intent_id, None);
    }
}
