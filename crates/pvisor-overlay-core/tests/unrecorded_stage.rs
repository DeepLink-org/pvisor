use pvisor_overlay_core::{
    OverlayCore, OverlayLayout,
    profile::Profile,
    stage::{self, StageDurability, StageRecording},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

fn open(root: &Path, recording: StageRecording) -> (OverlayCore, PathBuf, PathBuf) {
    let lower = root.join("lower");
    fs::create_dir_all(lower.join("dir")).unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    fs::write(lower.join("dir/child"), b"child").unwrap();
    let metadata = root.join("preimages");
    stage::begin_with_recording(&metadata, StageDurability::Ephemeral, recording).unwrap();
    let core = OverlayCore::new_for_layout_with_compact_preimages(
        OverlayLayout::new(vec![lower.clone()], lower.clone()).unwrap(),
        root.join("upper"),
        Some(root.join("work")),
        vec![],
        Some(metadata.clone()),
    )
    .unwrap()
    .with_profile(Profile::enabled("unrecorded-contract"));
    (core, lower, metadata)
}

#[test]
fn unrecorded_stage_preserves_mutations_and_skips_observations() {
    for recording in [StageRecording::On, StageRecording::Off] {
        let root = tempfile::tempdir().unwrap();
        let (core, lower, journal) = open(root.path(), recording);
        core.observe_read(Path::new("file")).unwrap();
        assert!(
            core.metadata_for_backing_lookup_optional(Path::new("missing"))
                .unwrap()
                .is_none()
        );
        let path = core.copy_up(Path::new("file")).unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        file.write_all(b"changed").unwrap();
        core.sync_preimages().unwrap();
        file.sync_all().unwrap();
        file.sync_data().unwrap();
        core.hard_link(Path::new("file"), Path::new("alias"))
            .unwrap();
        core.rename(Path::new("file"), Path::new("renamed"), false)
            .unwrap();
        core.rename(Path::new("dir"), Path::new("moved"), false)
            .unwrap();
        core.remove(Path::new("moved/child"), false).unwrap();
        core.create_dir(Path::new("newdir"), 0o750).unwrap();
        core.create_symlink(Path::new("link"), Path::new("renamed"))
            .unwrap();
        assert_eq!(fs::read(core.upper().join("alias")).unwrap(), b"changed");
        assert_eq!(fs::read(core.upper().join("renamed")).unwrap(), b"changed");
        assert_eq!(
            fs::read_link(core.upper().join("link")).unwrap(),
            Path::new("renamed")
        );
        assert!(core.resolve_checked(Path::new("file")).unwrap().is_none());
        assert!(
            core.resolve_checked(Path::new("moved/child"))
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
        assert_eq!(fs::read(lower.join("dir/child")).unwrap(), b"child");
        let report = core.profile_report().unwrap();
        let fingerprints = report
            .measurements
            .get("fingerprinted_paths")
            .map_or(0, |m| m.units);
        assert_eq!(fingerprints > 0, recording == StageRecording::On);
        drop(core);
        stage::complete(&root.path().join("upper"), &journal).unwrap();
        assert!(journal.join("completed-volatile-v1").is_file());
        assert!(!journal.join("sealed-v1").exists());
        if recording == StageRecording::Off {
            assert!(!journal.join("entries").exists());
            assert!(!journal.join("log-v2").exists());
            assert!(!journal.join("complete-v1").exists());
        }
        assert!(stage::require_persistent(&journal).is_err());
        assert!(stage::begin(&journal, StageDurability::Checkpoint).is_err());
        let reopened = OverlayCore::open_existing(
            vec![lower],
            root.path().join("upper"),
            Some(root.path().join("work")),
            vec![],
            Some(journal),
        )
        .unwrap();
        assert_eq!(
            reopened
                .remove(Path::new("alias"), false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        assert_eq!(
            reopened
                .rename(Path::new("newdir"), Path::new("otherdir"), false)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        assert!(reopened.upper().join("alias").exists());
        assert!(reopened.upper().join("newdir").exists());
        assert_eq!(
            reopened
                .copy_up(Path::new("renamed"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
    }
}

#[test]
fn unrecorded_stage_still_enforces_paths_policy_and_concurrent_copy_up() {
    let root = tempfile::tempdir().unwrap();
    let (core, lower, journal) = open(root.path(), StageRecording::Off);
    let policy: pvisor_overlay_core::FileAccessPolicy =
        serde_json::from_value(serde_json::json!({"deny": ["dir", "dir/**"]})).unwrap();
    let core = std::sync::Arc::new(core.with_access_policy(&policy));
    assert!(core.copy_up(Path::new("../escape")).is_err());
    assert!(
        core.metadata_for_backing_lookup(Path::new("dir/child"))
            .is_err()
    );
    assert!(core.copy_up(Path::new("dir/child")).is_err());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let core = core.clone();
            std::thread::spawn(move || core.copy_up(Path::new("file")).unwrap())
        })
        .collect();
    for thread in threads {
        assert_eq!(fs::read(thread.join().unwrap()).unwrap(), b"original");
    }
    assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
    assert!(!journal.join("entries").exists());
}

#[test]
fn unrecorded_stage_rejects_incompatible_or_mixed_state() {
    let root = tempfile::tempdir().unwrap();
    let metadata = root.path().join("metadata");
    for mode in [StageDurability::Checkpoint, StageDurability::Strict] {
        assert!(stage::begin_with_recording(&metadata, mode, StageRecording::Off).is_err());
        assert!(!metadata.exists());
    }
    let (core, lower, metadata) = open(root.path(), StageRecording::Off);
    drop(core);
    fs::write(metadata.join("sealed-v1"), b"pvisor.stage.sealed/1\n").unwrap();
    assert!(stage::require_sealed(&metadata).is_err());
    assert!(
        stage::begin_with_recording(&metadata, StageDurability::Ephemeral, StageRecording::On)
            .is_err()
    );
    fs::create_dir(metadata.join("entries")).unwrap();
    assert!(stage::complete(&root.path().join("upper"), &metadata).is_err());
    assert!(
        OverlayCore::open_existing(
            vec![lower],
            root.path().join("upper"),
            None,
            vec![],
            Some(metadata)
        )
        .is_err()
    );
}

#[test]
fn unrecorded_stage_policy_is_explicit_immutable_and_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    for durability in [
        StageDurability::Checkpoint,
        StageDurability::Strict,
        StageDurability::Ephemeral,
    ] {
        let journal = root.path().join(format!("{durability:?}"));
        stage::begin(&journal, durability).unwrap();
        assert_eq!(stage::recording(&journal).unwrap(), StageRecording::On);
        assert!(
            stage::begin_with_recording(&journal, StageDurability::Ephemeral, StageRecording::Off)
                .is_err()
        );
        assert_eq!(stage::policy(&journal).unwrap(), durability);
    }
    let journal = root.path().join("corrupt");
    stage::begin_with_recording(&journal, StageDurability::Ephemeral, StageRecording::Off).unwrap();
    fs::write(journal.join("durability-v1"), b"unknown\n").unwrap();
    assert!(stage::policy(&journal).is_err());
    assert!(stage::recording(&journal).is_err());
    fs::remove_file(journal.join("durability-v1")).unwrap();
    let outside = root.path().join("policy");
    fs::write(&outside, b"pvisor.stage.ephemeral-unrecorded/1\n").unwrap();
    std::os::unix::fs::symlink(outside, journal.join("durability-v1")).unwrap();
    assert!(stage::recording(&journal).is_err());
}
