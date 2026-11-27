//! Agent harness abstraction: runs one conversational turn and streams events.

pub mod claude;
pub mod claude_parser;
#[cfg(feature = "test-util")]
pub mod fake;
pub mod structured;

use std::path::PathBuf;
use std::time::Duration;

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
    /// The harness program could not be started, in a folder that is there:
    /// it is not installed, or cannot be run.
    #[error("failed to spawn harness process: {0}")]
    Spawn(#[source] std::io::Error),
    /// Nothing is at the path the run was to work in, or it is not a folder:
    /// a repository that was moved or deleted, a worktree that was removed.
    #[error("no working folder at {}", .0.display())]
    MissingCwd(PathBuf),
    #[error("harness run timed out")]
    Timeout,
    #[error("harness run failed: {0}")]
    Failed(String),
    #[error("invalid harness output: {0}")]
    InvalidOutput(String),
}

/// A single non-streaming run whose answer must match a JSON schema.
#[derive(Debug, Clone)]
pub struct StructuredRequest {
    pub prompt: String,
    pub cwd: PathBuf,
    pub schema: serde_json::Value,
    pub model: Option<String>,
    pub system_prompt_append: Option<String>,
    pub allowed_tools: Vec<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredResult {
    pub value: serde_json::Value,
    pub cost_usd: Option<f64>,
}

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run_turn(
        &self,
        req: TurnRequest,
    ) -> Result<BoxStream<'static, HarnessEvent>, HarnessError>;

    async fn run_structured(
        &self,
        req: StructuredRequest,
    ) -> Result<StructuredResult, HarnessError>;
}
