//! Events broadcast by the daemon.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, ChatMessage};
use crate::ids::AgentId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    AgentCreated(Agent),
    AgentUpdated(Agent),
    AgentClosed(AgentId),
    MessageAppended(ChatMessage),
    TurnDelta { agent_id: AgentId, text: String },
}
