//! One fact journal shared by Run and Gateway producers.
use crate::TrajectoryEventSink;
use persisting_gateway::sink::JournalObserver;
use persisting_journal::Journal;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct JournalRecording {
    pub journal: Journal,
}
impl JournalRecording {
    pub fn open(destination: &Path) -> anyhow::Result<Self> {
        let path = if destination.extension().is_some() {
            destination.to_path_buf()
        } else {
            destination.join("events.trace.jsonl")
        };
        Ok(Self {
            journal: Journal::open(&path)?,
        })
    }
    /// Every accepted append already has a synced receipt.
    pub fn finish(self) -> anyhow::Result<()> {
        Ok(())
    }
}
pub fn journal_capture_observer(writer: &JournalRecording) -> Arc<dyn TrajectoryEventSink> {
    Arc::new(JournalObserver {
        journal: writer.journal.clone(),
    })
}
