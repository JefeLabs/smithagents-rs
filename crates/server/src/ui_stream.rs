//! Encoder for the Vercel AI SDK "UI message stream" (SSE) wire format.

use serde::Serialize;

/// Response header that marks a UI message stream.
pub const STREAM_HEADER: (&str, &str) = ("x-vercel-ai-ui-message-stream", "v1");

/// Terminal sentinel event.
pub const DONE_SSE: &str = "data: [DONE]\n\n";

/// One UI message stream chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Chunk {
    Start {
        #[serde(rename = "messageId")]
        message_id: String,
    },
    StartStep,
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        delta: String,
    },
    TextEnd {
        id: String,
    },
    FinishStep,
    Finish,
    Error {
        #[serde(rename = "errorText")]
        error_text: String,
    },
}

impl Chunk {
    /// Encode as one SSE event: `data: {json}\n\n`. JSON escaping guarantees
    /// no raw newline inside the data line.
    pub fn to_sse(&self) -> String {
        let json = serde_json::to_string(self).expect("chunk serialization is infallible");
        format!("data: {json}\n\n")
    }
}

/// Canonical stream used by the golden fixture and the web contract test.
pub fn golden_transcript() -> String {
    let chunks = [
        Chunk::Start {
            message_id: "msg_golden".into(),
        },
        Chunk::StartStep,
        Chunk::TextStart { id: "t1".into() },
        Chunk::TextDelta {
            id: "t1".into(),
            delta: "Hello".into(),
        },
        Chunk::TextDelta {
            id: "t1".into(),
            delta: " \"world\"\nCaf\u{e9}".into(),
        },
        Chunk::TextEnd { id: "t1".into() },
        Chunk::FinishStep,
        Chunk::Finish,
    ];
    let mut out: String = chunks.iter().map(Chunk::to_sse).collect();
    out.push_str(DONE_SSE);
    out
}
