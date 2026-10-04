//! Markdown eligibility rules for [`CaptureRecord`] (storage layer only).

use crate::dialogue_extract::is_subagent_shape_payload;
use crate::record::{CaptureRecord, CaptureRecordExt};

pub fn should_skip_record(rec: &CaptureRecord) -> bool {
    match rec.kind.as_str() {
        "llm.request" => {
            if rec.is_internal_llm_request() {
                return true;
            }
            if should_skip_main_flash_companion_request(rec) {
                return true;
            }
            rec.visible_user_text().is_none()
        }
        "llm.response" | "llm.response.stream" => {
            if rec.payload.get("stream_partial").and_then(|v| v.as_bool()) == Some(true) {
                return true;
            }
            rec.visible_assistant_text().is_none()
        }
        "llm.spawn_link" => false,
        "llm.call.cancelled" => true,
        k if k.starts_with("session.") => true,
        _ => false,
    }
}

pub fn should_refresh_frontmatter(rec: &CaptureRecord) -> bool {
    matches!(
        rec.kind.as_str(),
        "llm.request" | "llm.response" | "llm.response.stream"
    )
}

fn should_skip_main_flash_companion_request(rec: &CaptureRecord) -> bool {
    if rec.subagent_id.is_some() {
        return false;
    }
    let model = rec
        .payload
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if !model.contains("flash") && !model.contains("haiku") {
        return false;
    }
    if rec.visible_user_text().is_none() {
        return false;
    }
    !is_subagent_shape_payload(&rec.payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::sink::{llm_request_summary_record, llm_response_record_with_content};
    use serde_json::json;

    fn test_call() -> Call {
        Call {
            call_id: "call-test".into(),
            trace_id: "trace-test".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    const LEVEL: CaptureLevel = CaptureLevel::Dialogue;

    #[test]
    fn skip_internal_suggestion_request_and_silent_response() {
        let req = llm_request_summary_record(Some("s".into()),
None,
crate::sink::LlmRequestSummary { model: "m", path: "/v1/messages", body_bytes: 100, protocol: "messages", provider: "anthropic", user_content: None, forward_to: None, body_json: None },
&test_call(),
LEVEL,);
        assert!(should_skip_record(&req));
        let resp = llm_response_record_with_content(Some("s".into()),
None,
crate::sink::LlmResponseContent { status: 200, payload: &json!({"body": "event: x\ndata: {}\n"}), streaming: true, assistant_content: Some(String::new()) },
&test_call(),
LEVEL,);
        assert!(should_skip_record(&resp));
    }

    #[test]
    fn skip_count_tokens_request() {
        let req = llm_request_summary_record(Some("s".into()),
None,
crate::sink::LlmRequestSummary { model: "m", path: "/v1/messages/count_tokens", body_bytes: 1000, protocol: "count_tokens", provider: "anthropic", user_content: Some("huge context".into()), forward_to: None, body_json: None },
&test_call(),
LEVEL,);
        assert!(should_skip_record(&req));
    }

    #[test]
    fn skip_main_flash_companion_user_duplicate() {
        let mut rec = llm_request_summary_record(Some("sess".into()),
Some("proxy".into()),
crate::sink::LlmRequestSummary { model: "deepseek-v4-flash", path: "/v1/messages", body_bytes: 100, protocol: "messages", provider: "anthropic", user_content: Some("再次开三个subagent".into()), forward_to: None, body_json: None },
&test_call(),
LEVEL,);
        assert!(should_skip_record(&rec));
        rec.subagent_id = Some("abc".into());
        assert!(!should_skip_record(&rec));
    }
}
