//! Typed per-story commands — one variant per I/O effect.

use super::super::story::{Story, StoryContext};

/// Routing identity for every story command.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Reply from [`super::super::actors::StoryActor`].
#[derive(Debug, Clone)]
pub(crate) enum StoryReply {
    Ack,
    Snapshot {
        story: Story,
    },
    LocalSnapshot {
        storage_session_id: String,
        story: Story,
    },
}

/// Typed commands for the in-process scheduling owner.
#[derive(Debug, Clone)]
pub(crate) enum LocalStoryCommand {
    Restore {
        scope: StoryScope,
        record: crate::record::CaptureRecord,
    },
    PersistRecord {
        scope: StoryScope,
        record: crate::record::CaptureRecord,
    },
    Flush,
    Snapshot {
        scope: StoryScope,
    },
    LocalSnapshot,
}

impl LocalStoryCommand {
    pub fn persist_record(scope: StoryScope, mut record: crate::record::CaptureRecord) -> Self {
        crate::record::ensure_timestamp(&mut record);
        if record.event_id.is_none() {
            record.event_id = Some(uuid::Uuid::new_v4().to_string());
        }
        Self::PersistRecord { scope, record }
    }

    pub fn scope(&self) -> &StoryScope {
        match self {
            Self::Restore { scope, .. }
            | Self::PersistRecord { scope, .. }
            | Self::Snapshot { scope } => scope,
            Self::Flush | Self::LocalSnapshot => panic!("command has no scope"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::session::storage::CaptureRoute;
    use crate::sink::llm_request_summary_record;

    #[test]
    fn typed_persistence_stamps_once_and_preserves_record() {
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
        let mut rec = rec;
        rec.event_id = None;
        rec.timestamp = None;
        let expected_scope = scope.clone();
        let local = LocalStoryCommand::persist_record(scope, rec.clone());
        let LocalStoryCommand::PersistRecord { scope, record } = local else {
            panic!("expected typed persistence command");
        };
        assert_eq!(scope, expected_scope);
        assert_eq!(record.kind, rec.kind);
        assert_eq!(record.call_id, rec.call_id);
        assert_eq!(record.trace_id, rec.trace_id);
        assert_eq!(record.payload, rec.payload);
        assert!(record.event_id.is_some());
        assert!(record.timestamp.is_some());
        let identity = record.event_id.clone();
        let timestamp = record.timestamp.clone();
        let local = LocalStoryCommand::persist_record(scope, record);
        let LocalStoryCommand::PersistRecord { record, .. } = local else {
            unreachable!()
        };
        assert_eq!(
            record.event_id, identity,
            "typed stamping must preserve retry identity"
        );
        assert_eq!(record.timestamp, timestamp);
    }
}
