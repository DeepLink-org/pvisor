//! Owned stage persistence. The supervisor must hold its stage lease and stop
//! all writers before sealing. A seal is a completion boundary, not a snapshot
//! of an upper that continues to change; live checkpoints seal their own copy.
use crate::{load_preimages, preimage_journal_is_complete};
pub use pvisor_core::overlay::StageDurability;
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
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
        pvisor_journal::atomic_write(&journal.join(POLICY), bytes, 0o600)
            .map_err(io::Error::other)?;
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
pub fn seal(upper: &Path, journal: &Path) -> io::Result<()> {
    if !managed(journal)? {
        return Ok(());
    }
    if !preimage_journal_is_complete(journal) {
        return Err(invalid("stage journal initialization is incomplete"));
    }
    sync_journal(journal)?;
    open(upper, true)?;
    sync_tree(upper, &mut HashSet::new())?;
    if let Some(parent) = upper.parent() {
        open(parent, true)?.sync_all()?;
    }
    pvisor_journal::atomic_write(&journal.join(SEAL), SEALED, 0o600).map_err(io::Error::other)?;
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
