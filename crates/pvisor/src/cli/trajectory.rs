//! One fact journal shared by Run and Gateway producers.
use pvisor_journal::Journal;
use std::path::Path;

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
