//! Mutable dialogue projection inputs and versioned capture observations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::protocol::ProtocolKind;

/// Mutable Gateway projection input. Never append this structure to a fact log.
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub event_id: Option<String>,
    pub observed_at_unix_ms: Option<u64>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub payload: Value,
}

pub use pvisor_core::unix_now_ms;

/// Parse an RFC3339 event timestamp into Unix milliseconds.
pub fn unix_ms_from_rfc3339(timestamp: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.timestamp_millis().max(0) as u64)
}

/// Fix the observation time before delivery to actors.
pub(crate) fn ensure_timestamp(record: &mut CaptureRecord) {
    if record.timestamp.is_none() {
        record.timestamp = Some(now_rfc3339());
    }
    if record.observed_at_unix_ms.is_none() {
        record.observed_at_unix_ms = record
            .timestamp
            .as_deref()
            .and_then(unix_ms_from_rfc3339)
            .or_else(|| Some(unix_now_ms()));
    }
}

/// Capture-only interpretation of raw proxy payloads.
///
/// SSE and provider extraction belong to this dialogue projection, not the event envelope.
pub trait CaptureRecordExt {
    /// Internal traffic (e.g. `count_tokens`) — not a dialogue turn.
    fn is_internal_llm_request(&self) -> bool;

    /// Visible user text used by Capture's live dialogue projection.
    fn visible_user_text(&self) -> Option<String>;

    /// Visible assistant text used by Capture's live dialogue projection.
    fn visible_assistant_text(&self) -> Option<String>;
}

impl CaptureRecordExt for CaptureRecord {
    fn is_internal_llm_request(&self) -> bool {
        if self.kind != "llm.request" {
            return false;
        }
        if self
            .payload
            .get("protocol")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p == ProtocolKind::CountTokens.as_str())
        {
            return true;
        }
        self.payload
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|path| ProtocolKind::from_path(path) == ProtocolKind::CountTokens)
    }

    fn visible_user_text(&self) -> Option<String> {
        visible_user_from_payload(&self.payload)
    }

    fn visible_assistant_text(&self) -> Option<String> {
        visible_assistant_from_payload(&self.payload)
    }
}

/// Parse structured message `content` (string or Anthropic-style blocks).
pub(crate) fn content_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let out: Vec<_> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if out.is_empty() {
                None
            } else {
                Some(out.join("\n"))
            }
        }
        _ => None,
    }
}

fn visible_user_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("user_content").and_then(|v| v.as_str()) {
        return non_empty(s);
    }
    let messages = llm_inner_body(payload)
        .and_then(|b| b.get("messages"))
        .or_else(|| payload.get("messages"))?
        .as_array()?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user")
            && let Some(text) = msg.get("content").and_then(content_to_string)
        {
            return non_empty(&text);
        }
    }
    None
}

fn visible_assistant_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("assistant_content").and_then(|v| v.as_str()) {
        return non_empty(s);
    }
    if let Some(s) = payload.get("body").and_then(|b| b.as_str()) {
        let text = extract_assistant_turn_from_sse(s);
        if let Some(t) = non_empty(&text) {
            return Some(t);
        }
    }
    if let Some(inner) = llm_inner_body(payload) {
        if let Some(s) = inner.as_str() {
            let text = extract_assistant_turn_from_sse(s);
            if let Some(t) = non_empty(&text) {
                return Some(t);
            }
        }
        if let Some(text) = extract_assistant_text_from_json(inner) {
            return non_empty(&text);
        }
    }
    llm_inner_body(payload)
        .and_then(|b| b.get("choices"))
        .or_else(|| payload.get("body").and_then(|b| b.get("choices")))
        .or_else(|| payload.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(content_to_string)
        .or_else(|| payload.get("content").and_then(content_to_string))
        .and_then(|s| non_empty(&s))
}

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Inner LLM JSON: `payload.body` or proxy wrapper `payload.body.body`.
fn llm_inner_body(payload: &Value) -> Option<&Value> {
    let wrap = payload.get("body")?;
    if wrap.get("messages").is_some() || wrap.get("choices").is_some() {
        Some(wrap)
    } else {
        wrap.get("body")
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Versioned domain payload. The event envelope owns identity, time and order.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureObservation {
    pub story: crate::engine::StoryContext,
    pub correlation: CaptureCorrelation,
    pub content: Value,
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCorrelation {
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub parent_uuid: Option<String>,
    pub parent_agent_id: Option<String>,
    pub branch: Option<String>,
    pub parent_call_id: Option<String>,
}

impl CaptureRecord {
    pub fn into_event(
        mut self,
        story: crate::engine::StoryContext,
    ) -> anyhow::Result<pvisor_core::event::Event> {
        use pvisor_core::event::{Fact, Granularity, Level, VERSION};
        ensure_timestamp(&mut self);
        let id = self
            .event_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let trace_id = story
            .run_id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .unwrap_or_else(|| story.story_id.as_str().to_string());
        let scope = vec!["capture".into(), story.story_id.as_str().to_string()];
        let (domain, name) = self
            .kind
            .split_once('.')
            .unwrap_or(("gateway", self.kind.as_str()));
        let data = CaptureObservation {
            story,
            correlation: CaptureCorrelation {
                call_id: self.call_id,
                trace_id: self.trace_id,
                parent_uuid: self.parent_uuid,
                parent_agent_id: self.parent_agent_id,
                branch: self.branch,
                parent_call_id: self.parent_call_id,
            },
            content: crate::sink::redact_sensitive_body(&self.payload),
        };
        let event = pvisor_core::event::Event {
            version: VERSION,
            id,
            trace_id,
            producer: "pvisor-gateway".into(),
            observed_at_unix_ms: self.observed_at_unix_ms.expect("ensured time"),
            scope,
            context: None,
            operation: None,
            caused_by: vec![],
            level: Level::Info,
            granularity: Granularity::Operation,
            data: Fact::Observation {
                domain: domain.into(),
                name: format!("{domain}.{name}"),
                version: 1,
                payload: serde_json::to_value(data)?,
            },
        };
        event.validate()?;
        Ok(event)
    }
    /// Reconstruct mutable dialogue input from a committed domain observation.
    pub fn from_event(event: &pvisor_core::event::Event, _offset: u64) -> anyhow::Result<Self> {
        let pvisor_core::event::Fact::Observation {
            version: 1,
            payload,
            ..
        } = &event.data
        else {
            anyhow::bail!("not a capture observation");
        };
        let data: CaptureObservation = serde_json::from_value(payload.clone())?;
        Ok(Self {
            event_id: Some(event.id.clone()),
            observed_at_unix_ms: Some(event.observed_at_unix_ms),
            kind: event.name().to_string(),
            timestamp: chrono::DateTime::from_timestamp_millis(event.observed_at_unix_ms as i64)
                .map(|t| t.to_rfc3339()),
            session_id: Some(data.story.route.session_id),
            agent_id: Some(data.story.agent_id),
            subagent_id: data.story.route.subagent_id,
            trace_id: data.correlation.trace_id,
            call_id: data.correlation.call_id,
            parent_uuid: data.correlation.parent_uuid,
            parent_agent_id: data.correlation.parent_agent_id,
            branch: data.correlation.branch,
            parent_call_id: data.correlation.parent_call_id,
            payload: data.content,
        })
    }
}

pub fn is_capture_event(event: &pvisor_core::event::Event) -> bool {
    event.producer == "pvisor-gateway" && event.scope.first().is_some_and(|s| s == "capture")
}

pub fn capture_observation(
    event: &pvisor_core::event::Event,
) -> anyhow::Result<CaptureObservation> {
    anyhow::ensure!(is_capture_event(event), "not a capture fact");
    let pvisor_core::event::Fact::Observation {
        version: 1,
        payload,
        ..
    } = &event.data
    else {
        anyhow::bail!("unsupported capture observation version");
    };
    Ok(serde_json::from_value(payload.clone())?)
}

#[cfg(test)]
mod timestamp_tests {
    use proptest::prelude::*;
    use serde_json::Value;

    use super::{CaptureRecord, ensure_timestamp, unix_ms_from_rfc3339};

    #[test]
    fn parses_rfc3339_to_unix_milliseconds() {
        assert_eq!(
            unix_ms_from_rfc3339("2026-01-01T00:00:00Z"),
            Some(1_767_225_600_000)
        );
    }

    #[test]
    fn ensure_timestamp_backfills_both_wire_fields() {
        let mut record = CaptureRecord {
            event_id: None,
            observed_at_unix_ms: None,

            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: Value::Null,
        };
        ensure_timestamp(&mut record);
        assert!(record.timestamp.is_some());
        assert!(record.observed_at_unix_ms.is_some());
    }

    proptest! {
        #[test]
        fn ensure_timestamp_is_idempotent_for_existing_rfc3339(
            milliseconds in 0u64..=4_000_000_000_000u64
        ) {
            let timestamp = chrono::DateTime::from_timestamp_millis(milliseconds as i64)
                .expect("generated timestamp is representable")
                .to_rfc3339();
            let mut record = CaptureRecord {
                event_id: None,
            observed_at_unix_ms: None,

                kind: "llm.request".into(),
                timestamp: Some(timestamp.clone()),
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: Value::Null,
            };

            ensure_timestamp(&mut record);
            let first = record.clone();
            ensure_timestamp(&mut record);

            prop_assert_eq!(record.timestamp.as_deref(), Some(timestamp.as_str()));
            prop_assert_eq!(record.observed_at_unix_ms, Some(milliseconds));
            prop_assert_eq!(record, first);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::sink::{llm_request_record, llm_request_summary_record, llm_response_record};

    #[test]
    fn internal_request_detects_count_tokens_path() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let rec = llm_request_summary_record(Some("s".into()),
Some("a".into()),
crate::sink::LlmRequestSummary { model: "m", path: "/v1/messages/count_tokens", body_bytes: 10, protocol: "count_tokens", provider: "openai", user_content: None, forward_to: None, body_json: None },
&call,
CaptureLevel::Dialogue,);
        assert!(rec.is_internal_llm_request());
    }

    #[test]
    fn visible_user_prefers_user_content_field() {
        let rec = CaptureRecord {
            event_id: None,
            observed_at_unix_ms: None,

            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "user_content": "hello",
                "body": {"messages": [{"role": "user", "content": "ignored"}]}
            }),
        };
        assert_eq!(rec.visible_user_text().as_deref(), Some("hello"));
    }

    #[test]
    fn visible_user_reads_proxy_nested_body() {
        let req = llm_request_record(
            Some("sess".into()),
            Some("agent".into()),
            "mock-model",
            "/v1/chat/completions",
            &serde_json::json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "body": {"messages":[{"role":"user","content":"你好"}],"model":"mock-model"},
            }),
        );
        assert_eq!(req.visible_user_text().as_deref(), Some("你好"));
    }

    #[test]
    fn visible_assistant_reads_proxy_nested_body() {
        let resp = llm_response_record(
            Some("sess".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "body": {
                    "choices":[{"message":{"role":"assistant","content":"你好！"}}],
                },
            }),
            false,
            &Call {
                call_id: "c".into(),
                trace_id: "t".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
        );
        assert_eq!(resp.visible_assistant_text().as_deref(), Some("你好！"));
    }
}
