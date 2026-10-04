//! Conflict guarantees through the public core/apply boundary, without FUSE.
use pvisor_overlay_core::apply::{
    OverlayRecord, OverlayState, OverlayUpper, apply_overlay, write_overlay_record,
};
use pvisor_overlay_core::{
    OverlayCore, OverlayLayout, PathFingerprint, fingerprint_at, load_preimages,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Stage {
    _temp: tempfile::TempDir,
    record: OverlayRecord,
    lowers: Vec<PathBuf>,
}
impl Stage {
    fn new(frozen: bool, composed: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let baseline = temp.path().join("baseline");
        let top = temp.path().join("top");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("value"), b"original").unwrap();
        fs::write(target.join("other"), b"unrelated").unwrap();
        let baseline_lower = frozen.then(|| {
            fs::create_dir(&baseline).unwrap();
            fs::copy(target.join("value"), baseline.join("value")).unwrap();
            fs::copy(target.join("other"), baseline.join("other")).unwrap();
            baseline.clone()
        });
        let mut lowers = vec![];
        if composed {
            fs::create_dir(&top).unwrap();
            fs::write(top.join("value"), b"visible extra layer").unwrap();
            lowers.push(top);
        }
        lowers.push(baseline_lower.as_ref().unwrap_or(&target).clone());
        let stage = temp.path().join("stage");
        let record = OverlayRecord {
            id: "read-conflict".into(),
            generation: 0,
            target,
            baseline_lower,
            upper: OverlayUpper {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: vec![],
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        Self {
            _temp: temp,
            record,
            lowers,
        }
    }
    fn core(&self, restoring: bool) -> OverlayCore {
        let layout = OverlayLayout::with_baseline(
            self.lowers.clone(),
            self.record.target.clone(),
            self.record.baseline_lower.as_deref(),
        )
        .unwrap();
        let open = if restoring {
            OverlayCore::open_existing_for_layout
        } else {
            OverlayCore::new_for_layout
        };
        let core = open(
            layout,
            self.record.upper.upper_dir.clone(),
            Some(self.record.upper.work_dir.clone()),
            vec![],
            Some(self.record.stage_dir.join("preimages")),
        )
        .unwrap();
        write_overlay_record(&self.record).unwrap();
        core
    }
    fn rejected(&mut self) {
        let error = apply_overlay(&mut self.record).unwrap_err();
        assert!(
            error.to_string().contains("target changed after staging"),
            "{error}"
        );
        assert_eq!(
            fs::read(self.record.target.join("value")).unwrap(),
            b"host edit"
        );
    }
}

#[test]
fn first_content_read_protects_before_and_after_copy_up_for_live_and_frozen_layouts() {
    for frozen in [false, true] {
        for composed in [false, true] {
            for edit_after_copy_up in [false, true] {
                let mut stage = Stage::new(frozen, composed);
                let core = stage.core(false);
                let path = Path::new("value");
                core.observe_read(path).unwrap();
                let visible = fs::read(core.resolve(path).unwrap().path).unwrap();
                assert_eq!(
                    visible,
                    if composed {
                        b"visible extra layer".as_slice()
                    } else {
                        b"original"
                    }
                );
                if !edit_after_copy_up {
                    fs::write(stage.record.target.join(path), b"host edit").unwrap();
                }
                let upper = core.copy_up(path).unwrap();
                if edit_after_copy_up {
                    fs::write(stage.record.target.join(path), b"host edit").unwrap();
                }
                fs::write(upper, b"agent edit based on old read").unwrap();
                drop(core);
                stage.rejected();
            }
        }
    }
}

#[test]
fn frozen_mutation_uses_target_baseline_even_without_a_prior_observation() {
    for composed in [false, true] {
        let mut stage = Stage::new(true, composed);
        let core = stage.core(false);
        fs::write(stage.record.target.join("value"), b"host edit").unwrap();
        fs::write(core.copy_up(Path::new("value")).unwrap(), b"agent edit").unwrap();
        drop(core);
        stage.rejected();
    }
}

#[test]
fn read_then_delete_rename_and_metadata_changes_keep_the_original_target() {
    for operation in ["delete", "rename", "metadata"] {
        let mut stage = Stage::new(false, false);
        let core = stage.core(false);
        let path = Path::new("value");
        core.observe_read(path).unwrap();
        fs::write(stage.record.target.join(path), b"host edit").unwrap();
        match operation {
            "delete" => core.remove(path, false).unwrap(),
            "rename" => core.rename(path, Path::new("moved"), false).unwrap(),
            _ => {
                let upper = core.prepare_metadata_change(path).unwrap();
                fs::set_permissions(upper, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        drop(core);
        stage.rejected();
    }
}

#[test]
fn negative_lookup_survives_reopen_and_rejects_host_creation() {
    let mut stage = Stage::new(false, false);
    let core = stage.core(false);
    let missing = Path::new("missing");
    assert_eq!(
        core.metadata(missing).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(
        load_preimages(&stage.record.stage_dir.join("preimages"))
            .unwrap()
            .iter()
            .any(|p| p.relative_path() == missing && p.state == PathFingerprint::Absent)
    );
    drop(core);
    fs::write(stage.record.target.join(missing), b"new host file").unwrap();
    let core = stage.core(true);
    // The write path may resolve the new live lower, but its first observed
    // target absence must still prevent acceptance of the candidate.
    fs::write(core.copy_up(missing).unwrap(), b"agent file").unwrap();
    drop(core);
    assert!(apply_overlay(&mut stage.record).is_err());
    assert_eq!(
        fs::read(stage.record.target.join(missing)).unwrap(),
        b"new host file"
    );
}

#[test]
fn read_only_observation_survives_reopen_before_first_mutation() {
    let mut stage = Stage::new(false, false);
    let core = stage.core(false);
    core.observe_read(Path::new("value")).unwrap();
    drop(core);
    fs::write(stage.record.target.join("value"), b"host edit").unwrap();
    let core = stage.core(true);
    fs::write(core.copy_up(Path::new("value")).unwrap(), b"agent edit").unwrap();
    drop(core);
    stage.rejected();
}

#[test]
fn mutation_only_uses_current_live_target_and_unrelated_read_does_not_block_apply() {
    let mut stage = Stage::new(false, false);
    let core = stage.core(false);
    core.observe_read(Path::new("other")).unwrap();
    fs::write(stage.record.target.join("other"), b"host unrelated edit").unwrap();
    fs::write(
        stage.record.target.join("value"),
        b"host before first mutation",
    )
    .unwrap();
    let expected = fingerprint_at(&stage.record.target, Path::new("value")).unwrap();
    fs::write(core.copy_up(Path::new("value")).unwrap(), b"agent mutation").unwrap();
    assert!(
        load_preimages(&stage.record.stage_dir.join("preimages"))
            .unwrap()
            .iter()
            .any(|p| p.relative_path() == Path::new("value") && p.state == expected)
    );
    drop(core);
    apply_overlay(&mut stage.record).unwrap();
    assert_eq!(
        fs::read(stage.record.target.join("value")).unwrap(),
        b"agent mutation"
    );
    assert_eq!(
        fs::read(stage.record.target.join("other")).unwrap(),
        b"host unrelated edit"
    );
}

#[test]
fn positive_metadata_walk_does_not_create_content_observations() {
    let stage = Stage::new(false, false);
    for i in 0..1024 {
        fs::write(
            stage.record.target.join(format!("file-{i}")),
            b"metadata probe",
        )
        .unwrap();
    }
    let core = stage.core(false);
    for name in core.list_names(Path::new("")).unwrap() {
        core.metadata(Path::new(&name)).unwrap();
    }
    assert!(
        load_preimages(&stage.record.stage_dir.join("preimages"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn composed_visible_file_keeps_an_absent_target_baseline() {
    for frozen in [false, true] {
        let mut stage = Stage::new(frozen, true);
        fs::remove_file(stage.record.target.join("value")).unwrap();
        if let Some(baseline) = &stage.record.baseline_lower {
            fs::remove_file(baseline.join("value")).unwrap();
        }
        let core = stage.core(false);
        core.observe_read(Path::new("value")).unwrap();
        assert_eq!(
            fs::read(core.resolve(Path::new("value")).unwrap().path).unwrap(),
            b"visible extra layer"
        );
        fs::write(stage.record.target.join("value"), b"host edit").unwrap();
        fs::write(core.copy_up(Path::new("value")).unwrap(), b"agent edit").unwrap();
        drop(core);
        stage.rejected();
    }
}

#[test]
fn rename_destination_negative_lookup_prevents_overwriting_host_creation() {
    let mut stage = Stage::new(false, false);
    let core = stage.core(false);
    let destination = Path::new("moved");
    assert!(core.metadata(destination).is_err());
    fs::write(stage.record.target.join(destination), b"host destination").unwrap();
    core.rename(Path::new("value"), destination, false).unwrap();
    drop(core);
    assert!(apply_overlay(&mut stage.record).is_err());
    assert_eq!(
        fs::read(stage.record.target.join(destination)).unwrap(),
        b"host destination"
    );
    assert_eq!(
        fs::read(stage.record.target.join("value")).unwrap(),
        b"original"
    );
}

#[test]
fn corrupt_read_observation_fails_before_upper_mutation() {
    let stage = Stage::new(false, false);
    let core = stage.core(false);
    core.observe_read(Path::new("value")).unwrap();
    let entry = fs::read_dir(stage.record.stage_dir.join("preimages/entries"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(entry, b"interrupted observation").unwrap();
    assert_eq!(
        core.copy_up(Path::new("value")).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert!(!stage.record.upper.upper_dir.join("value").exists());
    assert_eq!(
        fs::read(stage.record.target.join("value")).unwrap(),
        b"original"
    );
}

#[test]
fn frozen_partial_apply_then_reopen_rejects_rewriting_from_the_old_view() {
    use pvisor_overlay_core::apply::{ApplySelection, apply_overlay_selected};
    let mut stage = Stage::new(true, false);
    let core = stage.core(false);
    fs::write(
        core.copy_up(Path::new("value")).unwrap(),
        b"first accepted edit",
    )
    .unwrap();
    fs::write(
        core.copy_up(Path::new("other")).unwrap(),
        b"pending other edit",
    )
    .unwrap();
    drop(core);
    let selection = ApplySelection {
        paths: vec![PathBuf::from("value")],
        ..Default::default()
    };
    apply_overlay_selected(&mut stage.record, &stage.lowers, &selection).unwrap();
    assert_eq!(stage.record.state, OverlayState::Staged);
    assert_eq!(
        fs::read(stage.record.target.join("value")).unwrap(),
        b"first accepted edit"
    );
    let core = stage.core(true);
    core.observe_read(Path::new("value")).unwrap();
    // Pruning upper does not update the frozen lower. Keep this conservative
    // rejection: rebasing only the fingerprint would accept a stale rewrite.
    assert_eq!(
        fs::read(core.resolve(Path::new("value")).unwrap().path).unwrap(),
        b"original"
    );
    fs::write(
        core.copy_up(Path::new("value")).unwrap(),
        b"rewrite based on stale original",
    )
    .unwrap();
    drop(core);
    assert!(apply_overlay_selected(&mut stage.record, &stage.lowers, &selection).is_err());
    assert_eq!(
        fs::read(stage.record.target.join("value")).unwrap(),
        b"first accepted edit"
    );
    assert_eq!(
        fs::read(stage.record.upper.upper_dir.join("other")).unwrap(),
        b"pending other edit"
    );
}

#[test]
fn denied_hardlink_alias_is_not_a_negative_lookup_or_content_observation() {
    let stage = Stage::new(false, false);
    fs::hard_link(
        stage.record.target.join("value"),
        stage.record.target.join("alias"),
    )
    .unwrap();
    let core = stage.core(false).with_access_policy(
        &pvisor_overlay_core::FileAccessPolicy::new(vec!["value".into()], vec![]).unwrap(),
    );
    assert!(core.resolve(Path::new("alias")).is_none());
    assert_eq!(
        core.resolve_checked(Path::new("alias"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EACCES)
    );
    assert_eq!(
        core.metadata(Path::new("alias"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EACCES)
    );
    assert_eq!(
        core.observe_read(Path::new("alias"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EACCES)
    );
    assert!(
        load_preimages(&stage.record.stage_dir.join("preimages"))
            .unwrap()
            .is_empty()
    );
    assert!(!stage.record.upper.upper_dir.join("alias").exists());
}
