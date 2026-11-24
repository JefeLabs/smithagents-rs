//! Parsing of the Claude CLI's `--output-format json` result object.

use serde_json::Value;

use super::{HarnessError, StructuredResult};

/// Parse the CLI's stdout: the last JSON object wins, noise lines before it
/// are ignored. `is_error: true` is `Failed`; success needs `structured_output`.
pub fn parse_cli_result(stdout: &str) -> Result<StructuredResult, HarnessError> {
    let obj = last_json_object(stdout)
        .ok_or_else(|| HarnessError::InvalidOutput("no JSON object in claude output".into()))?;
    if obj.get("is_error").and_then(Value::as_bool) == Some(true) {
        let msg = obj
            .get("result")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("claude reported an error");
        return Err(HarnessError::Failed(msg.to_owned()));
    }
    match obj.get("structured_output") {
        Some(v) if !v.is_null() => Ok(StructuredResult {
            value: v.clone(),
            cost_usd: obj.get("total_cost_usd").and_then(Value::as_f64),
        }),
        _ => Err(HarnessError::InvalidOutput(
            "claude result has no structured_output".into(),
        )),
    }
}

fn last_json_object(stdout: &str) -> Option<Value> {
    let is_obj = |v: &Value| v.is_object();
    if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
        if is_obj(&v) {
            return Some(v);
        }
    }
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(is_obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_verified_shape() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"title\":\"probe\"}","structured_output":{"title":"probe","count":2},"total_cost_usd":0.007,"session_id":"s"}"#;
        let r = parse_cli_result(out).unwrap();
        assert_eq!(r.value, serde_json::json!({"title":"probe","count":2}));
        assert_eq!(r.cost_usd, Some(0.007));
    }

    #[test]
    fn is_error_is_failed_with_result_text() {
        let out = r#"{"is_error":true,"result":"boom","structured_output":{"a":1}}"#;
        assert!(matches!(parse_cli_result(out), Err(HarnessError::Failed(m)) if m == "boom"));
    }

    #[test]
    fn missing_structured_output_is_invalid() {
        let out = r#"{"is_error":false,"result":"{\"a\":1}"}"#;
        assert!(matches!(
            parse_cli_result(out),
            Err(HarnessError::InvalidOutput(_))
        ));
    }

    #[test]
    fn noise_before_the_object_is_ignored() {
        let out = "warming up\nnot json {\n{\"is_error\":false,\"structured_output\":{\"a\":1}}\n";
        let r = parse_cli_result(out).unwrap();
        assert_eq!(r.value, serde_json::json!({"a":1}));
        assert_eq!(r.cost_usd, None);
    }

    #[test]
    fn empty_stdout_is_invalid() {
        assert!(matches!(
            parse_cli_result(""),
            Err(HarnessError::InvalidOutput(_))
        ));
        assert!(matches!(
            parse_cli_result("  \n"),
            Err(HarnessError::InvalidOutput(_))
        ));
    }
}
