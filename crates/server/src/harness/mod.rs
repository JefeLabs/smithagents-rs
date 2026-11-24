//! Agent harness abstraction: runs one conversational turn and streams events.

pub mod claude;
pub mod claude_parser;
#[cfg(feature = "test-util")]
pub mod fake;

use std::path::PathBuf;

use async_trait::async_trait;
use futures::stream::BoxStream;

/// Everything needed to run a single turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRequest {
    pub prompt: String,
    pub cwd: PathBuf,
    pub resume_session_id: Option<String>,
    pub model: Option<String>,
    pub system_prompt_append: Option<String>,
}

/// Normalized event emitted while a turn runs.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    SessionStarted {
        session_id: String,
    },
    /// A new block of assistant text begins. Text of separate blocks belongs
    /// in separate paragraphs; the deltas themselves carry no separator.
    TextBlockStarted,
    AssistantText {
        text: String,
    },
    ToolUse {
        name: String,
        summary: String,
    },
    TurnCompleted {
        session_id: String,
        is_error: bool,
        result_text: Option<String>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("failed to spawn harness process: {0}")]
    Spawn(#[from] std::io::Error),
}

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError>;
}
