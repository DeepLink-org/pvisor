//! Checksummed, fsync-before-acknowledgement transaction log. A partial final
//! frame is crash debris; a complete invalid frame is corruption and fails closed.
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug)]
pub(crate) struct JournalFailure;
impl std::fmt::Display for JournalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("journal commit unavailable or uncertain; restart required")
    }
}

impl std::error::Error for JournalFailure {}

pub(crate) struct Journal {
    file: File,
    poisoned: bool,
}
impl Journal {
    pub fn open<T: DeserializeOwned>(path: &Path) -> anyhow::Result<(Self, Vec<T>)> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.try_lock_exclusive()
            .context("controller state is already owned by another process")?;
        // Persist directory entry as well as transaction contents.
        File::open(parent)?.sync_all()?;
        let mut reader = BufReader::new(file.try_clone()?);
        let mut records = Vec::new();
        let mut valid_end = 0;
        loop {
            let mut frame = Vec::new();
            if reader.read_until(b'\n', &mut frame)? == 0 {
                break;
            }
            if frame.last() != Some(&b'\n') {
                break;
            }
            let split = frame
                .iter()
                .position(|byte| *byte == b' ')
                .context("invalid journal frame")?;
            let checksum = std::str::from_utf8(&frame[..split])?;
            let payload = &frame[split + 1..frame.len() - 1];
            ensure!(
                blake3::hash(payload).to_hex().as_str() == checksum,
                "journal checksum mismatch at {valid_end}"
            );
            records.push(serde_json::from_slice(payload).context("invalid journal transaction")?);
            valid_end += frame.len() as u64;
        }
        drop(reader);
        file.set_len(valid_end)?;
        file.sync_all()?;
        let mut journal = Self {
            file,
            poisoned: false,
        };
        journal.file.seek(SeekFrom::End(0))?;
        Ok((journal, records))
    }

    pub fn append(&mut self, value: &impl Serialize) -> anyhow::Result<()> {
        if self.poisoned {
            return Err(JournalFailure.into());
        }
        let payload = serde_json::to_vec(value)?;
        let mut frame = blake3::hash(&payload).to_hex().as_bytes().to_vec();
        frame.push(b' ');
        frame.extend(payload);
        frame.push(b'\n');
        if let Err(error) = self
            .file
            .write_all(&frame)
            .and_then(|_| self.file.sync_all())
        {
            self.poisoned = true;
            return Err(anyhow::Error::new(JournalFailure).context(error.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_append_poisoning_cannot_be_confused_with_a_fencing_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let (mut journal, _) = Journal::open::<serde_json::Value>(&path).unwrap();
        journal
            .append(&serde_json::json!({"committed":true}))
            .unwrap();
        // Replace the writer with a read-only file to exercise an actual OS
        // write failure without depending on mount privileges or filling disk.
        journal.file = File::open(&path).unwrap();
        assert!(
            journal
                .append(&serde_json::json!({"lost":true}))
                .unwrap_err()
                .downcast_ref::<JournalFailure>()
                .is_some()
        );
        assert!(
            journal
                .append(&serde_json::json!({"later":true}))
                .unwrap_err()
                .downcast_ref::<JournalFailure>()
                .is_some()
        );
        drop(journal);
        let (_, records) = Journal::open::<serde_json::Value>(&path).unwrap();
        assert_eq!(records, vec![serde_json::json!({"committed":true})]);
    }
}
