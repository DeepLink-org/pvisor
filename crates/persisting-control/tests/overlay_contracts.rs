use persisting_control::overlay::{
    ApplyRecord, ApplyRecordState, OverlayRecord, OverlayState, PathPreimage, RunControlRequest,
    RunControlResponse,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn assert_wire_roundtrip<T: Serialize + DeserializeOwned>(wire: Value) {
    let decoded: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
}

#[test]
fn inspection_protocol_keeps_existing_json() {
    for request in [
        json!({"op": "ping"}),
        json!({"op": "overlay_status"}),
        json!({"op": "mount_inspect"}),
        json!({"op": "unmount_inspect", "id": "inspect-1"}),
    ] {
        assert_wire_roundtrip::<RunControlRequest>(request);
    }
    for response in [
        json!({"ok": true, "id": null, "mountpoint": null, "error": null,
            "overlay_status": {"changed_files": 2, "whiteouts": 1, "sample_paths": ["a"]}}),
        json!({"ok": true, "id": "inspect-1", "mountpoint": "/stage/inspect/merged",
            "error": null, "overlay_status": null}),
        json!({"ok": false, "id": null, "mountpoint": null,
            "error": "unknown inspect session", "overlay_status": null}),
    ] {
        assert_wire_roundtrip::<RunControlResponse>(response);
    }
}

#[test]
fn legacy_overlay_and_apply_records_keep_defaults() {
    let overlay: OverlayRecord = serde_json::from_value(json!({
        "id": "overlay-1", "target": "/target",
        "upper": {"kind": "directory", "upper_dir": "/stage/upper", "work_dir": "/stage/work"},
        "merged_dir": "/stage/merged", "stage_dir": "/stage",
        "auto_apply": false, "state": "staged"
    }))
    .unwrap();
    assert_eq!(overlay.generation, 0);
    assert_eq!(overlay.state, OverlayState::Staged);
    assert!(overlay.excluded_paths.is_empty());
    assert!(!overlay.auto_discard && !overlay.protect_target);

    let mut wire = json!({
        "schema_version": 1, "apply_id": "apply-1", "created_at_unix_ms": 123,
        "overlay_id": "overlay-1", "target": "/target", "selection": {},
        "changes": [{"path": "a", "kind": "added", "new_type": "file"}],
        "remaining_changes": 0
    });
    let record: ApplyRecord = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(record.state, ApplyRecordState::Committed);
    assert_eq!(record.overlay_generation, 0);
    assert!(record.selection.is_all());
    assert!(record.preimages.is_empty() && record.planned_paths.is_empty());
    wire["state"] = json!("committed");
    wire["overlay_generation"] = json!(0);
    assert_eq!(serde_json::to_value(record).unwrap(), wire);
}

#[test]
fn preimages_preserve_all_fingerprint_shapes_and_raw_path_bytes() {
    for state in [
        json!({"kind": "absent"}),
        json!({"kind": "file", "sha256": "abc", "mode": 420, "uid": 1, "gid": 2}),
        json!({"kind": "directory", "mode": 493, "uid": 1, "gid": 2,
            "mtime_seconds": 123, "mtime_nanoseconds": 456}),
        json!({"kind": "symlink", "target": [255, 97], "uid": 1, "gid": 2}),
        json!({"kind": "other", "mode": 4480, "uid": 1, "gid": 2, "rdev": 7}),
    ] {
        let wire = json!({"path": [100, 47, 255], "state": state});
        assert_wire_roundtrip::<PathPreimage>(wire.clone());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let preimage: PathPreimage = serde_json::from_value(wire).unwrap();
            assert_eq!(
                preimage.relative_path().as_os_str().as_bytes(),
                &[100, 47, 255]
            );
        }
    }
}
