//! Failed capture events — append-only JSONL for recovery (`{storage}/.capture/dead_letter.jsonl`).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::CaptureLevel;
use crate::engine::{
    Call, CallContext, CancelEvent, CompleteEvent, DraftEvent, Event, RequestEvent,
};
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::runtime::open_private_append_file;
use crate::session::storage::CaptureRoute;
use crate::sink::{redact_sensitive_body, redact_sensitive_headers, redact_sensitive_url};
use crate::usage::StreamMetrics;

fn default_post() -> String {
    "POST".into()
}

const DEAD_LETTER_FILENAME: &str = "dead_letter.jsonl";
const TRAJECTORY_DEAD_LETTER_FILENAME: &str = "trajectory_dead_letter.jsonl";

/// Serializable call context for dead-letter replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadLetterContext {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub call: Call,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_headers: Vec<(String, String)>,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: ProviderKind,
    pub protocol: ProtocolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_peer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_meta: Option<crate::session::client::SessionClientMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_url: Option<String>,
}

/// Serializable event (wire format for JSONL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializableEvent {
    Request {
        path: String,
        #[serde(default = "default_post")]
        method: String,
        #[serde(default)]
        url: Option<String>,
        body_bytes: usize,
        user_content: Option<String>,
        body_json: Option<Value>,
        model_rewritten: bool,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
    ResponseComplete {
        status: u16,
        resp_payload: RespPayload,
        streaming: bool,
        stream_metrics: Option<StreamMetrics>,
        assistant_content: Option<String>,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
    ResponseDraft {
        status: u16,
        assistant_content: String,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        status: u16,
        bytes_received: usize,
        streaming: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RespPayload {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeadLetterEntry {
    pub timestamp: String,
    pub context: DeadLetterContext,
    pub event: SerializableEvent,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_record_json: Option<String>,
    /// Target of an already-prepared command, distinct from the source call.
    /// This is retry input, not evidence that the record committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_story: Option<crate::engine::StoryContext>,
}

pub fn dead_letter_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join(DEAD_LETTER_FILENAME)
}

pub fn trajectory_dead_letter_path(storage: &Path) -> PathBuf {
    storage
        .join(".capture")
        .join(TRAJECTORY_DEAD_LETTER_FILENAME)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrajectoryDeadLetterEntry {
    pub timestamp: String,
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session: Option<String>,
    #[serde(default)]
    pub records: Vec<pvisor_core::event::Event>,
    pub error: String,
}

impl TrajectoryDeadLetterEntry {
    pub fn decoded_records(&self) -> Result<Vec<pvisor_core::event::Event>> {
        Ok(self.records.clone())
    }
}

pub fn append_trajectory_dead_letter(
    storage: &Path,
    agent_id: &str,
    session_id: &str,
    root_session: Option<&str>,
    records: &[pvisor_core::event::Event],
    error: &str,
) -> Result<()> {
    let entry = TrajectoryDeadLetterEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        storage: storage.display().to_string(),
        agent_id: agent_id.to_string(),
        session_id: session_id.to_string(),
        root_session: root_session.map(str::to_string),
        records: records.to_vec(),
        error: error.to_string(),
    };
    let path = trajectory_dead_letter_path(storage);
    let mut file = open_private_append_file(&path)
        .with_context(|| format!("open trajectory dead letter {}", path.display()))?;
    let line = serde_json::to_string(&entry).context("serialize trajectory dead letter")?;
    writeln!(file, "{line}").context("append trajectory dead letter")?;
    Ok(())
}

pub fn read_trajectory_dead_letter_entries(
    storage: &Path,
) -> Result<Vec<TrajectoryDeadLetterEntry>> {
    let path = trajectory_dead_letter_path(storage);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read trajectory dead letter line {i}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(trimmed)
                .with_context(|| format!("parse trajectory dead letter line {i}"))?,
        );
    }
    Ok(out)
}

pub fn append_dead_letter(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &str,
    prepared_record_json: Option<String>,
) -> Result<()> {
    append_dead_letter_with_story(storage, ctx, event, error, prepared_record_json, None)
}

/// Preserve a failed prepared command without reconstructing a committed event
/// or replaying its source call through a registry that already matched the link.
pub(crate) fn append_prepared_dead_letter(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &str,
    story: &crate::engine::StoryContext,
    record: &crate::record::CaptureRecord,
) -> Result<()> {
    append_dead_letter_with_story(
        storage,
        ctx,
        event,
        error,
        Some(serde_json::to_string(record)?),
        Some(story.clone()),
    )
}

fn append_dead_letter_with_story(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &str,
    prepared_record_json: Option<String>,
    prepared_story: Option<crate::engine::StoryContext>,
) -> Result<()> {
    let mut retained_event = SerializableEvent::from_event(event);
    match &mut retained_event {
        SerializableEvent::Request {
            user_content,
            body_json,
            ..
        } => {
            if !ctx.level.includes_user_text() {
                *user_content = None;
            }
            if !ctx.level.includes_full_body() {
                *body_json = None;
            }
        }
        SerializableEvent::ResponseComplete {
            resp_payload,
            assistant_content,
            ..
        } => {
            if !ctx.level.includes_assistant_text() {
                *assistant_content = None;
            }
            if !ctx.level.includes_full_body() {
                let usage = match event {
                    Event::ResponseComplete(event) if !event.streaming => {
                        serde_json::from_slice::<Value>(&event.resp_bytes)
                            .ok()
                            .map(|body| crate::usage::extract_usage_from_response(&body))
                            .unwrap_or_default()
                    }
                    _ => Default::default(),
                };
                *resp_payload = RespPayload::Text(serde_json::json!({"usage":usage}).to_string());
            }
        }
        SerializableEvent::ResponseDraft {
            assistant_content, ..
        } => {
            if !ctx.level.includes_assistant_text() {
                assistant_content.clear();
            }
        }
        SerializableEvent::Cancelled { .. } => {}
    }
    let prepared_record_json = prepared_record_json
        .map(|raw| -> Result<String> {
            let mut record: crate::record::CaptureRecord = serde_json::from_str(&raw)?;
            crate::sink::retain_capture_content(&mut record.payload, ctx.level);
            record.payload = redact_sensitive_body(&record.payload);
            Ok(serde_json::to_string(&record)?)
        })
        .transpose()?;
    let entry = DeadLetterEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        context: DeadLetterContext::from_context(ctx),
        event: retained_event,
        error: error.to_string(),
        prepared_record_json,
        prepared_story,
    };
    let path = dead_letter_path(storage);
    let mut file = open_private_append_file(&path)
        .with_context(|| format!("open dead letter {}", path.display()))?;
    let line = serde_json::to_string(&entry).context("serialize dead letter entry")?;
    writeln!(file, "{line}").context("append dead letter")?;
    Ok(())
}

pub fn read_dead_letter_entries(storage: &Path) -> Result<Vec<DeadLetterEntry>> {
    let path = dead_letter_path(storage);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read dead letter line {i}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(trimmed).with_context(|| format!("parse dead letter line {i}"))?,
        );
    }
    Ok(out)
}

impl DeadLetterContext {
    pub fn from_context(ctx: &CallContext) -> Self {
        Self {
            route: ctx.route().clone(),
            agent_id: ctx.agent_id().to_string(),
            call: ctx.call.clone(),
            request_headers: redact_sensitive_headers(&ctx.request_headers),
            level: ctx.level,
            client_model: ctx.client_model.clone(),
            upstream_model: ctx.upstream_model.clone(),
            provider: ctx.provider,
            protocol: ctx.protocol,
            client_peer: ctx.client_peer.clone(),
            client_meta: ctx.client_meta.clone(),
            http_version: ctx.http_version.clone(),
            upstream_url: ctx.upstream_url.as_deref().map(redact_sensitive_url),
        }
    }

    pub fn to_call_context(&self) -> CallContext {
        let mut ctx = CallContext::new(
            crate::engine::StoryContext::from_route(self.route.clone(), self.agent_id.clone()),
            self.call.clone(),
            self.request_headers.clone(),
            crate::engine::CallCaptureConfig {
                level: self.level,
                client_model: self.client_model.clone(),
                upstream_model: self.upstream_model.clone(),
                provider: self.provider,
                protocol: self.protocol,
                debug_on: false,
            },
        );
        if let Some(peer) = &self.client_peer {
            ctx.attach_client(peer.clone(), self.client_meta.clone());
        } else if self.client_meta.is_some() {
            ctx.client_meta = self.client_meta.clone();
        }
        if let Some(v) = &self.http_version {
            ctx.attach_http_version(v.clone());
        }
        if let Some(u) = &self.upstream_url {
            ctx.attach_upstream_url(u.clone());
        }
        ctx
    }
}

impl SerializableEvent {
    pub fn from_event(event: &Event) -> Self {
        match event {
            Event::Request(e) => Self::Request {
                path: redact_sensitive_url(&e.path),
                method: e.method.clone(),
                url: e.url.as_deref().map(redact_sensitive_url),
                body_bytes: e.body_bytes,
                user_content: e.user_content.clone(),
                body_json: e.body_json.as_ref().map(redact_sensitive_body),
                model_rewritten: e.model_rewritten,
                headers: redact_sensitive_headers(&e.headers),
            },
            Event::ResponseComplete(e) => Self::ResponseComplete {
                status: e.status,
                resp_payload: resp_to_payload(&e.resp_bytes),
                streaming: e.streaming,
                stream_metrics: e.stream_metrics.clone(),
                assistant_content: e.assistant_content.clone(),
                headers: redact_sensitive_headers(&e.headers),
            },
            Event::ResponseDraft(e) => Self::ResponseDraft {
                status: e.status,
                assistant_content: e.assistant_content.clone(),
            },
            Event::Cancelled(e) => Self::Cancelled {
                reason: e.reason.clone(),
                status: e.status,
                bytes_received: e.bytes_received,
                streaming: e.streaming,
            },
        }
    }

    pub fn to_event(&self) -> Event {
        match self {
            Self::Request {
                path,
                method,
                url,
                body_bytes,
                user_content,
                body_json,
                model_rewritten,
                headers,
            } => Event::Request(RequestEvent {
                path: path.clone(),
                method: method.clone(),
                url: url.clone(),
                body_bytes: *body_bytes,
                user_content: user_content.clone(),
                body_json: body_json.clone(),
                // Dead-letter diagnostics retain the exact
                // client JSON. Replay reconstructs the typed request in prepare.
                semantic: None,
                model_rewritten: *model_rewritten,
                headers: headers.clone(),
            }),
            Self::ResponseComplete {
                status,
                resp_payload,
                streaming,
                stream_metrics,
                assistant_content,
                headers,
            } => Event::ResponseComplete(CompleteEvent {
                status: *status,
                resp_bytes: payload_to_bytes(resp_payload),
                streaming: *streaming,
                stream_metrics: stream_metrics.clone(),
                assistant_content: assistant_content.clone(),
                semantic: None,
                headers: headers.clone(),
            }),
            Self::ResponseDraft {
                status,
                assistant_content,
            } => Event::ResponseDraft(DraftEvent {
                status: *status,
                assistant_content: assistant_content.clone(),
            }),
            Self::Cancelled {
                reason,
                status,
                bytes_received,
                streaming,
            } => Event::Cancelled(CancelEvent {
                reason: reason.clone(),
                status: *status,
                bytes_received: *bytes_received,
                streaming: *streaming,
            }),
        }
    }
}

fn resp_to_payload(bytes: &Bytes) -> RespPayload {
    if let Ok(s) = std::str::from_utf8(bytes) {
        RespPayload::Text(crate::sink::redact_sensitive_wire_text(s))
    } else {
        RespPayload::Bytes(bytes.to_vec())
    }
}

fn payload_to_bytes(payload: &RespPayload) -> Bytes {
    match payload {
        RespPayload::Text(s) => Bytes::from(s.clone()),
        RespPayload::Bytes(b) => Bytes::from(b.clone()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterReplaySummary {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

pub async fn replay_dead_letter(
    storage: &Path,
    engine: &crate::engine::CaptureEngine,
) -> Result<DeadLetterReplaySummary> {
    let entries = read_dead_letter_entries(storage)?;
    let mut summary = DeadLetterReplaySummary {
        attempted: entries.len(),
        succeeded: 0,
        failed: 0,
    };
    for entry in entries {
        let result = if let Some(story) = entry.prepared_story {
            async {
                let raw = entry
                    .prepared_record_json
                    .context("prepared dead letter missing record")?;
                let record = serde_json::from_str(&raw).context("decode prepared dead letter")?;
                engine.apply_prepared_record(story, record).await
            }
            .await
        } else {
            let ctx = entry.context.to_call_context();
            engine.apply(&ctx, entry.event.to_event()).await
        };
        match result {
            Ok(()) => summary.succeeded += 1,
            Err(e) => {
                summary.failed += 1;
                tracing::warn!("dead letter replay failed: {e:#}");
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {

    #[test]
    fn summary_dead_letter_does_not_restore_dropped_payload_copies() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = sample_ctx(dir.path());
        ctx.level = CaptureLevel::Summary;
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: None,
            body_bytes: 10,
            user_content: Some("unique-private-prompt".into()),
            body_json: Some(serde_json::json!({"messages":["unique-private-prompt"]})),
            semantic: None,
            model_rewritten: false,
            headers: vec![],
        });
        let prepared = serde_json::json!({"kind":"llm.request","payload":{"user_content":"unique-private-prompt","llm_request":{"text":"unique-private-prompt"},"http":{"request_body":"unique-private-prompt"}}}).to_string();
        append_dead_letter(dir.path(), &ctx, &event, "queue full", Some(prepared)).unwrap();
        let persisted = std::fs::read_to_string(dead_letter_path(dir.path())).unwrap();
        assert!(!persisted.contains("unique-private-prompt"));
        assert_eq!(read_dead_letter_entries(dir.path()).unwrap().len(), 1);
    }
    use super::*;
    use crate::config::CaptureLevel;
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;

    fn sample_ctx(_dir: &Path) -> CallContext {
        CallContext::new(
            crate::engine::StoryContext::from_route(
                CaptureRoute {
                    root_session: Some("run-1".into()),
                    session_id: "sess".into(),
                    storage_session_id: "run-1".into(),
                    subagent_id: None,
                },
                "agent",
            ),
            Call {
                call_id: "c1".into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            Vec::new(),
            crate::engine::CallCaptureConfig {
                level: CaptureLevel::Dialogue,
                client_model: "m".into(),
                upstream_model: "m".into(),
                provider: ProviderKind::OpenAi,
                protocol: ProtocolKind::ChatCompletions,
                debug_on: false,
            },
        )
    }

    #[test]
    fn dead_letter_roundtrip_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx(dir.path());
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: None,
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            semantic: None,
            model_rewritten: false,
            headers: vec![],
        });
        append_dead_letter(dir.path(), &ctx, &event, "mailbox full", None).unwrap();
        let entries = read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error, "mailbox full");
        assert!(matches!(entries[0].event.to_event(), Event::Request(_)));
        assert_eq!(entries[0].context.provider, ProviderKind::OpenAi);
        assert_eq!(entries[0].context.protocol, ProtocolKind::ChatCompletions);
        assert!(
            entries[0].prepared_story.is_none(),
            "legacy entries remain source-event retries"
        );
    }

    #[test]
    fn prepared_dead_letter_retains_scope_identity_and_applies_content_limits() {
        for level in [
            CaptureLevel::Summary,
            CaptureLevel::Dialogue,
            CaptureLevel::Full,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ctx = sample_ctx(dir.path());
            ctx.level = level;
            ctx.request_headers = vec![("authorization".into(), "Bearer context-secret".into())];
            let target = ctx.story.clone();
            let record: crate::record::CaptureRecord = serde_json::from_value(serde_json::json!({
                "kind": "llm.spawn_link", "event_id": "retained-retry-id",
                "payload": {"user_content":"unique-private-prompt", "body":{"api_key":"prepared-secret"},
                    "spawn_links":[{"subagent_id":"child", "description":"unique-private-description"}]}
            })).unwrap();
            append_prepared_dead_letter(
                dir.path(),
                &ctx,
                &Event::Cancelled(CancelEvent {
                    reason: None,
                    status: 200,
                    bytes_received: 0,
                    streaming: true,
                }),
                "no receipt",
                &target,
                &record,
            )
            .unwrap();
            let serialized = std::fs::read_to_string(dead_letter_path(dir.path())).unwrap();
            assert!(!serialized.contains("context-secret"));
            assert!(!serialized.contains("prepared-secret"));
            let entries = read_dead_letter_entries(dir.path()).unwrap();
            assert_eq!(entries[0].prepared_story.as_ref(), Some(&target));
            let retained: crate::record::CaptureRecord =
                serde_json::from_str(entries[0].prepared_record_json.as_ref().unwrap()).unwrap();
            assert_eq!(retained.event_id.as_deref(), Some("retained-retry-id"));
            assert_eq!(retained.payload["spawn_links"][0]["subagent_id"], "child");
            if level == CaptureLevel::Summary {
                assert!(!serialized.contains("unique-private-prompt"));
            }
            if level.includes_full_body() {
                assert_eq!(retained.payload["body"]["api_key"], "<redacted>");
                assert!(serialized.contains("unique-private-description"));
            } else {
                assert!(retained.payload.get("body").is_none());
                assert!(!serialized.contains("unique-private-description"));
            }
        }
    }

    #[test]
    fn dead_letter_redacts_credentials_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = sample_ctx(dir.path());
        ctx.level = CaptureLevel::Full;
        ctx.request_headers = vec![
            ("authorization".into(), "Bearer context-secret".into()),
            ("x-request-id".into(), "req-safe".into()),
        ];
        ctx.upstream_url = Some("https://upstream.example/v1?key=url-secret".into());
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: Some("//gateway.example/v1?api_key=request-url-secret".into()),
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: Some(serde_json::json!({
                "api_key": "body-secret",
                "nested": {"clientSecret": "nested-secret"},
                "safe": "kept"
            })),
            semantic: None,
            model_rewritten: false,
            headers: vec![("x-goog-api-key".into(), "event-secret".into())],
        });

        append_dead_letter(dir.path(), &ctx, &event, "mailbox full", None).unwrap();

        let serialized = std::fs::read_to_string(dead_letter_path(dir.path())).unwrap();
        for secret in [
            "context-secret",
            "event-secret",
            "body-secret",
            "nested-secret",
            "url-secret",
            "request-url-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "dead letter persisted credential {secret}: {serialized}"
            );
        }
        assert!(serialized.contains("req-safe"));
        assert!(serialized.contains("kept"));
        assert!(serialized.contains("<redacted>"));

        let entries = read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries[0].context.request_headers[0].1, "<redacted>");
        let SerializableEvent::Request {
            body_json, headers, ..
        } = &entries[0].event
        else {
            panic!("expected request event")
        };
        assert_eq!(headers[0].1, "<redacted>");
        assert_eq!(body_json.as_ref().unwrap()["api_key"], "<redacted>");
    }

    #[cfg(unix)]
    #[test]
    fn dead_letter_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx(dir.path());
        let event = Event::Cancelled(CancelEvent {
            reason: None,
            status: 499,
            bytes_received: 0,
            streaming: true,
        });
        append_dead_letter(dir.path(), &ctx, &event, "cancelled", None).unwrap();

        let capture_dir = dir.path().join(".capture");
        assert_eq!(
            std::fs::metadata(&capture_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dead_letter_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn trajectory_dead_letter_roundtrip_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        append_trajectory_dead_letter(
            dir.path(),
            "agent",
            "sess",
            Some("run-1"),
            &[crate::record::CaptureRecord {
                event_id: None,
                observed_at_unix_ms: None,

                kind: "note".into(),
                timestamp: None,
                session_id: Some("sess".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"retry"}),
            }
            .into_event(crate::engine::StoryContext::from_route(
                crate::session::storage::CaptureRoute {
                    root_session: None,
                    session_id: "s".into(),
                    storage_session_id: "s".into(),
                    subagent_id: None,
                },
                "a",
            ))
            .unwrap()],
            "engine invoke failed",
        )
        .unwrap();
        let entries = read_trajectory_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error, "engine invoke failed");
        assert!(
            serde_json::to_value(&entries[0])
                .unwrap()
                .get("schema_version")
                .is_none()
        );
        assert_eq!(entries[0].decoded_records().unwrap().len(), 1);
    }

    #[test]
    fn dead_letter_context_requires_canonical_provider_and_protocol() {
        let json = r#"{
            "timestamp": "2026-01-01T00:00:00Z",
            "context": {
                "route": {
                    "root_session": "run-1",
                    "session_id": "sess",
                    "storage_session_id": "run-1",
                    "subagent_id": null
                },
                "agent_id": "agent",
                "call": {"call_id": "c1", "trace_id": "t1", "started_at": "2026-01-01T00:00:00Z"},
                "level": "dialogue",
                "client_model": "m",
                "upstream_model": "m",
                "provider": "openAi",
                "protocol": "chat_completions"
            },
            "event": {"kind": "cancelled", "status": 499, "bytes_received": 0, "streaming": true},
            "error": "test"
        }"#;
        let entry: DeadLetterEntry = serde_json::from_str(json).unwrap();
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut old = value.clone();
        old["context"]["provider"] = serde_json::json!("openai");
        assert!(serde_json::from_value::<DeadLetterEntry>(old).is_err());
        let mut old = value;
        let context = old.as_object_mut().unwrap().remove("context").unwrap();
        old["invocation"] = context;
        assert!(serde_json::from_value::<DeadLetterEntry>(old).is_err());
        assert_eq!(entry.context.provider, ProviderKind::OpenAi);
        assert_eq!(entry.context.protocol, ProtocolKind::ChatCompletions);
        let ctx = entry.context.to_call_context();
        assert_eq!(ctx.provider, ProviderKind::OpenAi);
        assert_eq!(ctx.protocol, ProtocolKind::ChatCompletions);
    }
}
