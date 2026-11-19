//! Fixed publication faults retain independently observed effects and recovery rights.
use pvisor_overlay_core::{
    OverlayCore,
    apply::{
        ApplyRecordState, ApplySelection, OverlayError, OverlayRecord, OverlayState, OverlayUpper,
        apply_overlay_selected, load_apply_records, recover_pending_applies,
    },
    stage::{self, StageDurability},
};
use std::{fs, path::Path};

fn fixture() -> (tempfile::TempDir, OverlayRecord) {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    let stage_dir = root.path().join("stage");
    let upper = stage_dir.join("upper");
    let work = stage_dir.join("work");
    let journal = stage_dir.join("preimages");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("file"), b"host baseline").unwrap();
    stage::begin(&journal, StageDurability::Strict).unwrap();
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![target.clone()],
        upper.clone(),
        Some(work.clone()),
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    fs::write(core.copy_up(Path::new("file")).unwrap(), b"staged effect").unwrap();
    drop(core);
    stage::complete(&upper, &journal).unwrap();
    let record = OverlayRecord {
        id: "failure-contract".into(),
        generation: 0,
        target,
        baseline_lower: None,
        upper: OverlayUpper {
            upper_dir: upper,
            work_dir: work,
        },
        merged_dir: stage_dir.join("merged"),
        stage_dir,
        excluded_paths: vec![],
        access_policy: Default::default(),
        auto_apply: false,
        auto_discard: false,
        protect_target: false,
        state: OverlayState::Staged,
    };
    (root, record)
}

#[rstest::rstest]
#[case::intent_not_confirmed(false)]
#[case::target_applied(true)]
fn failed_publication_exposes_identity_phase_and_original_error(#[case] after_target: bool) {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // Root bypasses the fixture's DAC obstruction.
    }
    let (_root, mut record) = fixture();
    let blocked = if after_target {
        record.upper.path().to_path_buf()
    } else {
        record.stage_dir.clone()
    };
    let original = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
    let target = record.target.clone();
    let result = apply_overlay_selected(&mut record, &[target], &ApplySelection::default());
    fs::set_permissions(&blocked, original).unwrap();
    let OverlayError::ApplyFailure(failure) = result.unwrap_err() else {
        panic!("missing apply context")
    };
    assert!(!failure.apply_id.is_empty());
    if after_target {
        assert_eq!(
            failure.last_confirmed,
            Some(ApplyRecordState::TargetApplied)
        );
        assert!(
            matches!(*failure.source, OverlayError::Io(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied)
        );
        let records = load_apply_records(&record.stage_dir).unwrap();
        assert_eq!(records[0].apply_id, failure.apply_id);
        assert_eq!(records[0].state, ApplyRecordState::TargetApplied);
        assert_eq!(
            fs::read(record.target.join("file")).unwrap(),
            b"staged effect"
        );
        assert_eq!(
            fs::read(record.upper.path().join("file")).unwrap(),
            b"staged effect"
        );
        fs::write(record.target.join("file"), b"later host edit").unwrap();
        let target = record.target.clone();
        assert_eq!(
            recover_pending_applies(&mut record, &[target]).unwrap(),
            vec![failure.apply_id]
        );
        assert_eq!(
            fs::read(record.target.join("file")).unwrap(),
            b"later host edit"
        );
        assert_eq!(
            load_apply_records(&record.stage_dir).unwrap()[0].state,
            ApplyRecordState::Committed
        );
        assert!(!record.upper.path().join("file").exists());
    } else {
        assert_eq!(failure.last_confirmed, None);
        assert!(matches!(*failure.source, OverlayError::Persist(_)));
        assert_eq!(
            fs::read(record.target.join("file")).unwrap(),
            b"host baseline"
        );
        assert_eq!(
            fs::read(record.upper.path().join("file")).unwrap(),
            b"staged effect"
        );
        assert!(load_apply_records(&record.stage_dir).unwrap().is_empty());
    }
}
