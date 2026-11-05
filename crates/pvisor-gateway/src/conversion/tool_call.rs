//! Shared tool-call helpers (moon-bridge Core `tool_use` / `tool_result` subset).

use serde_json::Value;

/// Normalize tool arguments for Chat Completions (must be a JSON object string).
pub fn normalize_tool_arguments_string(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }
    "{}".to_string()
}

/// Preserve a streaming `function.arguments` fragment after the SSE JSON decode.
/// A fragment may itself look like a quoted JSON string; decoding it again would
/// remove quotes that belong to the assembled arguments.
pub fn decode_stream_arguments_delta(raw: &str) -> String {
    raw.to_string()
}

/// Unwrap double-encoded tool arguments from a non-streaming Chat message.
pub fn unquote_chat_tool_arguments(raw: &Value) -> String {
    match raw {
        Value::String(s) => normalize_tool_arguments_string(s),
        Value::Null => "{}".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_stream_arguments_delta_passthrough() {
        assert_eq!(decode_stream_arguments_delta("{\"cmd\""), "{\"cmd\"");
    }
}
