//! Per-story actor commands — one variant per I/O effect.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::story::{Story, StoryContext};
use super::CaptureAck;

/// Routing identity for every story command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoryScope {
    pub context: StoryContext,
}

impl StoryScope {
    pub fn from_context(ctx: &super::super::CallContext) -> Self {
        Self {
            context: ctx.story.clone(),
        }
    }

    pub fn route(&self) -> &crate::session::storage::CaptureRoute {
        &self.context.route
    }

    pub fn agent_id(&self) -> &str {
        &self.context.agent_id
    }
}

/// Commands handled by [`super::super::actors::StoryActor`].
///
/// Inner payloads are sent as raw JSON bytes (`Vec<u8>`) rather than `String`:
/// - `bincode` (the actor wire format) length-prefixes both, but `String`
///   forces a UTF-8 validation pass on every serialize/deserialize, while
///   `Vec<u8>` is a memcpy. We're already JSON-encoding (and thus producing
///   valid UTF-8) at the producer, so the second validation is wasted work.
/// - The receiver does `serde_json::from_slice(&bytes)` directly without
///   first reconstructing a `String`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoryCommand {
    /// Append one record to the event sink and sync live markdown.
    Restore {
        scope: StoryScope,
        record_bytes: Vec<u8>,
    },
    PersistRecord {
        scope: StoryScope,
        /// JSON-encoded [`crate::record::CaptureRecord`].
        record_bytes: Vec<u8>,
    },
    /// Legacy projection command, retained for wire compatibility. Canonical
    /// event capture currently ignores drafts; no live markdown writer is installed.
    UpsertDraft {
        scope: StoryScope,
        draft_bytes: Vec<u8>,
    },
    /// Drain mailbox before shutdown (no I/O).
    Flush,
    /// Read-model snapshot (no I/O).
    Snapshot { scope: StoryScope },
    /// Read-model snapshot without scope (shutdown / persist).
    LocalSnapshot,
}

/// Reply from [`super::super::actors::StoryActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoryReply {
    Ack(CaptureAck),
    Snapshot {
        story: Story,
    },
    LocalSnapshot {
        storage_session_id: String,
        story: Story,
    },
}

impl StoryCommand {
    pub fn persist_record(scope: StoryScope, record_bytes: Vec<u8>) -> Self {
        let record_bytes = stamp_record(record_bytes);
        Self::PersistRecord {
            scope,
            record_bytes,
        }
    }

    pub fn upsert_draft(
        scope: StoryScope,
        record_bytes: Vec<u8>,
        assistant_content: String,
    ) -> Result<Self> {
        let draft = DraftPayload {
            record_bytes,
            assistant_content,
        };
        Ok(Self::UpsertDraft {
            scope,
            draft_bytes: serde_json::to_vec(&draft)?,
        })
    }

    pub fn scope(&self) -> &StoryScope {
        match self {
            Self::Restore { scope, .. }
            | Self::PersistRecord { scope, .. }
            | Self::UpsertDraft { scope, .. }
            | Self::Snapshot { scope } => scope,
            Self::Flush | Self::LocalSnapshot => panic!("command has no scope"),
        }
    }
}

/// JSON payload for draft upsert: record template (seq assigned in StoryActor) + stream text.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DraftPayload {
    pub record_bytes: Vec<u8>,
    pub assistant_content: String,
}

fn stamp_record(bytes: Vec<u8>) -> Vec<u8> {
    // Inputs originate from typed constructors; invalid wire data is left for
    // the receiving actor to reject, rather than panic at this boundary.
    let Ok(mut record) = serde_json::from_slice::<crate::record::CaptureRecord>(&bytes) else {
        return bytes;
    };
    crate::record::ensure_timestamp(&mut record);
    if record.event_id.is_none() {
        record.event_id = Some(uuid::Uuid::new_v4().to_string());
    }
    serde_json::to_vec(&record).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::session::storage::CaptureRoute;
    use crate::sink::llm_request_summary_record;

    #[test]
    fn story_command_bincode_roundtrip() {
        let scope = StoryScope {
            context: StoryContext::from_route(
                CaptureRoute {
                    root_session: Some("run".into()),
                    session_id: "s".into(),
                    storage_session_id: "s".into(),
                    subagent_id: None,
                },
                "agent",
            ),
        };
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let rec = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            crate::sink::LlmRequestSummary {
                model: "model",
                path: "/v1/chat/completions",
                body_bytes: 12,
                protocol: "chat_completions",
                provider: "openai",
                user_content: Some("hi".into()),
                forward_to: None,
                body_json: None,
            },
            &call,
            CaptureLevel::Dialogue,
        );
        let cmd = StoryCommand::persist_record(scope, serde_json::to_vec(&rec).unwrap());
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: StoryCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, StoryCommand::PersistRecord { .. }));
    }
}
