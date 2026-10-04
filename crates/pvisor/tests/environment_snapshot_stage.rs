#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
use pvisor::environment_snapshot::{BaseReference, Compatibility, SnapshotStore};
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, symlink},
};

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "boot".into(),
        build: "build".into(),
        firmware: "firmware".into(),
        profile: "test-stage-v1".into(),
    }
}
#[test]
fn stage_branches_preserve_metadata_and_pin_bases_after_object_deletion() {
    for compressed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("immutable"), vec![3; 1024 * 1024]).unwrap();
        let root = temp.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let base = store.import_base(&source).unwrap();
        base.verify().unwrap();
        let reference = base.reference().clone();
        fs::remove_dir_all(source).unwrap();
        let stage = temp.path().join("stage");
        for part in ["upper", "work", "preimages"] {
            fs::create_dir_all(stage.join(part)).unwrap();
        }
        fs::write(stage.join("upper/new"), b"private data").unwrap();
        fs::hard_link(stage.join("upper/new"), stage.join("upper/alias")).unwrap();
        symlink("new", stage.join("upper/link")).unwrap();
        // The store copies opaque overlay metadata without interpreting/removing it.
        fs::write(stage.join("work/metadata"), b"whiteouts and copy-up state").unwrap();
        fs::write(stage.join("preimages/before"), b"baseline").unwrap();
        let pending = store.begin().unwrap();
        pending
            .create_ram()
            .unwrap()
            .write_all(&[7; 65536])
            .unwrap();
        let id = pending
            .publish_stage(&stage, &[base], b"machine", compatibility(), compressed)
            .unwrap();
        let snapshot = store.open(&id, &compatibility()).unwrap();
        assert_eq!(snapshot.manifest().version, if compressed { 5 } else { 4 });
        assert_eq!(
            snapshot.manifest().stage_bases.as_ref().unwrap(),
            std::slice::from_ref(&reference)
        );
        assert!(
            !root
                .join("objects")
                .join(&id)
                .join("rootfs/immutable")
                .exists()
        );
        assert!(
            snapshot
                .materialize(&temp.path().join("wrong-profile"))
                .is_err()
        );
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        snapshot.materialize_stage(&first).unwrap();
        snapshot.materialize_stage(&second).unwrap();
        let lease = snapshot.base_leases().unwrap();
        let a = fs::metadata(first.join("upper/new")).unwrap();
        let alias = fs::metadata(first.join("upper/alias")).unwrap();
        let b = fs::metadata(second.join("upper/new")).unwrap();
        assert_eq!(a.ino(), alias.ino());
        assert_ne!(a.ino(), b.ino());
        fs::write(first.join("upper/alias"), b"branch a").unwrap();
        assert_eq!(fs::read(second.join("upper/new")).unwrap(), b"private data");
        assert_eq!(
            fs::read(second.join("work/metadata")).unwrap(),
            b"whiteouts and copy-up state"
        );
        assert_eq!(
            fs::read(second.join("preimages/before")).unwrap(),
            b"baseline"
        );
        assert_eq!(
            fs::read_link(second.join("upper/link")).unwrap(),
            std::path::Path::new("new")
        );
        drop(snapshot);
        store.collect_abandoned().unwrap();
        assert!(store.open_base(&reference).is_ok());
        store.delete(&id).unwrap();
        store.collect_abandoned().unwrap();
        assert!(lease[0].root().join("immutable").exists());
        drop(lease);
        store.collect_abandoned().unwrap();
        assert!(store.open_base(&reference).is_err());
    }
}
#[test]
fn missing_replaced_or_unpinned_base_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"base").unwrap();
    let root = temp.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let base = store.import_base(&source).unwrap();
    let reference = base.reference().clone();
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    let stage = temp.path().join("stage");
    fs::create_dir(&stage).unwrap();
    let id = pending
        .publish_stage(&stage, &[base], b"machine", compatibility(), false)
        .unwrap();
    let directory = root.join("bases").join(&reference.id);
    let missing = root.join("moved-base");
    fs::rename(&directory, &missing).unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    fs::rename(missing, &directory).unwrap();
    let pin = root
        .join("objects")
        .join(&id)
        .join("base-refs")
        .join(&reference.id);
    fs::remove_file(&pin).unwrap();
    fs::write(&pin, b"fake reference").unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    assert!(
        store
            .open_base(&BaseReference {
                id: "../outside".into()
            })
            .is_err()
    );
    let original = directory.join("rootfs");
    fs::rename(&original, directory.join("old-rootfs")).unwrap();
    fs::create_dir(original).unwrap();
    assert!(store.open_base(&reference).is_err());
}
#[test]
fn full_audit_detects_nested_content_changes_and_foreign_store_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/file"), b"base").unwrap();
    let store = SnapshotStore::new(&temp.path().join("store")).unwrap();
    let base = store.import_base(&source).unwrap();
    fs::write(base.root().join("nested/file"), b"corrupt").unwrap();
    assert!(base.verify().is_err());
    let other = SnapshotStore::new(&temp.path().join("other")).unwrap();
    let pending = other.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    let stage = temp.path().join("stage");
    fs::create_dir(stage.clone()).unwrap();
    assert!(
        pending
            .publish_stage(&stage, &[base], b"machine", compatibility(), false)
            .is_err()
    );
    assert_eq!(
        fs::read_dir(temp.path().join("other/objects"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn eager_ram_checks_full_raw_digest_after_lazy_open() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let store_root = temp.path().join("store");
    let store = SnapshotStore::new(&store_root).unwrap();
    let base = store.import_base(&source).unwrap();
    let stage = temp.path().join("stage");
    fs::create_dir(&stage).unwrap();
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    let id = pending
        .publish_stage(&stage, &[base], b"machine", compatibility(), false)
        .unwrap();
    let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
    assert!(snapshot.ram_file().is_ok());
    fs::write(store_root.join("objects").join(id).join("ram.bin"), b"BAD").unwrap();
    assert!(snapshot.ram_file().is_err());
}
