//! Checksummed, fsync-before-acknowledgement transaction log. A partial final
//! frame is crash debris; a complete invalid frame is corruption and fails closed.
use anyhow::{Context, ensure};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;

pub(crate) struct Journal { file: File, poisoned: bool }
impl Journal {
    pub fn open<T: DeserializeOwned>(path: &Path) -> anyhow::Result<(Self, Vec<T>)> {
        let parent = path.parent().context("journal has no parent")?;
        std::fs::create_dir_all(parent)?;
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
        file.try_lock_exclusive().context("controller state is already owned by another process")?;
        // Persist directory entry as well as transaction contents.
        File::open(parent)?.sync_all()?;
        let mut reader = BufReader::new(file.try_clone()?);
        let mut records = Vec::new();
        let mut valid_end = 0;
        loop {
            let mut frame = Vec::new();
            if reader.read_until(b'\n', &mut frame)? == 0 { break; }
            if frame.last() != Some(&b'\n') { break; }
            let split = frame.iter().position(|byte| *byte == b' ').context("invalid journal frame")?;
            let checksum = std::str::from_utf8(&frame[..split])?;
            let payload = &frame[split + 1..frame.len() - 1];
            ensure!(blake3::hash(payload).to_hex().as_str() == checksum, "journal checksum mismatch at {valid_end}");
            records.push(serde_json::from_slice(payload).context("invalid journal transaction")?);
            valid_end += frame.len() as u64;
        }
        drop(reader);
        file.set_len(valid_end)?;
        file.sync_all()?;
        let mut journal = Self { file, poisoned: false };
        journal.file.seek(SeekFrom::End(0))?;
        Ok((journal, records))
    }

    pub fn append(&mut self, value: &impl Serialize) -> anyhow::Result<()> {
        ensure!(!self.poisoned, "journal write failed previously; restart required");
        let payload = serde_json::to_vec(value)?;
        let mut frame = blake3::hash(&payload).to_hex().as_bytes().to_vec();
        frame.push(b' ');
        frame.extend(payload);
        frame.push(b'\n');
        if let Err(error) = self.file.write_all(&frame).and_then(|_| self.file.sync_all()) {
            self.poisoned = true;
            return Err(error).context("journal commit uncertain; restart required");
        }
        Ok(())
    }
}
