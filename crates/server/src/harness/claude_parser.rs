//! Parser for `claude -p --output-format stream-json --verbose
//! --include-partial-messages` lines.

use serde_json::Value;

use super::HarnessEvent;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

const SUMMARY_MAX_CHARS: usize = 80;

/// Parse one stream-json line into at most one event.
///
/// Whole-message `assistant` text blocks are ignored because the same text
/// already arrived as `text_delta` stream events (with
/// `--include-partial-messages`); mapping both would double-count. The start
/// of each text block is reported, so separate blocks can be kept apart.
/// Stream events of sub-agents (a non-null `parent_tool_use_id`) are ignored.
/// If one
/// `assistant` message holds several `tool_use` blocks only the first is
/// returned here; use [`parse_events`] to get all of them.
pub fn parse_line(line: &str) -> Result<Option<HarnessEvent>, ParseError> {
    Ok(parse_events(line)?.into_iter().next())
}

/// Like [`parse_line`] but returns every event the line yields.
pub fn parse_events(line: &str) -> Result<Vec<HarnessEvent>, ParseError> {
    let v: Value = serde_json::from_str(line)?;
    let str_at = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let events = match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            str_at(&v, "session_id")
                .map(|session_id| HarnessEvent::SessionStarted { session_id })
                .into_iter()
                .collect()
        }
        // A sub-agent (Task tool) streams its own messages, marked with the id
        // of the tool call that started it; they are not this agent's reply.
        Some("stream_event") if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) => {
            Vec::new()
        }
        Some("stream_event") => {
            let event = v.get("event");
            match type_of(event) {
                Some("content_block_start")
                    if type_of(event.and_then(|e| e.get("content_block"))) == Some("text") =>
                {
                    vec![HarnessEvent::TextBlockStarted]
                }
                Some("content_block_delta") => {
                    let delta = event.and_then(|e| e.get("delta"));
                    match delta.and_then(|d| str_at(d, "text")) {
                        Some(text) if type_of(delta) == Some("text_delta") => {
                            vec![HarnessEvent::AssistantText { text }]
                        }
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            }
        }
        Some("assistant") => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .filter_map(|b| {
                        Some(HarnessEvent::ToolUse {
                            name: str_at(b, "name")?,
                            summary: b.get("input").map(summarize_input).unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("result") => match str_at(&v, "session_id") {
            Some(session_id) => vec![HarnessEvent::TurnCompleted {
                session_id,
                is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                result_text: str_at(&v, "result"),
            }],
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    Ok(events)
}

/// The `type` field of an optional JSON object.
fn type_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(|v| v.get("type")).and_then(Value::as_str)
}

/// Short human-readable description of a tool call's input.
fn summarize_input(input: &Value) -> String {
    const KEYS: [&str; 6] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "description",
    ];
    let text = KEYS
        .iter()
        .find_map(|k| input.get(k).and_then(Value::as_str))
        .unwrap_or_default();
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(SUMMARY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "e878731e-acb9-4f03-83cb-9e255971ab46";

    #[test]
    fn parses_system_init_to_session_started() {
        let line =
            format!(r#"{{"type":"system","subtype":"init","session_id":"{SID}","tools":[]}}"#);
        assert_eq!(
            parse_line(&line).unwrap(),
            Some(HarnessEvent::SessionStarted {
                session_id: SID.into()
            })
        );
    }

    #[test]
    fn non_init_system_events_are_ignored() {
        let line = format!(r#"{{"type":"system","subtype":"status","session_id":"{SID}"}}"#);
        assert_eq!(parse_line(&line).unwrap(), None);
    }

    #[test]
    fn parses_text_delta_to_assistant_text() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi! How"}}}"#;
        assert_eq!(
            parse_line(line).unwrap(),
            Some(HarnessEvent::AssistantText {
                text: "Hi! How".into()
            })
        );
    }

    #[test]
    fn text_block_start_marks_a_block_boundary() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}"#;
        assert_eq!(
            parse_line(line).unwrap(),
            Some(HarnessEvent::TextBlockStarted)
        );
    }

    #[test]
    fn non_text_block_starts_are_ignored() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"Read","input":{}}}}"#;
        assert_eq!(parse_line(line).unwrap(), None);
    }

    #[test]
    fn sub_agent_stream_events_are_ignored() {
        // A sub-agent (Task tool) streams its own messages, marked with the id
        // of the tool call that started it; they are not the agent's reply.
        for event in [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"sub"}}"#,
        ] {
            let line = format!(
                r#"{{"type":"stream_event","event":{event},"parent_tool_use_id":"toolu_01","session_id":"{SID}"}}"#
            );
            assert_eq!(parse_line(&line).unwrap(), None, "{line}");
        }
        // The agent's own deltas carry a null parent.
        let own = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"own"}},"parent_tool_use_id":null}"#;
        assert_eq!(
            parse_line(own).unwrap(),
            Some(HarnessEvent::AssistantText { text: "own".into() })
        );
    }

    #[test]
    fn non_text_deltas_are_ignored() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert_eq!(parse_line(line).unwrap(), None);
    }

    #[test]
    fn assistant_text_blocks_are_ignored_to_avoid_double_counting() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi!"}]}}"#;
        assert_eq!(parse_line(line).unwrap(), None);
    }

    #[test]
    fn parses_tool_use_block_to_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la","description":"list"}}]}}"#;
        assert_eq!(
            parse_line(line).unwrap(),
            Some(HarnessEvent::ToolUse {
                name: "Bash".into(),
                summary: "ls -la".into()
            })
        );
    }

    #[test]
    fn tool_use_summary_is_truncated() {
        let long = "x".repeat(500);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{long}"}}}}]}}}}"#
        );
        match parse_line(&line).unwrap() {
            Some(HarnessEvent::ToolUse { summary, .. }) => {
                assert!(summary.chars().count() <= 81, "{}", summary.len());
                assert!(summary.starts_with("xxx"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_use_without_input_has_empty_summary() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task"}]}}"#;
        assert_eq!(
            parse_line(line).unwrap(),
            Some(HarnessEvent::ToolUse {
                name: "Task".into(),
                summary: String::new()
            })
        );
    }

    #[test]
    fn parses_result_success() {
        let line = format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"result":"Hi!","session_id":"{SID}"}}"#
        );
        assert_eq!(
            parse_line(&line).unwrap(),
            Some(HarnessEvent::TurnCompleted {
                session_id: SID.into(),
                is_error: false,
                result_text: Some("Hi!".into())
            })
        );
    }

    #[test]
    fn parses_result_error() {
        let line = format!(
            r#"{{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"{SID}"}}"#
        );
        assert_eq!(
            parse_line(&line).unwrap(),
            Some(HarnessEvent::TurnCompleted {
                session_id: SID.into(),
                is_error: true,
                result_text: None
            })
        );
    }

    #[test]
    fn missing_fields_never_panic() {
        for line in [
            r#"{}"#,
            r#"{"type":"system"}"#,
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"stream_event"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#,
            r#"{"type":"assistant"}"#,
            r#"{"type":"assistant","message":{"content":"str"}}"#,
            r#"{"type":"result"}"#,
            r#"[1,2]"#,
            r#"null"#,
        ] {
            assert_eq!(parse_line(line).unwrap(), None, "{line}");
        }
    }

    #[test]
    fn unknown_event_type_returns_none() {
        assert_eq!(parse_line(r#"{"type":"brand_new"}"#).unwrap(), None);
    }

    #[test]
    fn invalid_json_returns_err() {
        assert!(matches!(
            parse_line("not json"),
            Err(ParseError::InvalidJson(_))
        ));
    }

    #[test]
    fn parser_handles_megabyte_line() {
        let text = "a".repeat(1024 * 1024);
        let line = format!(
            r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}}}"#
        );
        assert!(line.len() >= 1024 * 1024);
        assert_eq!(
            parse_line(&line).unwrap(),
            Some(HarnessEvent::AssistantText { text })
        );
    }

    #[test]
    fn tool_use_fixture_keeps_text_blocks_and_tool_call_in_order() {
        let raw = include_str!("../../tests/fixtures/claude_stream_tool_use.jsonl");
        let events: Vec<HarnessEvent> = raw
            .lines()
            .flat_map(|l| parse_events(l).expect("fixture line parses"))
            .collect();
        let text = |t: &str| HarnessEvent::AssistantText { text: t.into() };
        assert_eq!(
            events,
            [
                HarnessEvent::SessionStarted {
                    session_id: "5d1c1e5a-0f3b-4c7e-9a52-7b0f3e9d2c41".into()
                },
                HarnessEvent::TextBlockStarted,
                text("I'll check "),
                text("the file."),
                HarnessEvent::ToolUse {
                    name: "Read".into(),
                    summary: "README.md".into()
                },
                HarnessEvent::TextBlockStarted,
                text("The file is "),
                text("a short README."),
                HarnessEvent::TextBlockStarted,
                text("It has one heading."),
                HarnessEvent::TurnCompleted {
                    session_id: "5d1c1e5a-0f3b-4c7e-9a52-7b0f3e9d2c41".into(),
                    is_error: false,
                    result_text: Some("The file is a short README.\n\nIt has one heading.".into())
                },
            ]
        );
    }

    #[test]
    fn fixture_parses_to_started_then_text_then_completed() {
        let raw = include_str!("../../tests/fixtures/claude_stream_hello.jsonl");
        let events: Vec<HarnessEvent> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| parse_line(l).expect("fixture line parses"))
            .collect();
        assert!(matches!(
            events.first(),
            Some(HarnessEvent::SessionStarted { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(HarnessEvent::TurnCompleted {
                is_error: false,
                ..
            })
        ));
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                HarnessEvent::AssistantText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!text.is_empty());
        // No double counting: concatenated deltas equal the final result text.
        match events.last() {
            Some(HarnessEvent::TurnCompleted { result_text, .. }) => {
                assert_eq!(result_text.as_deref(), Some(text.as_str()));
            }
            _ => unreachable!(),
        }
    }
}
