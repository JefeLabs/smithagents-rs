//! Agent, repository and chat domain types, plus naming helpers.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, MessageId, RepoId};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub id: RepoId,
    pub path: PathBuf,
    pub default_branch: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub color: AgentColor,
    pub repo_id: RepoId,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub harness_session_id: Option<String>,
    pub state: AgentState,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
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
            branch: "agent/earth".into(),
            harness_session_id: Some("s1".into()),
            state: AgentState::Working,
            created_at: chrono::Utc::now(),
            closed_at: None,
        };
        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("\"working\""));
        let back: Agent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, agent);
    }
}
