//! Owned stage persistence. The supervisor must hold its stage lease and stop
//! all writers before sealing. A seal is a completion boundary, not a snapshot
//! of an upper that continues to change; live checkpoints seal their own copy.
use crate::{load_preimages, preimage_journal_is_complete};
pub use pvisor_core::overlay::StageDurability;
use pvisor_journal::api::{DurableFiles, Persistence};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const POLICY: &str = "durability-v1";
const SEAL: &str = "sealed-v1";
const SEALED: &[u8] = b"pvisor.stage.sealed/1\n";

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn open(path: &Path, directory: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(
            libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
        )
        .open(path)
}

/// Missing policy means the original strict journal contract, including legacy
/// stages. Unknown or corrupt policy is an error, never a relaxed fallback.
pub fn policy(journal: &Path) -> io::Result<StageDurability> {
    match open(&journal.join(POLICY), false) {
        Ok(mut file) => {
            use std::io::Read;
            if !file.metadata()?.is_file() {
                return Err(invalid("stage policy must be a regular file"));
            }
            let mut bytes = Vec::new();
            file.by_ref().take(128).read_to_end(&mut bytes)?;
            match bytes.as_slice() {
                b"pvisor.stage.checkpoint/1\n" => Ok(StageDurability::Checkpoint),
                b"pvisor.stage.strict/1\n" => Ok(StageDurability::Strict),
                _ => Err(invalid("unsupported stage durability policy")),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(StageDurability::Strict),
        Err(error) => Err(error),
    }
}

fn managed(journal: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(journal.join(POLICY)) {
        Ok(_) => {
            policy(journal)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Admit a new owned stage, or a verified checkpoint copy. Existing managed
/// stages must be sealed before reuse: an interrupted upper cannot be promoted
/// to a trusted result simply by reopening and syncing whatever survived.
pub fn begin(journal: &Path, durability: StageDurability) -> io::Result<()> {
    fs::create_dir_all(journal)?;
    if managed(journal)? {
        require_sealed(journal)?;
        if policy(journal)? != durability {
            return Err(invalid(
                "cannot change an existing stage's durability policy",
            ));
        }
    } else {
        // Never silently relax a restored legacy stage's crash contract.
        // Its missing policy remains strict, including old journals without
        // complete-v1. New policies are registered only before initialization.
        match fs::symlink_metadata(journal.join("entries")) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let bytes: &[u8] = match durability {
            StageDurability::Checkpoint => b"pvisor.stage.checkpoint/1\n",
            StageDurability::Strict => b"pvisor.stage.strict/1\n",
        };
        Persistence::atomic_write(&journal.join(POLICY), bytes, 0o600).map_err(io::Error::other)?;
    }
    match fs::remove_file(journal.join(SEAL)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Remove the old acknowledgement durably before any writer is exposed.
    open(journal, true)?.sync_all()?;
    if let Some(parent) = journal.parent() {
        open(parent, true)?.sync_all()?;
    }
    Ok(())
}

pub fn require_sealed(journal: &Path) -> io::Result<()> {
    if !managed(journal)? {
        return Ok(());
    }
    let mut file = open(&journal.join(SEAL), false).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            invalid("STAGE_INCOMPLETE: no durable stage completion; restore a committed checkpoint or discard this stage")
        } else { error }
    })?;
    use std::io::Read;
    if !file.metadata()?.is_file() {
        return Err(invalid("stage completion marker must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(128).read_to_end(&mut bytes)?;
    if bytes != SEALED {
        return Err(invalid("invalid stage completion marker"));
    }
    if !preimage_journal_is_complete(journal) {
        return Err(invalid(
            "sealed stage journal is missing its completeness marker",
        ));
    }
    if crate::core::compact_preimages(journal)? {
        crate::preimage_log::PreimageLog::read_complete(
            &journal.join(crate::core::PREIMAGE_LOG_NAME),
        )?;
    } else {
        load_preimages(journal)?;
    }
    Ok(())
}

pub(crate) fn admits_writes(journal: &Path) -> io::Result<bool> {
    if !managed(journal)? {
        return Ok(true);
    }
    match fs::symlink_metadata(journal.join(SEAL)) {
        Ok(_) => {
            require_sealed(journal)?;
            Ok(false)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

/// Persist all observations before an explicit workload fsync or snapshot.
pub fn sync_journal(journal: &Path) -> io::Result<()> {
    if crate::core::compact_preimages(journal)? {
        crate::preimage_log::PreimageLog::read_complete(
            &journal.join(crate::core::PREIMAGE_LOG_NAME),
        )?;
    } else {
        load_preimages(journal)?;
    }
    sync_tree(journal, &mut HashSet::new())
}

/// Journal first, upper contents and namespace second, acknowledgement last.
/// On failure no new seal is published. Call only with all writers stopped.
/// `PVISOR_STAGE_SYNC_WORKERS=2|4|8` opts into bounded parallel upper-file
/// drains; unset or `1` retains serial traversal. Other values are errors.
/// Parallel drains join before bottom-up directory sync and seal publication.
pub fn seal(upper: &Path, journal: &Path) -> io::Result<()> {
    let workers = sync_workers(std::env::var_os("PVISOR_STAGE_SYNC_WORKERS"))?;
    seal_with_upper(upper, journal, |upper| {
        if workers == 1 {
            sync_tree(upper, &mut HashSet::new())
        } else {
            sync_upper_parallel(upper, workers, &|file, _, _| file.sync_all())
        }
    })
}

// Experimental opt-in. The default retains the original traversal, allowing a
// same-binary control. Reject typos rather than silently changing durability.
fn sync_workers(value: Option<std::ffi::OsString>) -> io::Result<usize> {
    match value.as_deref().and_then(|value| value.to_str()) {
        None if value.is_none() => Ok(1),
        Some("1") => Ok(1),
        Some("2") => Ok(2),
        Some("4") => Ok(4),
        Some("8") => Ok(8),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_STAGE_SYNC_WORKERS must be 1, 2, 4 or 8",
        )),
    }
}

fn seal_with_upper(
    upper: &Path,
    journal: &Path,
    sync_upper: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if !managed(journal)? {
        return Ok(());
    }
    if !preimage_journal_is_complete(journal) {
        return Err(invalid("stage journal initialization is incomplete"));
    }
    sync_journal(journal)?;
    open(upper, true)?;
    sync_upper(upper)?;
    if let Some(parent) = upper.parent() {
        open(parent, true)?.sync_all()?;
    }
    Persistence::atomic_write(&journal.join(SEAL), SEALED, 0o600).map_err(io::Error::other)?;
    Ok(())
}

struct UpperEntry {
    path: PathBuf,
    identity: (u64, u64),
}

impl UpperEntry {
    fn open_checked(&self, directory: bool) -> io::Result<File> {
        let file = open(&self.path, directory)?;
        let metadata = file.metadata()?;
        if (metadata.dev(), metadata.ino()) != self.identity
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return Err(invalid("stage entry changed during persistence"));
        }
        Ok(file)
    }
}

fn collect_upper(
    path: &Path,
    seen: &mut HashSet<(u64, u64)>,
    files: &mut Vec<UpperEntry>,
    directories: &mut Vec<UpperEntry>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let identity = (metadata.dev(), metadata.ino());
    let entry = UpperEntry {
        path: path.to_owned(),
        identity,
    };
    if metadata.is_dir() {
        // Validate now and again when syncing. Do not keep an FD per entry.
        entry.open_checked(true)?;
        for child in fs::read_dir(path)? {
            collect_upper(&child?.path(), seen, files, directories)?;
        }
        directories.push(entry); // Children must be durable before parents.
    } else if metadata.is_file() && seen.insert(identity) {
        files.push(entry);
    }
    // Symlinks and special files are persisted by their directory entries.
    Ok(())
}

fn sync_upper_parallel(
    upper: &Path,
    workers: usize,
    sync: &(impl Fn(&File, &Path, bool) -> io::Result<()> + Sync),
) -> io::Result<()> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    collect_upper(upper, &mut HashSet::new(), &mut files, &mut directories)?;
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut error = None;
        for _ in 0..workers.min(files.len()) {
            let task = || -> io::Result<()> {
                while !failed.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = files.get(index) else { break };
                    let result = entry
                        .open_checked(false)
                        .and_then(|file| sync(&file, &entry.path, false));
                    if let Err(error) = result {
                        failed.store(true, Ordering::Relaxed);
                        return Err(error);
                    }
                }
                Ok(())
            };
            match std::thread::Builder::new()
                .name("stage-sync".into())
                .spawn_scoped(scope, task)
            {
                Ok(handle) => handles.push(handle),
                Err(cause) => {
                    failed.store(true, Ordering::Relaxed);
                    error = Some(cause);
                    break;
                }
            }
        }
        // Join every worker even after an error. No background writes survive
        // this boundary, and no directories/seal are committed after failure.
        for handle in handles {
            if let Err(cause) = handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("stage sync worker panicked")))
            {
                failed.store(true, Ordering::Relaxed);
                error.get_or_insert(cause);
            }
        }
        error.map_or(Ok(()), Err)
    })?;
    for entry in directories {
        sync(&entry.open_checked(true)?, &entry.path, true)?;
    }
    Ok(())
}

// Never open a symlink target or block on a FIFO/device. Directory fsync
// persists their entries; hard-linked regular files need only one data drain.
fn sync_tree(path: &Path, seen: &mut HashSet<(u64, u64)>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        let directory = open(path, true)?;
        for entry in fs::read_dir(path)? {
            sync_tree(&entry?.path(), seen)?;
        }
        directory.sync_all()
    } else if metadata.is_file() && seen.insert((metadata.dev(), metadata.ino())) {
        let file = open(path, false)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("stage entry changed during persistence"));
        }
        file.sync_all()
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OverlayCore, OverlayLayout, apply::apply_overlay, fingerprint_at};
    use pvisor_core::overlay::{OverlayRecord, OverlayState, OverlayUpper};

    #[test]
    fn worker_configuration_is_bounded_and_explicit() {
        assert_eq!(sync_workers(None).unwrap(), 1);
        for value in [1, 2, 4, 8] {
            assert_eq!(sync_workers(Some(value.to_string().into())).unwrap(), value);
        }
        for value in ["", "0", "3", "9", "-1", "unlimited"] {
            assert!(sync_workers(Some(value.into())).is_err());
        }
    }

    #[test]
    fn parallel_sync_is_bounded_deduplicates_and_orders_directories_after_files() {
        use std::sync::{Barrier, Mutex};
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        fs::create_dir_all(upper.join("child/grandchild")).unwrap();
        for index in 0..8 {
            fs::write(upper.join(format!("child/grandchild/{index}")), b"payload").unwrap();
        }
        fs::hard_link(upper.join("child/grandchild/0"), upper.join("alias")).unwrap();
        std::os::unix::fs::symlink("/does-not-exist", upper.join("symlink")).unwrap();
        let fifo =
            std::ffi::CString::new(upper.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let first_wave = Barrier::new(4);
        let started = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let directories = Mutex::new(Vec::new());
        sync_upper_parallel(&upper, 4, &|file, path, directory| {
            if directory {
                assert_eq!(completed.load(Ordering::SeqCst), 8);
                assert_eq!(active.load(Ordering::SeqCst), 0);
                directories.lock().unwrap().push(path.to_owned());
            } else {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                if started.fetch_add(1, Ordering::SeqCst) < 4 {
                    first_wave.wait();
                }
            }
            file.sync_all()?;
            if !directory {
                active.fetch_sub(1, Ordering::SeqCst);
                completed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 8);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(
            *directories.lock().unwrap(),
            vec![upper.join("child/grandchild"), upper.join("child"), upper]
        );
    }

    #[test]
    fn parallel_file_error_joins_workers_and_never_syncs_directories_or_seals() {
        use std::sync::Barrier;
        let temp = tempfile::tempdir().unwrap();
        let (core, record) = fixture(temp.path(), StageDurability::Checkpoint);
        for index in 0..4 {
            fs::write(record.upper.path().join(index.to_string()), b"payload").unwrap();
        }
        drop(core);
        let journal = temp.path().join("preimages");
        let barrier = Barrier::new(4);
        let joined_work = AtomicUsize::new(0);
        let directories = AtomicUsize::new(0);
        let result = seal_with_upper(record.upper.path(), &journal, |upper| {
            sync_upper_parallel(upper, 4, &|file, path, directory| {
                if directory {
                    directories.fetch_add(1, Ordering::SeqCst);
                    return file.sync_all();
                }
                barrier.wait();
                let result = if path.file_name().unwrap() == "0" {
                    Err(io::Error::from_raw_os_error(libc::EIO))
                } else {
                    file.sync_all()
                };
                joined_work.fetch_add(1, Ordering::SeqCst);
                result
            })
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EIO));
        assert_eq!(joined_work.load(Ordering::SeqCst), 4);
        assert_eq!(directories.load(Ordering::SeqCst), 0);
        assert!(!journal.join(SEAL).exists());
        assert!(require_sealed(&journal).is_err());
    }

    #[test]
    fn parallel_directory_failure_or_worker_panic_never_publishes_seal() {
        for panic_worker in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let (core, record) = fixture(temp.path(), StageDurability::Checkpoint);
            fs::write(record.upper.path().join("payload"), b"data").unwrap();
            drop(core);
            let journal = temp.path().join("preimages");
            let result = seal_with_upper(record.upper.path(), &journal, |upper| {
                sync_upper_parallel(upper, 2, &|file, _, directory| {
                    assert!(!panic_worker, "injected worker panic");
                    if directory {
                        Err(io::Error::from_raw_os_error(libc::EIO))
                    } else {
                        file.sync_all()
                    }
                })
            });
            assert!(result.is_err());
            assert!(!journal.join(SEAL).exists());
            assert!(begin(&journal, StageDurability::Checkpoint).is_err());
        }
    }

    #[test]
    fn parallel_seal_supports_empty_uppers_and_both_durability_modes() {
        for durability in [StageDurability::Strict, StageDurability::Checkpoint] {
            for populated in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let (core, record) = fixture(temp.path(), durability);
                if populated {
                    fs::write(core.copy_up(Path::new("value")).unwrap(), b"staged").unwrap();
                }
                drop(core);
                let journal = temp.path().join("preimages");
                seal_with_upper(record.upper.path(), &journal, |upper| {
                    sync_upper_parallel(upper, 4, &|file, _, _| file.sync_all())
                })
                .unwrap();
                require_sealed(&journal).unwrap();
            }
        }
    }

    #[test]
    fn parallel_inventory_rejects_inode_replacement_before_sync() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("value");
        fs::write(&path, b"original").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = UpperEntry {
            path: path.clone(),
            identity: (metadata.dev(), metadata.ino()),
        };
        fs::rename(&path, temp.path().join("old")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(entry.open_checked(false).is_err());
    }

    fn fixture(root: &Path, durability: StageDurability) -> (OverlayCore, OverlayRecord) {
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("value"), b"original").unwrap();
        let journal = root.join("preimages");
        begin(&journal, durability).unwrap();
        let core = OverlayCore::new_for_layout_with_compact_preimages(
            OverlayLayout::new(vec![target.clone()], target.clone()).unwrap(),
            root.join("upper"),
            Some(root.join("work")),
            vec![],
            Some(journal),
        )
        .unwrap();
        let record = OverlayRecord {
            id: "boundary".into(),
            generation: 0,
            target,
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: root.join("upper"),
                work_dir: root.join("work"),
            },
            merged_dir: root.join("merged"),
            stage_dir: root.to_owned(),
            excluded_paths: vec![],
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        (core, record)
    }

    #[test]
    fn apply_requires_a_seal_and_preserves_exact_content_conflicts_in_both_modes() {
        for durability in [StageDurability::Checkpoint, StageDurability::Strict] {
            for external_edit in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let (core, mut record) = fixture(temp.path(), durability);
                let original = fingerprint_at(&record.target, Path::new("value")).unwrap();
                core.observe_read(Path::new("value")).unwrap();
                if external_edit {
                    fs::write(record.target.join("value"), b"external").unwrap();
                }
                fs::write(core.copy_up(Path::new("value")).unwrap(), b"staged").unwrap();
                drop(core);
                let journal = temp.path().join("preimages");
                assert_eq!(load_preimages(&journal).unwrap()[0].state, original);
                assert!(
                    apply_overlay(&mut record)
                        .unwrap_err()
                        .to_string()
                        .contains("STAGE_INCOMPLETE")
                );
                seal(record.upper.path(), &journal).unwrap();
                require_sealed(&journal).unwrap();
                if external_edit {
                    assert!(apply_overlay(&mut record).is_err());
                    assert_eq!(fs::read(record.target.join("value")).unwrap(), b"external");
                } else {
                    apply_overlay(&mut record).unwrap();
                    assert_eq!(fs::read(record.target.join("value")).unwrap(), b"staged");
                    apply_overlay(&mut record).unwrap();
                }
            }
        }
    }

    #[test]
    fn interrupted_stages_cannot_be_reopened_as_completed_and_reuse_removes_the_seal() {
        let temp = tempfile::tempdir().unwrap();
        let (core, record) = fixture(temp.path(), StageDurability::Checkpoint);
        fs::write(core.copy_up(Path::new("value")).unwrap(), b"staged").unwrap();
        drop(core);
        let journal = temp.path().join("preimages");
        assert!(begin(&journal, StageDurability::Checkpoint).is_err());
        seal(record.upper.path(), &journal).unwrap();
        let reopened = OverlayCore::open_existing(
            vec![record.target.clone()],
            record.upper.upper_dir.clone(),
            Some(record.upper.work_dir.clone()),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        assert_eq!(
            reopened
                .copy_up(Path::new("value"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        drop(reopened);
        assert!(begin(&journal, StageDurability::Strict).is_err());
        require_sealed(&journal).unwrap();
        begin(&journal, StageDurability::Checkpoint).unwrap();
        assert!(!journal.join(SEAL).exists());
        assert!(require_sealed(&journal).is_err());
    }

    #[test]
    fn seal_rejects_truncated_or_corrupt_logs_without_repair_or_acknowledgement() {
        use std::io::Write;
        for corruption in [b"PVR2".as_slice(), b"bad-header".as_slice()] {
            let temp = tempfile::tempdir().unwrap();
            let (core, record) = fixture(temp.path(), StageDurability::Checkpoint);
            core.observe_read(Path::new("value")).unwrap();
            drop(core);
            let journal = temp.path().join("preimages");
            let log = journal.join(crate::core::PREIMAGE_LOG_NAME);
            OpenOptions::new()
                .append(true)
                .open(&log)
                .unwrap()
                .write_all(corruption)
                .unwrap();
            let before = fs::read(&log).unwrap();
            assert!(seal(record.upper.path(), &journal).is_err());
            assert_eq!(fs::read(log).unwrap(), before);
            assert!(!journal.join(SEAL).exists());
            assert!(begin(&journal, StageDurability::Checkpoint).is_err());
        }
    }

    #[test]
    fn persistence_failure_does_not_publish_completion_and_symlinks_are_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let (core, record) = fixture(temp.path(), StageDurability::Checkpoint);
        drop(core);
        let journal = temp.path().join("preimages");
        assert!(seal(&temp.path().join("missing-upper"), &journal).is_err());
        assert!(!journal.join(SEAL).exists());
        let outside = temp.path().join("outside");
        fs::write(&outside, b"external data").unwrap();
        std::os::unix::fs::symlink(&outside, record.upper.path().join("link")).unwrap();
        seal(record.upper.path(), &journal).unwrap();
        assert_eq!(fs::read(outside).unwrap(), b"external data");
        fs::remove_file(journal.join(SEAL)).unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), journal.join(SEAL)).unwrap();
        assert!(require_sealed(&journal).is_err());
        assert!(begin(&journal, StageDurability::Checkpoint).is_err());
        fs::write(journal.join(POLICY), b"unknown version").unwrap();
        assert!(policy(&journal).is_err());
    }

    #[test]
    fn legacy_stages_keep_the_strict_contract_without_new_completion_requirements() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(policy(temp.path()).unwrap(), StageDurability::Strict);
        require_sealed(temp.path()).unwrap();
        assert!(!temp.path().join(POLICY).exists());
        fs::create_dir(temp.path().join("entries")).unwrap();
        begin(temp.path(), StageDurability::Checkpoint).unwrap();
        assert_eq!(policy(temp.path()).unwrap(), StageDurability::Strict);
        assert!(!temp.path().join(POLICY).exists());
    }
}
