use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::Call;
use crate::config::CaptureLevel;
use crate::engine::{CallContext, CaptureEngine};
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::record::CaptureRecord;
use crate::session::index::SessionIndexStore;
use crate::session::storage::CaptureRoute;
use crate::sink::CaptureEventObserver;

pub(crate) struct RecordingSink {
    records: Mutex<Vec<CaptureRecord>>,
    next_seq: Mutex<HashMap<String, u64>>,
}

impl RecordingSink {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(Vec::new()),
            next_seq: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn drain(&self) -> Vec<CaptureRecord> {
        self.records.lock().unwrap().drain(..).collect()
    }
}

impl CaptureEventObserver for RecordingSink {
    fn observe(&self, event: &pvisor_core::event::Event) -> anyhow::Result<()> {
        let mut guard = self.next_seq.lock().unwrap();
        let data = crate::record::capture_observation(event)?;
        let next = guard.entry(data.story.route.seq_key()).or_insert(0);
        self.records
            .lock()
            .unwrap()
            .push(CaptureRecord::from_event(event, *next)?);
        *next += 1;
        Ok(())
    }
}

pub(crate) fn test_context() -> CallContext {
    CallContext::new(
        crate::engine::StoryContext::from_route(
            CaptureRoute {
                root_session: Some("run-test".into()),
                session_id: "sess-1".into(),
                storage_session_id: "sess-1".into(),
                subagent_id: None,
            },
            "agent-1",
        ),
        Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        },
        Vec::new(),
        crate::engine::CallCaptureConfig {
            level: CaptureLevel::Dialogue,
            client_model: "deepseek-chat".into(),
            upstream_model: "deepseek-chat".into(),
            provider: ProviderKind::OpenAi,
            protocol: ProtocolKind::ChatCompletions,
            debug_on: false,
        },
    )
}

pub(crate) async fn test_engine(
    sink: Arc<RecordingSink>,
    storage: &std::path::Path,
) -> CaptureEngine {
    let index = SessionIndexStore::open(storage).unwrap().clone_handle();
    CaptureEngine::new(sink, index, Arc::new(storage.to_path_buf()))
        .await
        .unwrap()
}

pub(crate) async fn flush_engine(engine: &CaptureEngine) {
    engine.flush().await.unwrap();
}
