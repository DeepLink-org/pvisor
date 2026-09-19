//! Local JSONL recording for pVisor lifecycle and Gateway events.

use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use persisting_events::EventRecord;
use persisting_gateway::sink::CallbackSink;

use crate::{EventAppendErrorKind, EventSink, TrajectoryEventSink};

/// Append-only JSONL writer used by the pVisor recording path.
/// The serialized value is the complete EventRecord, not a Markdown or
/// dialogue projection, so HTTP request/response wire bodies remain available.
#[derive(Clone)]
pub struct JsonlWriter {
    path: PathBuf,
    file: Arc<Mutex<BufWriter<File>>>,
}

impl std::fmt::Debug for JsonlWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlWriter")
            .field("path", &self.path)
            .finish()
    }
}

impl JsonlWriter {
    pub fn open(destination: &Path) -> anyhow::Result<Self> {
        let path = if destination.extension().is_some() {
            destination.to_path_buf()
        } else {
            destination.join("events.jsonl")
        };
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Arc::new(Mutex::new(BufWriter::new(file))),
        })
    }

    pub fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        serde_json::to_writer(&mut *file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn finish(self) -> anyhow::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.flush()?;
        file.get_ref().sync_all()?;
        Ok(())
    }
}

pub struct JsonlEventSink {
    writer: JsonlWriter,
}

impl JsonlEventSink {
    pub fn new(writer: JsonlWriter) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl EventSink for JsonlEventSink {
    async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
        self.writer.append(event)
    }

    fn classify_append_error(&self, _error: &anyhow::Error) -> EventAppendErrorKind {
        EventAppendErrorKind::Rejected
    }
}

pub fn jsonl_capture_sink(
    writer: &JsonlWriter,
    default_agent_id: &str,
) -> Arc<dyn TrajectoryEventSink> {
    let writer = writer.clone();
    Arc::new(CallbackSink::new(
        default_agent_id,
        move |_route, _agent, record| writer.append(&record),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_events::EventIdentity;

    #[test]
    fn jsonl_writer_preserves_complete_http_payload() {
        let dir = tempfile::tempdir().unwrap();
        let writer = JsonlWriter::open(dir.path()).unwrap();
        let event = EventRecord {
            identity: EventIdentity::default(),
            seq: 0,
            source: "gateway".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: Some("session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "http": {
                    "method": "POST",
                    "request_body": {"messages": [{"role": "user", "content": "full"}]},
                    "response_body": {"choices": [{"message": {"content": "reply"}}]}
                }
            }),
        };
        writer.append(&event).unwrap();
        writer.finish().unwrap();
        let line = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let decoded: EventRecord = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            decoded.payload["http"]["request_body"]["messages"][0]["content"],
            "full"
        );
        assert_eq!(
            decoded.payload["http"]["response_body"]["choices"][0]["message"]["content"],
            "reply"
        );
    }
}
