//! Checksummed, fsync-before-acknowledgement transaction log. A partial final
//! frame is crash debris; a complete invalid frame is corruption and fails closed.
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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
    max_bytes: u64,
    deferred: bool,
    pending_bytes: usize,
    #[cfg(test)]
    syncs: usize,
}
impl Journal {
    #[cfg(test)]
    pub fn open<T: DeserializeOwned>(path: &Path) -> anyhow::Result<(Self, Vec<T>)> {
        let (mut journal, mut replay) = Self::open_stream(path, DEFAULT_MAX_JOURNAL_BYTES)?;
        let records = replay.by_ref().collect::<anyhow::Result<Vec<T>>>()?;
        let valid_end = replay.valid_end;
        drop(replay);
        journal.finish_replay(valid_end)?;
        Ok((journal, records))
    }
    /// Replay one bounded frame at a time. A malformed complete frame never
    /// truncates the original WAL; only successful full replay may remove debris.
    pub fn open_stream<T: DeserializeOwned>(
        path: &Path,
        max_bytes: u64,
    ) -> anyhow::Result<(Self, Replay<T>)> {
        ensure!(
            max_bytes >= MAX_FRAME_BYTES as u64,
            "journal quota must allow one maximum frame"
        );
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        file.try_lock_exclusive()
            .context("controller state is already owned by another process")?;
        ensure!(file.metadata()?.is_file(), "journal must be a regular file");
        ensure!(
            file.metadata()?.len() <= max_bytes,
            "existing journal exceeds configured byte quota; increase the quota before restart"
        );
        File::open(parent)?.sync_all()?;
        let reader = BufReader::new(file.try_clone()?);
        Ok((
            Self {
                file,
                poisoned: false,
                max_bytes,
                deferred: false,
                pending_bytes: 0,
                #[cfg(test)]
                syncs: 0,
            },
            Replay {
                reader,
                valid_end: 0,
                done: false,
                _record: std::marker::PhantomData,
            },
        ))
    }
    pub fn finish_replay(&mut self, valid_end: u64) -> anyhow::Result<()> {
        self.file.set_len(valid_end)?;
        self.file.sync_all()?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn append(&mut self, value: &impl Serialize) -> anyhow::Result<()> {
        if self.poisoned {
            return Err(JournalFailure.into());
        }
        let mut payload = BoundedPayload {
            bytes: Vec::new(),
            exceeded: false,
        };
        if let Err(error) = serde_json::to_writer(&mut payload, value) {
            if payload.exceeded {
                return Err(crate::artifacts::CapacityExceeded(
                    "journal frame exceeds 16 MiB limit",
                )
                .into());
            }
            return Err(error.into());
        }
        let payload = payload.bytes;
        let frame_bytes = payload.len() as u64 + 66;
        if self
            .file
            .metadata()?
            .len()
            .checked_add(frame_bytes)
            .is_none_or(|size| size > self.max_bytes)
        {
            return Err(crate::artifacts::CapacityExceeded("journal byte quota reached; retain WAL and increase configured quota or perform offline maintenance").into());
        }
        let mut frame = blake3::hash(&payload).to_hex().as_bytes().to_vec();
        frame.push(b' ');
        frame.extend(payload);
        frame.push(b'\n');
        if let Err(error) = self.file.write_all(&frame) {
            self.poisoned = true;
            return Err(anyhow::Error::new(JournalFailure).context(error.to_string()));
        }
        self.pending_bytes = self.pending_bytes.saturating_add(frame.len());
        if !self.deferred {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn ensure_available(&self) -> anyhow::Result<()> {
        if self.poisoned {
            return Err(JournalFailure.into());
        }
        Ok(())
    }

    // Only the HTTP dispatcher may defer sync, while it holds the scheduler
    // mutex across the entire group and withholds every response until end_group.
    pub(crate) fn begin_group(&mut self) -> anyhow::Result<()> {
        self.ensure_available()?;
        ensure!(!self.deferred, "nested journal group");
        self.deferred = true;
        Ok(())
    }

    pub(crate) fn end_group(&mut self) -> anyhow::Result<()> {
        self.deferred = false;
        self.flush()
    }

    pub(crate) fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        self.ensure_available()?;
        if self.pending_bytes == 0 {
            return Ok(());
        }
        if let Err(error) = self.file.sync_all() {
            self.poisoned = true;
            return Err(anyhow::Error::new(JournalFailure).context(error.to_string()));
        }
        self.pending_bytes = 0;
        #[cfg(test)]
        {
            self.syncs += 1;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn syncs(&self) -> usize {
        self.syncs
    }

    #[cfg(test)]
    pub(crate) fn fail_sync(&mut self) {
        // /dev/null accepts writes but fsync fails with EINVAL on Linux.
        self.file = OpenOptions::new().write(true).open("/dev/null").unwrap();
    }
}

pub(crate) const DEFAULT_MAX_JOURNAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
struct BoundedPayload {
    bytes: Vec<u8>,
    exceeded: bool,
}
impl Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_FRAME_BYTES - 66)
        {
            self.exceeded = true;
            return Err(std::io::Error::other("journal payload exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) struct Replay<T> {
    reader: BufReader<File>,
    pub valid_end: u64,
    done: bool,
    _record: std::marker::PhantomData<T>,
}
impl<T: DeserializeOwned> Iterator for Replay<T> {
    type Item = anyhow::Result<T>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| -> anyhow::Result<Option<T>> {
            let mut frame = Vec::new();
            (&mut self.reader)
                .take(MAX_FRAME_BYTES as u64 + 1)
                .read_until(b'\n', &mut frame)?;
            ensure!(
                frame.len() <= MAX_FRAME_BYTES,
                "journal frame exceeds read bound at {}",
                self.valid_end
            );
            if frame.last() != Some(&b'\n') {
                return Ok(None);
            }
            let split = frame
                .iter()
                .position(|byte| *byte == b' ')
                .context("invalid journal frame")?;
            ensure!(split == 64, "invalid journal checksum length");
            let checksum = std::str::from_utf8(&frame[..split])?;
            let payload = &frame[split + 1..frame.len() - 1];
            ensure!(
                blake3::hash(payload).to_hex().as_str() == checksum,
                "journal checksum mismatch at {}",
                self.valid_end
            );
            let transaction =
                serde_json::from_slice(payload).context("invalid journal transaction")?;
            self.valid_end += frame.len() as u64;
            Ok(Some(transaction))
        })();
        match result {
            Ok(Some(transaction)) => Some(Ok(transaction)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_payload_and_full_quota_never_change_committed_wal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let (mut journal, _) = Journal::open::<serde_json::Value>(&path).unwrap();
        journal
            .append(&serde_json::json!({"committed": true}))
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        let oversized = "x".repeat(MAX_FRAME_BYTES);
        assert!(
            journal
                .append(&oversized)
                .unwrap_err()
                .downcast_ref::<crate::artifacts::CapacityExceeded>()
                .is_some()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        journal.max_bytes = original.len() as u64 + 1;
        assert!(
            journal
                .append(&0)
                .unwrap_err()
                .downcast_ref::<crate::artifacts::CapacityExceeded>()
                .is_some()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn bounded_stream_preserves_corruption_and_discards_only_partial_tail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let (mut journal, _) = Journal::open::<u64>(&path).unwrap();
        journal.append(&1u64).unwrap();
        drop(journal);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"partial")
            .unwrap();
        let (_, records) = Journal::open::<u64>(&path).unwrap();
        assert_eq!(records, [1]);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"invalid complete frame\n")
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(Journal::open::<u64>(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn direct_calls_sync_immediately_and_empty_groups_do_not_sync() {
        let temp = tempfile::tempdir().unwrap();
        let (mut journal, _) =
            Journal::open::<serde_json::Value>(&temp.path().join("wal")).unwrap();
        journal
            .append(&serde_json::json!({"direct": true}))
            .unwrap();
        assert_eq!(journal.syncs(), 1);
        assert_eq!(journal.pending_bytes(), 0);
        journal.begin_group().unwrap();
        journal.end_group().unwrap();
        assert_eq!(journal.syncs(), 1);
    }

    #[test]
    fn grouped_frames_replay_a_complete_prefix_and_reject_complete_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let (mut journal, _) = Journal::open::<serde_json::Value>(&path).unwrap();
        journal.begin_group().unwrap();
        for index in 0..3 {
            journal.append(&serde_json::json!({"index":index})).unwrap();
        }
        assert_eq!(journal.syncs(), 0);
        assert!(journal.pending_bytes() > 0);
        journal.end_group().unwrap();
        assert_eq!(journal.syncs(), 1);
        drop(journal);
        let complete = std::fs::read(&path).unwrap();
        let mut truncated = complete.clone();
        truncated.truncate(truncated.len() - 4);
        std::fs::write(&path, truncated).unwrap();
        let (journal, records) = Journal::open::<serde_json::Value>(&path).unwrap();
        assert_eq!(
            records,
            vec![
                serde_json::json!({"index":0}),
                serde_json::json!({"index":1})
            ]
        );
        drop(journal);
        let mut corrupted = complete;
        let last_payload = corrupted
            .iter()
            .rposition(|b| *b == b'0' || *b == b'1' || *b == b'2')
            .unwrap();
        corrupted[last_payload] = b'9';
        std::fs::write(&path, corrupted).unwrap();
        assert!(
            Journal::open::<serde_json::Value>(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("checksum mismatch")
        );
    }

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
