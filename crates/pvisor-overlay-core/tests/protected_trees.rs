//! Hidden descendants and imported metadata must not acquire mutation authority.
use pvisor_overlay_core::{
    FileAccessPolicy, OverlayCore,
    apply::{
        ApplySelection, OverlayRecord, OverlayState, OverlayUpper, apply_overlay,
        apply_overlay_selected, plan_overlay_apply, restore_overlay_upper,
    },
    stage, sys,
};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

fn record(root: &Path, excluded: &str) -> OverlayRecord {
    let stage = root.join("stage");
    OverlayRecord {
        id: "protected-tree".into(),
        generation: 0,
        target: root.join("lower"),
        baseline_lower: None,
        upper: OverlayUpper {
            upper_dir: stage.join("upper"),
            work_dir: stage.join("work"),
        },
        merged_dir: stage.join("merged"),
        stage_dir: stage,
        excluded_paths: vec![PathBuf::from(excluded)],
        access_policy: Default::default(),
        auto_apply: false,
        auto_discard: false,
        protect_target: false,
        state: OverlayState::Staged,
    }
}

#[test]
fn excluded_descendants_block_parent_removal_and_both_sides_of_rename() {
    for operation in ["remove", "rename source", "rename target", "exchange"] {
        let temp = tempfile::tempdir().unwrap();
        let record = record(temp.path(), "dir/secret");
        fs::create_dir_all(record.target.join("dir")).unwrap();
        fs::create_dir(record.target.join("other")).unwrap();
        fs::write(record.target.join("dir/secret"), b"protected").unwrap();
        fs::write(record.target.join("dir/normal"), b"remove me").unwrap();
        let core = OverlayCore::new_with_exclusions(
            vec![record.target.clone()],
            record.upper.upper_dir.clone(),
            Some(record.upper.work_dir.clone()),
            record.excluded_paths.clone(),
        )
        .unwrap();
        core.remove(Path::new("dir/normal"), false).unwrap();
        let result = match operation {
            "remove" => core.remove(Path::new("dir"), true),
            "rename source" => core.rename(Path::new("dir"), Path::new("renamed"), false),
            "rename target" => core.rename(Path::new("other"), Path::new("dir"), false),
            "exchange" => core.exchange(Path::new("dir"), Path::new("other")),
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{operation} changed a protected tree");
        assert_eq!(
            fs::read(record.target.join("dir/secret")).unwrap(),
            b"protected"
        );
        assert!(!record.upper.upper_dir.join(".wh.dir").exists());
    }
}

#[test]
fn apply_independently_rejects_old_staged_deletions_replacements_and_opaque_trees() {
    for operation in ["delete", "replace", "opaque", "direct"] {
        let temp = tempfile::tempdir().unwrap();
        let mut record = record(temp.path(), "dir/secret");
        fs::create_dir_all(record.target.join("dir")).unwrap();
        fs::write(record.target.join("dir/secret"), b"protected").unwrap();
        fs::create_dir_all(&record.upper.upper_dir).unwrap();
        match operation {
            "delete" => fs::write(record.upper.upper_dir.join(".wh.dir"), b"").unwrap(),
            "replace" => fs::write(record.upper.upper_dir.join("dir"), b"replacement").unwrap(),
            "opaque" => {
                fs::create_dir(record.upper.upper_dir.join("dir")).unwrap();
                fs::write(record.upper.upper_dir.join("dir/.wh..wh..opq"), b"").unwrap();
            }
            "direct" => {
                fs::create_dir(record.upper.upper_dir.join("dir")).unwrap();
                fs::write(record.upper.upper_dir.join("dir/secret"), b"replacement").unwrap();
            }
            _ => unreachable!(),
        }
        let error = plan_overlay_apply(
            &record,
            std::slice::from_ref(&record.target),
            &ApplySelection::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("excluded"),
            "{operation}: {error}"
        );
        assert!(apply_overlay(&mut record).is_err(), "{operation}");
        assert_eq!(
            fs::read(record.target.join("dir/secret")).unwrap(),
            b"protected"
        );
    }
}

#[test]
fn visible_sibling_changes_still_apply_without_touching_the_excluded_file() {
    let temp = tempfile::tempdir().unwrap();
    let mut record = record(temp.path(), "dir/secret");
    fs::create_dir_all(record.target.join("dir")).unwrap();
    fs::write(record.target.join("dir/secret"), b"protected").unwrap();
    fs::write(record.target.join("dir/normal"), b"original").unwrap();
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![record.target.clone()],
        record.upper.upper_dir.clone(),
        Some(record.upper.work_dir.clone()),
        record.excluded_paths.clone(),
        Some(record.stage_dir.join("preimages")),
    )
    .unwrap();
    fs::write(core.copy_up(Path::new("dir/normal")).unwrap(), b"approved").unwrap();
    drop(core);
    apply_overlay(&mut record).unwrap();
    assert_eq!(
        fs::read(record.target.join("dir/normal")).unwrap(),
        b"approved"
    );
    assert_eq!(
        fs::read(record.target.join("dir/secret")).unwrap(),
        b"protected"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn case_folded_exclusions_guard_both_directory_operations_and_apply() {
    let temp = tempfile::tempdir().unwrap();
    let record = record(temp.path(), "DIR/SECRET");
    fs::create_dir_all(record.target.join("dir")).unwrap();
    fs::write(record.target.join("dir/secret"), b"protected").unwrap();
    let core = OverlayCore::new_with_exclusions(
        vec![record.target.clone()],
        record.upper.upper_dir.clone(),
        Some(record.upper.work_dir.clone()),
        record.excluded_paths.clone(),
    )
    .unwrap();
    assert!(core.remove(Path::new("dir"), true).is_err());
    drop(core);
    fs::write(record.upper.upper_dir.join(".wh.dir"), b"").unwrap();
    let error = plan_overlay_apply(
        &record,
        std::slice::from_ref(&record.target),
        &ApplySelection::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("excluded"), "{error}");
    assert_eq!(
        fs::read(record.target.join("dir/secret")).unwrap(),
        b"protected"
    );
}

#[test]
fn recovery_rejects_a_prepared_intent_that_crosses_an_excluded_tree() {
    use pvisor_overlay_core::apply::{
        ApplyRecord, ApplyRecordState, load_apply_records, recover_pending_applies,
        write_overlay_record,
    };
    let temp = tempfile::tempdir().unwrap();
    let mut record = record(temp.path(), "dir/secret");
    fs::create_dir_all(record.target.join("dir")).unwrap();
    fs::write(record.target.join("dir/secret"), b"protected").unwrap();
    fs::create_dir_all(&record.upper.upper_dir).unwrap();
    fs::write(record.upper.upper_dir.join(".wh.dir"), b"").unwrap();
    // An older writer could persist this intent without enforcing exclusions.
    let exclusions = std::mem::take(&mut record.excluded_paths);
    let plan = plan_overlay_apply(
        &record,
        std::slice::from_ref(&record.target),
        &ApplySelection::default(),
    )
    .unwrap();
    record.excluded_paths = exclusions;
    write_overlay_record(&record).unwrap();
    let pending = ApplyRecord {
        schema_version: 2,
        apply_id: "legacy-protected-intent".into(),
        created_at_unix_ms: 0,
        overlay_id: record.id.clone(),
        overlay_generation: record.generation,
        target: record.target.clone(),
        selection: ApplySelection::default(),
        changes: plan.selected,
        planned_paths: vec![PathBuf::from("dir")],
        preimages: vec![],
        state: ApplyRecordState::Prepared,
        remaining_changes: 0,
    };
    fs::write(
        record.stage_dir.join("apply-ledger.json"),
        serde_json::to_vec(&serde_json::json!({ "schema_version": 2, "records": [pending] }))
            .unwrap(),
    )
    .unwrap();
    let lower = record.target.clone();
    let error = recover_pending_applies(&mut record, &[lower]).unwrap_err();
    assert!(error.to_string().contains("excluded"), "{error}");
    assert_eq!(
        load_apply_records(&record.stage_dir).unwrap()[0].state,
        ApplyRecordState::Prepared
    );
    assert_eq!(
        fs::read(record.target.join("dir/secret")).unwrap(),
        b"protected"
    );
}

#[test]
fn lower_opaque_attributes_are_not_imported_but_upper_snapshots_keep_them() {
    for control in ["user.overlay.opaque", "user.fuseoverlayfs.opaque"] {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(lower.join("dir")).unwrap();
        fs::write(lower.join("visible"), b"root").unwrap();
        fs::write(lower.join("dir/visible"), b"nested").unwrap();
        sys::set_xattr(&lower, OsStr::new(control), b"y", 0).unwrap();
        sys::set_xattr(&lower.join("dir"), OsStr::new(control), b"y", 0).unwrap();
        sys::set_xattr(
            &lower.join("dir"),
            OsStr::new("user.pvisor.test"),
            b"metadata",
            0,
        )
        .unwrap();
        let core = OverlayCore::new(vec![lower], upper.clone(), None).unwrap();
        assert!(
            core.list_names(Path::new(""))
                .unwrap()
                .contains(&"visible".into())
        );
        core.copy_up(Path::new("dir")).unwrap();
        assert!(
            core.list_names(Path::new("dir"))
                .unwrap()
                .contains(&"visible".into())
        );
        assert_eq!(
            sys::get_xattr(&upper.join("dir"), OsStr::new("user.pvisor.test")).unwrap(),
            b"metadata"
        );
        sys::set_xattr(&upper.join("dir"), OsStr::new(control), b"y", 0).unwrap();
        let restored = temp.path().join("restored");
        restore_overlay_upper(&upper, &restored).unwrap();
        assert_eq!(
            sys::get_xattr(&restored.join("dir"), OsStr::new(control)).unwrap(),
            b"y"
        );
    }
}

#[test]
fn excluded_descendants_block_destructive_tree_operations_before_publication() {
    for operation in [
        "remove",
        "rename",
        "replace",
        "exchange",
        "materialize",
        "prepare",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let lower = tmp.path().join("lower");
        fs::create_dir_all(lower.join("dir/nested")).unwrap();
        fs::create_dir_all(lower.join("empty")).unwrap();
        fs::write(lower.join("dir/nested/secret"), b"protected").unwrap();
        fs::write(lower.join("dir/normal"), b"normal").unwrap();
        let upper = tmp.path().join("upper");
        let core = OverlayCore::new_with_exclusions(
            vec![lower.clone()],
            upper.clone(),
            None,
            vec!["dir/nested/secret".into()],
        )
        .unwrap();
        core.remove(Path::new("dir/normal"), false).unwrap();
        assert!(core.list_names(Path::new("dir/nested")).unwrap().is_empty());
        let dir = Path::new("dir");
        let empty = Path::new("empty");
        let result = match operation {
            "remove" => core.remove(Path::new("dir/nested"), true),
            "rename" => core.rename(dir, Path::new("moved"), false),
            "replace" => core.rename(empty, Path::new("dir/nested"), false),
            "exchange" => core.exchange(dir, empty),
            "materialize" => core.materialize_tree(dir).map(|_| ()),
            "prepare" => core
                .prepare_rename_copy_up(dir, empty, false, true)
                .map(|_| ()),
            _ => unreachable!(),
        };
        assert!(result.is_err(), "{operation}");
        assert_eq!(
            fs::read(lower.join("dir/nested/secret")).unwrap(),
            b"protected"
        );
        assert!(!upper.join("moved").exists());
        assert!(!upper.join("dir/.wh.nested").exists());
        assert!(!core.is_opaque(dir));
    }
}

#[test]
fn destination_exclusions_also_block_tree_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let lower = tmp.path().join("lower");
    fs::create_dir_all(lower.join("source")).unwrap();
    fs::write(lower.join("source/secret"), b"data").unwrap();
    let core = OverlayCore::new_with_exclusions(
        vec![lower],
        tmp.path().join("upper"),
        None,
        vec!["destination/secret".into()],
    )
    .unwrap();
    assert!(
        core.rename(Path::new("source"), Path::new("destination"), false)
            .is_err()
    );
    assert!(!core.is_opaque(Path::new("source")));
}

#[test]
fn sealed_legacy_stages_cannot_apply_effects_over_excluded_descendants() {
    for operation in ["delete", "replace", "opaque"] {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir_all(target.join("dir")).unwrap();
        fs::write(target.join("dir/secret"), b"protected").unwrap();
        let stage_dir = tmp.path().join("stage");
        let upper = stage_dir.join("upper");
        let journal = stage_dir.join("preimages");
        stage::begin(&journal, stage::StageDurability::Checkpoint).unwrap();
        // Produce a valid stage with an older/unrestricted writer, then restore
        // its owner's exclusions. Apply must enforce the actual effect closure.
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(stage_dir.join("work")),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        if operation == "opaque" {
            core.materialize_tree(Path::new("dir")).unwrap();
        } else {
            core.remove(Path::new("dir/secret"), false).unwrap();
            core.remove(Path::new("dir"), true).unwrap();
            if operation == "replace" {
                core.create_file(Path::new("dir"), 0o600, libc::O_WRONLY)
                    .unwrap();
            }
        }
        drop(core);
        stage::seal(&upper, &journal).unwrap();
        let mut record = OverlayRecord {
            id: "protected".into(),
            generation: 0,
            target: target.clone(),
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: upper,
                work_dir: stage_dir.join("work"),
            },
            merged_dir: stage_dir.join("merged"),
            stage_dir,
            excluded_paths: vec!["dir/secret".into()],
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        for protection in ["excluded", "denied", "selection"] {
            record.excluded_paths = if protection == "excluded" {
                vec!["dir/secret".into()]
            } else {
                vec![]
            };
            record.access_policy = FileAccessPolicy::new(
                if protection == "denied" {
                    vec!["dir/secret".into()]
                } else {
                    vec![]
                },
                vec![],
            )
            .unwrap();
            let selection = ApplySelection {
                excludes: if protection == "selection" {
                    vec!["dir/secret".into()]
                } else {
                    vec![]
                },
                ..Default::default()
            };
            assert!(
                plan_overlay_apply(&record, std::slice::from_ref(&target), &selection).is_err(),
                "{operation} {protection}"
            );
            assert!(
                apply_overlay_selected(&mut record, std::slice::from_ref(&target), &selection)
                    .is_err(),
                "{operation} {protection}"
            );
            assert_eq!(fs::read(target.join("dir/secret")).unwrap(), b"protected");
        }
    }
}

#[test]
fn rejecting_parent_removal_still_allows_applying_an_unprotected_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    fs::create_dir_all(target.join("dir")).unwrap();
    fs::write(target.join("dir/secret"), b"protected").unwrap();
    fs::write(target.join("dir/normal"), b"normal").unwrap();
    let stage_dir = tmp.path().join("stage");
    let journal = stage_dir.join("preimages");
    stage::begin(&journal, stage::StageDurability::Checkpoint).unwrap();
    let upper = stage_dir.join("upper");
    let exclusions = vec!["dir/secret".into()];
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![target.clone()],
        upper.clone(),
        Some(stage_dir.join("work")),
        exclusions.clone(),
        Some(journal.clone()),
    )
    .unwrap();
    core.remove(Path::new("dir/normal"), false).unwrap();
    assert!(core.remove(Path::new("dir"), true).is_err());
    drop(core);
    stage::seal(&upper, &journal).unwrap();
    let mut record = OverlayRecord {
        id: "protected".into(),
        generation: 0,
        target: target.clone(),
        baseline_lower: None,
        upper: OverlayUpper {
            upper_dir: upper,
            work_dir: stage_dir.join("work"),
        },
        merged_dir: stage_dir.join("merged"),
        stage_dir,
        excluded_paths: exclusions,
        access_policy: Default::default(),
        auto_apply: false,
        auto_discard: false,
        protect_target: false,
        state: OverlayState::Staged,
    };
    apply_overlay(&mut record).unwrap();
    assert_eq!(fs::read(target.join("dir/secret")).unwrap(), b"protected");
    assert!(!target.join("dir/normal").exists());
}

#[test]
fn lower_control_xattrs_do_not_hide_children_after_metadata_import() {
    for name in ["user.overlay.opaque", "user.fuseoverlayfs.opaque"] {
        let tmp = tempfile::tempdir().unwrap();
        let lower = tmp.path().join("lower");
        fs::create_dir_all(lower.join("dir")).unwrap();
        fs::write(lower.join("visible"), b"root").unwrap();
        fs::write(lower.join("dir/child"), b"child").unwrap();
        for path in [&lower, &lower.join("dir")] {
            sys::set_xattr(path, OsStr::new(name), b"y", 0).unwrap();
            sys::set_xattr(path, OsStr::new("user.pvisor-test"), b"metadata", 0).unwrap();
        }
        let upper = tmp.path().join("upper");
        let core = OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap();
        assert!(
            core.list_names(Path::new(""))
                .unwrap()
                .contains(&"visible".into())
        );
        core.copy_up(Path::new("dir")).unwrap();
        assert!(
            core.list_names(Path::new("dir"))
                .unwrap()
                .contains(&"child".into())
        );
        for path in [&upper, &upper.join("dir")] {
            assert!(!pvisor_overlay_core::is_opaque_directory(path));
            assert_eq!(
                sys::get_xattr(path, OsStr::new("user.pvisor-test")).unwrap(),
                b"metadata"
            );
        }
        // Existing upper control state must retain its semantics on reopen.
        drop(core);
        sys::set_xattr(&upper.join("dir"), OsStr::new(name), b"y", 0).unwrap();
        let restored = tmp.path().join("restored");
        restore_overlay_upper(&upper, &restored).unwrap();
        assert!(pvisor_overlay_core::is_opaque_directory(
            &restored.join("dir")
        ));
        let core = OverlayCore::open_existing_for_layout(
            pvisor_overlay_core::OverlayLayout::new(vec![lower.clone()], lower).unwrap(),
            upper,
            None,
            vec![],
            None,
        )
        .unwrap();
        assert!(core.is_opaque(Path::new("dir")));
        assert!(core.list_names(Path::new("dir")).unwrap().is_empty());
    }
}
