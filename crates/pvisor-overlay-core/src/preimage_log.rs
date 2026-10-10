//! Framed, multi-writer storage for compact first observations.
//!
//! OverlayCore explicitly registers this format for a fresh, exclusively owned
//! stage. Callers must retain
//! the existing stage/target transaction contract. File locks arbitrate writers;
//! a digest covers small serialized records, never a second scan of file data.
//! A failed append or sync poisons this handle: reopen to resolve its outcome.
//! Published log files must not be replaced or compacted while handles are live.

use crate::{OverlayCore, PathFingerprint, PathPreimage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

const MAGIC: &[u8] = b"pvisor.preimages/2\n";
const FRAME_MAGIC: &[u8; 4] = b"PVR2";
const HEADER: usize = 12;
const DIGEST: usize = 32;
const MAX_RECORD: usize = 4 * 1024 * 1024;
const MAX_LOG: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Record {
    Observe { preimage: PathPreimage },
    Consume { paths: Vec<Vec<u8>> },
    DirectoryBaseline { preimage: PathPreimage },
}

#[derive(Debug)]
struct Observation {
    preimage: PathPreimage,
    end: u64,
}

#[derive(Debug)]
struct State {
    profile: crate::profile::Profile,
    cursor: u64,
    synced: u64,
    observations: BTreeMap<Vec<u8>, Observation>,
    poisoned: bool,
    // Apple barrier orders log bytes; this directory's FULLFSYNC is the
    // durable device drain. Read-only replay never performs either operation.
    #[cfg(target_os = "macos")]
    durability_directory: Option<File>,
}

/// An incremental view of one immutable log inode. Multiple handles/processes
/// may share it; every operation refreshes the newly appended suffix under flock.
#[derive(Debug)]
pub struct PreimageLog {
    path: PathBuf,
    file: File,
    // A live regular-file descriptor cannot change its physical identity. Only
    // the named binding/length must be observed again under the writer lock.
    identity: (u64, u64),
    state: State,
}

struct Lock<'a>(&'a File);
impl<'a> Lock<'a> {
    fn new(file: &'a File) -> io::Result<Self> {
        fs2::FileExt::lock_exclusive(file)?;
        Ok(Self(file))
    }
}
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(self.0);
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn sync(file: &File, state: &mut State) -> io::Result<()> {
    let profile = state.profile.clone();
    let _span = profile.span("sync");
    #[cfg(target_os = "macos")]
    let result = crate::sys::order_before_publish(file).and_then(|()| {
        state
            .durability_directory
            .as_ref()
            .ok_or_else(|| invalid("read-only replay cannot sync"))?
            .sync_all()
    });
    #[cfg(not(target_os = "macos"))]
    let result = file.sync_all();
    if let Err(error) = result {
        state.poisoned = true;
        return Err(error);
    }
    state.synced = state.cursor;
    Ok(())
}

fn truncate_tail(file: &File, state: &mut State) -> io::Result<()> {
    if let Err(error) = file.set_len(state.cursor) {
        state.poisoned = true;
        return Err(error);
    }
    sync(file, state)
}

fn validate(record: &Record) -> io::Result<()> {
    match record {
        Record::Observe { preimage } => validate_path(&preimage.relative_path()),
        Record::DirectoryBaseline { preimage } => {
            validate_path(&preimage.relative_path())?;
            if !matches!(preimage.state, PathFingerprint::Directory { .. }) {
                return Err(invalid("apply baseline must be a directory"));
            }
            Ok(())
        }
        Record::Consume { paths } => {
            use std::os::unix::ffi::OsStrExt;
            for path in paths {
                validate_path(Path::new(std::ffi::OsStr::from_bytes(path)))?;
            }
            Ok(())
        }
    }
}

fn validate_path(path: &Path) -> io::Result<()> {
    OverlayCore::validate_rel(path)?;
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(invalid("preimage path contains NUL"));
    }
    Ok(())
}

fn apply(state: &mut State, record: Record, end: u64) {
    match record {
        Record::Observe { preimage } => {
            state
                .observations
                .entry(preimage.path.clone())
                .or_insert(Observation { preimage, end });
        }
        Record::Consume { paths } => {
            for path in paths {
                state.observations.remove(&path);
            }
        }
        Record::DirectoryBaseline { preimage } => {
            state
                .observations
                .insert(preimage.path.clone(), Observation { preimage, end });
        }
    }
}

// Only a structurally valid but incomplete final frame may be repaired.
// A complete invalid header, digest or JSON record is never silently dropped.
fn refresh(file: &File, state: &mut State, repair: bool, length: u64) -> io::Result<()> {
    let profile = state.profile.clone();
    let _span = profile.span("refresh");
    if length < state.cursor || length > MAX_LOG {
        return Err(invalid("preimage log truncated or exceeds size limit"));
    }
    while state.cursor < length {
        let remaining = length - state.cursor;
        let mut header = [0u8; HEADER];
        let available = remaining.min(HEADER as u64) as usize;
        file.read_exact_at(&mut header[..available], state.cursor)?;
        if header[..available.min(4)] != FRAME_MAGIC[..available.min(4)] {
            return Err(invalid("invalid preimage frame magic"));
        }
        if available >= 8 {
            let bytes = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if bytes == 0 || bytes as usize > MAX_RECORD {
                return Err(invalid("invalid preimage frame length"));
            }
            let known = available.saturating_sub(8);
            if header[8..8 + known] != (!bytes).to_le_bytes()[..known] {
                return Err(invalid("invalid preimage frame length"));
            }
        }
        if available < HEADER {
            if !repair {
                return Ok(());
            }
            return truncate_tail(file, state);
        }
        let bytes = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let frame_length = HEADER as u64 + u64::from(bytes) + DIGEST as u64;
        if remaining < frame_length {
            if !repair {
                return Ok(());
            }
            return truncate_tail(file, state);
        }
        let mut body = vec![0; bytes as usize];
        file.read_exact_at(&mut body, state.cursor + HEADER as u64)?;
        let mut expected = [0; DIGEST];
        file.read_exact_at(
            &mut expected,
            state.cursor + HEADER as u64 + u64::from(bytes),
        )?;
        let mut digest = Sha256::new();
        digest.update(header);
        digest.update(&body);
        if digest.finalize().as_slice() != expected {
            return Err(invalid("preimage frame digest mismatch"));
        }
        let record: Record = serde_json::from_slice(&body).map_err(io::Error::other)?;
        validate(&record)?;
        state.cursor += frame_length;
        apply(state, record, state.cursor);
    }
    Ok(())
}

fn append(file: &File, state: &mut State, record: Record, durable: bool) -> io::Result<()> {
    let profile = state.profile.clone();
    let _span = profile.span("append");
    validate(&record)?;
    let body = serde_json::to_vec(&record).map_err(io::Error::other)?;
    if body.is_empty() || body.len() > MAX_RECORD {
        return Err(invalid("preimage record exceeds size limit"));
    }
    let bytes = body.len() as u32;
    let mut frame = Vec::with_capacity(HEADER + body.len() + DIGEST);
    frame.extend_from_slice(FRAME_MAGIC);
    frame.extend_from_slice(&bytes.to_le_bytes());
    frame.extend_from_slice(&(!bytes).to_le_bytes());
    frame.extend_from_slice(&body);
    let digest = Sha256::digest(&frame);
    frame.extend_from_slice(&digest);
    let end = state.cursor + frame.len() as u64;
    if end > MAX_LOG {
        return Err(invalid("preimage log exceeds size limit"));
    }
    if let Err(error) = file.write_all_at(&frame, state.cursor) {
        state.poisoned = true;
        return Err(error);
    }
    state.cursor = end;
    apply(state, record, end);
    if durable {
        sync(file, state)?;
    }
    Ok(())
}

impl PreimageLog {
    /// Drain the complete prefix under the same lock used by all writers.
    pub fn sync_all(&mut self) -> io::Result<()> {
        self.locked(sync)
    }
    /// Create/open, replay once, repair only an incomplete tail, and sync the
    /// recovered prefix before retrying an operation whose acknowledgement was lost.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut state = State {
            profile: crate::profile::Profile::from_env("preimage-log"),
            cursor: MAGIC.len() as u64,
            synced: 0,
            observations: BTreeMap::new(),
            poisoned: false,
            #[cfg(target_os = "macos")]
            durability_directory: Some(File::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?),
        };
        let identity;
        {
            let _lock = Lock::new(&file)?;
            let metadata = file.metadata()?;
            identity = (metadata.dev(), metadata.ino());
            if !metadata.is_file() {
                return Err(invalid("preimage log must be a regular file"));
            }
            let length = if metadata.len() == 0 {
                file.write_all_at(MAGIC, 0)?;
                MAGIC.len() as u64
            } else {
                let mut magic = vec![0; MAGIC.len()];
                file.read_exact_at(&mut magic, 0)?;
                if magic != MAGIC {
                    return Err(invalid("unsupported preimage log format"));
                }
                metadata.len()
            };
            refresh(&file, &mut state, true, length)?;
            sync(&file, &mut state)?;
            // sync orders the complete prefix, including a newly written magic.
            // On macOS it also drains the parent directory; repeating that full
            // sync here adds no durability. Linux file fsync needs a separate
            // directory fsync, including recovery of an interrupted creator.
            #[cfg(not(target_os = "macos"))]
            File::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
            state,
        })
    }

    fn locked<T>(
        &mut self,
        operation: impl FnOnce(&File, &mut State) -> io::Result<T>,
    ) -> io::Result<T> {
        let profile = self.state.profile.clone();
        let _span = profile.span("transaction");
        if self.state.poisoned {
            return Err(io::Error::other(
                "preimage append outcome unknown; reopen log",
            ));
        }
        let lock_wait = profile.span("lock_wait");
        let _lock = Lock::new(&self.file)?;
        drop(lock_wait);
        let binding = profile.span("binding_check");
        let named = fs::symlink_metadata(&self.path)?;
        drop(binding);
        if !named.is_file() || (named.dev(), named.ino()) != self.identity {
            return Err(invalid("preimage log inode was replaced"));
        }
        if let Err(error) = refresh(&self.file, &mut self.state, true, named.len()) {
            self.state.poisoned = true;
            return Err(error);
        }
        operation(&self.file, &mut self.state)
    }

    /// Capture outside flock, then arbitrate again: another writer's earlier
    /// observation wins. Durable promotion syncs the actual winner, not a loser.
    pub fn observe(
        &mut self,
        path: &Path,
        durable: bool,
        capture: impl FnOnce() -> io::Result<PathFingerprint>,
    ) -> io::Result<()> {
        if self.contains_or_promote(path, durable)? {
            return Ok(());
        }
        self.publish_observation(path, durable, capture()?)
    }

    // Split the transaction so OverlayCore can also release its handle mutex
    // while capturing bytes; both halves still refresh under flock.
    pub(crate) fn contains_or_promote(&mut self, path: &Path, durable: bool) -> io::Result<bool> {
        validate_path(path)?;
        let key = path.as_os_str().as_bytes();
        self.locked(|file, state| {
            if let Some(existing) = state.observations.get(key) {
                if durable && existing.end > state.synced {
                    sync(file, state)?;
                }
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    pub(crate) fn publish_observation(
        &mut self,
        path: &Path,
        durable: bool,
        fingerprint: PathFingerprint,
    ) -> io::Result<()> {
        validate_path(path)?;
        let key = path.as_os_str().as_bytes();
        let candidate = PathPreimage {
            path: key.to_vec(),
            state: fingerprint,
        };
        self.locked(|file, state| {
            if let Some(existing) = state.observations.get(key) {
                if durable && existing.end > state.synced {
                    sync(file, state)?;
                }
                Ok(())
            } else {
                append(
                    file,
                    state,
                    Record::Observe {
                        preimage: candidate,
                    },
                    durable,
                )
            }
        })
    }

    /// Consume only after corresponding target mutations commit. Other live
    /// handles replay the tombstone before deciding whether to capture again.
    pub fn consume(&mut self, paths: &[PathBuf]) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        for path in paths {
            validate_path(path)?;
        }
        self.locked(|file, state| {
            let paths = paths
                .iter()
                .map(|path| path.as_os_str().as_bytes().to_vec())
                .filter(|path| state.observations.contains_key(path))
                .collect::<Vec<_>>();
            let mut batch = Vec::new();
            let mut estimate = 128usize;
            let mut appended = false;
            for path in paths {
                // JSON u8 arrays use at most four bytes per source byte;
                // conservatively bound each tombstone frame before encoding.
                let bytes = path
                    .len()
                    .checked_mul(4)
                    .and_then(|n| n.checked_add(3))
                    .ok_or_else(|| invalid("preimage path exceeds size limit"))?;
                if bytes > MAX_RECORD - 128 {
                    return Err(invalid("preimage path exceeds size limit"));
                }
                if estimate + bytes > MAX_RECORD {
                    append(
                        file,
                        state,
                        Record::Consume {
                            paths: std::mem::take(&mut batch),
                        },
                        false,
                    )?;
                    appended = true;
                    estimate = 128;
                }
                estimate += bytes;
                batch.push(path);
            }
            if !batch.is_empty() {
                append(file, state, Record::Consume { paths: batch }, false)?;
                appended = true;
            }
            if appended {
                sync(file, state)?;
            }
            Ok(())
        })
    }

    /// Current logical observations in byte-path order. This method repairs an
    /// incomplete tail under the writer lock; it is not a read-only audit API.
    pub fn observations(&mut self) -> io::Result<Vec<PathPreimage>> {
        self.locked(|_, state| {
            Ok(state
                .observations
                .values()
                .map(|entry| entry.preimage.clone())
                .collect())
        })
    }

    /// Persist the post-apply directory baseline before pruning/TargetApplied.
    /// This exception to first-observation immutability is for committed apply
    /// recovery only; files and ordinary workload observations cannot rebase.
    pub fn applied_directory(&mut self, preimage: PathPreimage) -> io::Result<()> {
        let record = Record::DirectoryBaseline { preimage };
        validate(&record)?;
        self.locked(|file, state| append(file, state, record, true))
    }

    /// Read complete frames without repairs, creation, syncing or truncation.
    /// An incomplete final frame is not an observation; complete corruption
    /// still fails. Callers must not treat this as proof of durable read-set state.
    pub fn read(path: &Path) -> io::Result<Vec<PathPreimage>> {
        Self::read_prefix(path, false)
    }

    /// Sealing must never acknowledge an incomplete final observation.
    pub fn read_complete(path: &Path) -> io::Result<Vec<PathPreimage>> {
        Self::read_prefix(path, true)
    }

    fn read_prefix(path: &Path, complete: bool) -> io::Result<Vec<PathPreimage>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let _lock = Lock::new(&file)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("preimage log must be a regular file"));
        }
        let mut magic = vec![0; MAGIC.len()];
        file.read_exact_at(&mut magic, 0)?;
        if magic != MAGIC {
            return Err(invalid("unsupported preimage log format"));
        }
        let mut state = State {
            profile: crate::profile::Profile::from_env("preimage-log-read"),
            cursor: MAGIC.len() as u64,
            synced: 0,
            observations: BTreeMap::new(),
            poisoned: false,
            #[cfg(target_os = "macos")]
            durability_directory: None,
        };
        refresh(&file, &mut state, false, metadata.len())?;
        if complete && state.cursor != metadata.len() {
            return Err(invalid("incomplete preimage log cannot be sealed"));
        }
        Ok(state
            .observations
            .into_values()
            .map(|entry| entry.preimage)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_observations_wait_for_explicit_sync_and_keep_the_first_value() {
        let temp = tempfile::tempdir().unwrap();
        let mut log = PreimageLog::open(&temp.path().join("log")).unwrap();
        let initial = log.state.synced;
        for index in 0..256 {
            log.observe(Path::new(&format!("new-{index}")), false, || {
                Ok(PathFingerprint::Absent)
            })
            .unwrap();
        }
        assert_eq!(log.state.synced, initial);
        assert!(log.state.cursor > initial);
        log.observe(Path::new("new-0"), false, || {
            panic!("first observation must win")
        })
        .unwrap();
        log.sync_all().unwrap();
        assert_eq!(log.state.synced, log.state.cursor);
        assert_eq!(PreimageLog::read_complete(&log.path).unwrap().len(), 256);
    }

    #[test]
    fn winner_promotion_and_external_consumption_refresh_live_handles() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        fs::write(temp.path().join("value"), b"original").unwrap();
        let original = crate::fingerprint_at(temp.path(), Path::new("value")).unwrap();
        let mut loser = PreimageLog::open(&path).unwrap();
        let mut winner = PreimageLog::open(&path).unwrap();
        loser
            .observe(Path::new("value"), true, || {
                winner.observe(Path::new("value"), false, || Ok(original.clone()))?;
                fs::write(temp.path().join("value"), b"host edit")?;
                crate::fingerprint_at(temp.path(), Path::new("value"))
            })
            .unwrap();
        assert_eq!(loser.observations().unwrap()[0].state, original);
        assert_eq!(winner.observations().unwrap().len(), 1);
        winner.consume(&[PathBuf::from("value")]).unwrap();
        assert!(loser.observations().unwrap().is_empty());
        let changed = crate::fingerprint_at(temp.path(), Path::new("value")).unwrap();
        loser
            .observe(Path::new("value"), true, || Ok(changed.clone()))
            .unwrap();
        assert_eq!(winner.observations().unwrap()[0].state, changed);
    }

    #[test]
    fn every_incomplete_tail_recovers_only_the_valid_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut log = PreimageLog::open(&path).unwrap();
        log.observe(Path::new("first"), true, || Ok(PathFingerprint::Absent))
            .unwrap();
        let prefix = fs::metadata(&path).unwrap().len();
        log.observe(Path::new("second"), false, || Ok(PathFingerprint::Absent))
            .unwrap();
        let full = fs::metadata(&path).unwrap().len();
        drop(log);
        for cut in prefix..full {
            let candidate = temp.path().join(format!("cut-{cut}"));
            fs::copy(&path, &candidate).unwrap();
            OpenOptions::new()
                .write(true)
                .open(&candidate)
                .unwrap()
                .set_len(cut)
                .unwrap();
            let mut recovered = PreimageLog::open(&candidate).unwrap();
            let observations = recovered.observations().unwrap();
            assert_eq!(observations.len(), 1, "cut {cut}");
            assert_eq!(observations[0].relative_path(), Path::new("first"));
            assert_eq!(fs::metadata(&candidate).unwrap().len(), prefix);
            recovered
                .observe(Path::new("second"), true, || Ok(PathFingerprint::Absent))
                .unwrap();
            assert_eq!(recovered.observations().unwrap().len(), 2);
        }
    }

    #[test]
    fn complete_corruption_is_rejected_without_truncating_the_log() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut log = PreimageLog::open(&path).unwrap();
        for value in ["first", "second"] {
            log.observe(Path::new(value), true, || Ok(PathFingerprint::Absent))
                .unwrap();
        }
        drop(log);
        let original = fs::read(&path).unwrap();
        for offset in [MAGIC.len(), MAGIC.len() + 8, MAGIC.len() + HEADER + 5] {
            let mut corrupt = original.clone();
            corrupt[offset] ^= 1;
            fs::write(&path, &corrupt).unwrap();
            assert!(PreimageLog::open(&path).is_err(), "offset {offset}");
            assert_eq!(fs::read(&path).unwrap(), corrupt);
        }
    }

    #[test]
    fn malformed_partial_header_is_not_mistaken_for_a_recoverable_tail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut corrupt = MAGIC.to_vec();
        corrupt.extend_from_slice(FRAME_MAGIC);
        corrupt.extend_from_slice(&8u32.to_le_bytes());
        corrupt.push(0); // A known byte of the length complement is invalid.
        fs::write(&path, &corrupt).unwrap();
        assert!(PreimageLog::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn large_consumption_is_framed_and_visible_to_an_existing_reader() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut writer = PreimageLog::open(&path).unwrap();
        let mut reader = PreimageLog::open(&path).unwrap();
        let paths = (0..300)
            .map(|index| PathBuf::from(format!("{}{index:04}", "p/".repeat(2000))))
            .collect::<Vec<_>>();
        for path in &paths {
            writer
                .observe(path, false, || Ok(PathFingerprint::Absent))
                .unwrap();
        }
        assert_eq!(reader.observations().unwrap().len(), paths.len());
        writer.consume(&paths).unwrap();
        assert!(reader.observations().unwrap().is_empty());
        assert!(PreimageLog::read(&path).unwrap().is_empty());
    }

    #[test]
    fn read_only_audit_preserves_incomplete_tail_and_directory_apply_baselines() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        fs::create_dir(temp.path().join("directory")).unwrap();
        let before = crate::fingerprint_at(temp.path(), Path::new("directory")).unwrap();
        let mut log = PreimageLog::open(&path).unwrap();
        log.observe(Path::new("directory"), true, || Ok(before))
            .unwrap();
        fs::write(temp.path().join("directory/child"), b"applied").unwrap();
        let after = crate::fingerprint_at(temp.path(), Path::new("directory")).unwrap();
        log.applied_directory(PathPreimage {
            path: b"directory".to_vec(),
            state: after.clone(),
        })
        .unwrap();
        assert_eq!(PreimageLog::read(&path).unwrap()[0].state, after);
        let stable = fs::read(&path).unwrap();
        assert!(
            log.applied_directory(PathPreimage {
                path: b"file".to_vec(),
                state: PathFingerprint::Absent
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), stable);
        log.observe(Path::new("last"), false, || Ok(PathFingerprint::Absent))
            .unwrap();
        drop(log);
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        let incomplete = fs::read(&path).unwrap();
        assert_eq!(PreimageLog::read(&path).unwrap().len(), 1);
        assert_eq!(fs::read(&path).unwrap(), incomplete);
        assert_eq!(
            PreimageLog::open(&path)
                .unwrap()
                .observations()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(fs::read(&path).unwrap(), stable);
    }

    #[test]
    fn invalid_paths_size_limits_and_replaced_inodes_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut log = PreimageLog::open(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            log.observe(Path::new("../escape"), true, || panic!(
                "must reject before capture"
            ))
            .is_err()
        );
        assert!(log.consume(&[PathBuf::from("/absolute")]).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::rename(&path, temp.path().join("old")).unwrap();
        fs::write(&path, &before).unwrap();
        assert!(log.observations().is_err());
        drop(log);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_LOG + 1)
            .unwrap();
        assert!(PreimageLog::open(&path).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_LOG + 1);
    }

    #[test]
    fn failed_append_poisons_handle_and_reopen_adopts_complete_lost_ack() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let mut log = PreimageLog::open(&path).unwrap();
        let read_only = File::open(&path).unwrap();
        assert!(
            append(
                &read_only,
                &mut log.state,
                Record::Observe {
                    preimage: PathPreimage {
                        path: b"failed".to_vec(),
                        state: PathFingerprint::Absent
                    },
                },
                true
            )
            .is_err()
        );
        assert!(
            log.observe(Path::new("value"), false, || panic!("poisoned handle"))
                .is_err()
        );
        drop(log);
        let mut log = PreimageLog::open(&path).unwrap();
        log.observe(Path::new("value"), false, || Ok(PathFingerprint::Absent))
            .unwrap();
        // Simulate loss of the acknowledgement after a complete append.
        let bytes = fs::read(&path).unwrap();
        drop(log);
        let mut retry = PreimageLog::open(&path).unwrap();
        retry
            .observe(Path::new("value"), true, || panic!("must adopt winner"))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(retry.observations().unwrap().len(), 1);
    }

    #[test]
    fn independent_processes_preserve_first_observation_and_all_appends() {
        const ROLE: &str = "PVISOR_PREIMAGE_LOG_CHILD";
        if let Some(index) = std::env::var_os(ROLE) {
            let root = PathBuf::from(std::env::var_os("PVISOR_PREIMAGE_LOG_ROOT").unwrap());
            let mut log = PreimageLog::open(&root.join("log")).unwrap();
            let source = PathBuf::from(index);
            let state = crate::fingerprint_at(&root, &source).unwrap();
            log.observe(Path::new("shared"), true, || Ok(state.clone()))
                .unwrap();
            log.observe(&source, true, || Ok(state)).unwrap();
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let mut children = Vec::new();
        let mut candidates = Vec::new();
        for index in 0..4 {
            let name = format!("source-{index}");
            fs::write(temp.path().join(&name), name.as_bytes()).unwrap();
            candidates.push(crate::fingerprint_at(temp.path(), Path::new(&name)).unwrap());
            children.push(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "preimage_log::tests::independent_processes_preserve_first_observation_and_all_appends"])
                .env(ROLE, name).env("PVISOR_PREIMAGE_LOG_ROOT", temp.path())
                .stdout(std::process::Stdio::null()).spawn().unwrap());
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let mut log = PreimageLog::open(&temp.path().join("log")).unwrap();
        let observations = log.observations().unwrap();
        assert_eq!(observations.len(), 5);
        let shared = observations
            .iter()
            .find(|record| record.path == b"shared")
            .unwrap();
        assert!(candidates.contains(&shared.state));
        for index in 0..4 {
            assert!(
                observations
                    .iter()
                    .any(|record| record.path == format!("source-{index}").as_bytes())
            );
        }
    }
    #[test]
    #[ignore = "manual repeated known-observation journal overhead measurement"]
    fn known_observation_screening() {
        use std::time::Instant;
        for count in [64, 2048] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("log");
            let paths = (0..count)
                .map(|i| PathBuf::from(format!("file-{i:04}")))
                .collect::<Vec<_>>();
            let mut log = PreimageLog::open(&path).unwrap();
            for path in &paths {
                log.observe(path, false, || Ok(PathFingerprint::Absent))
                    .unwrap();
            }
            for round in 0..18 {
                let started = Instant::now();
                for i in 0..20000 {
                    log.observe(&paths[i % count], false, || {
                        panic!("known observation unexpectedly recaptured")
                    })
                    .unwrap();
                }
                let elapsed = started.elapsed().as_nanos();
                if round >= 3 {
                    println!(
                        "PVISOR_KNOWN_OBSERVATION_SCREEN {}",
                        serde_json::json!({
                            "paths":count,"round":round-3,"observations":20000,"elapsed_ns":elapsed,
                            "scope":"existing journal observations only; no source access or durability promotion"
                        })
                    );
                }
            }
        }
    }
}
