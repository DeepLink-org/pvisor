#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
use pvisor::environment_snapshot::{Compatibility, SnapshotStore};
use std::{fs, io::Write};

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "test-boot".into(),
        build: "test-build".into(),
        firmware: "test-firmware".into(),
        profile: "test-no-external-resources".into(),
    }
}

fn layered_snapshot(root: &std::path::Path) -> (SnapshotStore, String) {
    let source = root.join("layered-source");
    for name in ["lower", "upper", "work", "preimages"] {
        fs::create_dir_all(source.join(name)).unwrap();
    }
    fs::write(source.join("lower/data"), b"immutable baseline").unwrap();
    fs::hard_link(source.join("lower/data"), source.join("lower/alias")).unwrap();
    fs::write(source.join("lower/unvisited"), b"complete inventory").unwrap();
    fs::write(source.join("upper/private"), b"initial upper").unwrap();
    pvisor::environment_snapshot::copy_owned_tree(
        &source.join("lower"),
        &source.join("same-lower"),
    )
    .unwrap();
    let store = SnapshotStore::new(&root.join("layered-store")).unwrap();
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    let id = pending
        .publish(&source, b"machine", compatibility())
        .unwrap();
    fs::remove_dir_all(source).unwrap();
    (store, id)
}

#[test]
fn layer_sharing_checks_reference_volume_and_private_copy_remains_available() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let (store, id) = layered_snapshot(root.path());
    let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
    // A workspace on a disk volume and /tmp on tmpfs exercise EXDEV locally.
    // On single-volume hosts this still verifies the capability decision.
    let references = tempfile::Builder::new()
        .prefix(".pvisor-filesystem-volume-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let same_volume =
        fs::metadata(root.path()).unwrap().dev() == fs::metadata(references.path()).unwrap().dev();
    assert_eq!(
        snapshot.can_share_layers(references.path()).unwrap(),
        same_volume
    );
    if !same_volume {
        assert!(
            snapshot
                .share_readonly_layer(std::path::Path::new("lower"), references.path())
                .is_err()
        );
        assert_eq!(fs::read_dir(references.path()).unwrap().count(), 0);
    }
    let private = references.path().join("private");
    snapshot
        .materialize_layer(std::path::Path::new("lower"), &private)
        .unwrap();
    assert_eq!(
        fs::read(private.join("data")).unwrap(),
        b"immutable baseline"
    );
    assert_eq!(fs::metadata(private.join("data")).unwrap().nlink(), 2);
    assert_eq!(
        fs::metadata(private.join("data")).unwrap().ino(),
        fs::metadata(private.join("alias")).unwrap().ino()
    );
}

#[test]
fn shared_lower_references_survive_snapshot_deletion_restart_and_retained_attempts() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let (store, id) = layered_snapshot(root.path());
    let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
    let references = [
        root.path().join("first-references"),
        root.path().join("second-references"),
    ];
    for path in &references {
        fs::create_dir(path).unwrap();
    }
    let first = snapshot
        .share_readonly_layer(std::path::Path::new("lower"), &references[0])
        .unwrap();
    let second = snapshot
        .share_readonly_layer(std::path::Path::new("lower"), &references[1])
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.root(), second.root());
    let data = first.root().join("data");
    let meta = fs::metadata(&data).unwrap();
    assert_eq!(
        meta.ino(),
        fs::metadata(second.root().join("data")).unwrap().ino()
    );
    assert_eq!(
        meta.nlink(),
        2,
        "only marker links may reference a shared tree"
    );
    assert_eq!(
        meta.ino(),
        fs::metadata(first.root().join("alias")).unwrap().ino()
    );
    let private = [
        root.path().join("first-upper"),
        root.path().join("second-upper"),
    ];
    for path in &private {
        snapshot
            .materialize_layer(std::path::Path::new("upper"), path)
            .unwrap();
    }
    assert_ne!(
        fs::metadata(private[0].join("private")).unwrap().ino(),
        fs::metadata(private[1].join("private")).unwrap().ino()
    );
    fs::write(private[0].join("private"), b"first edit").unwrap();
    assert_eq!(
        fs::read(private[1].join("private")).unwrap(),
        b"initial upper"
    );
    let tree = first.root().to_owned();
    let identity = first.id.clone();
    drop(snapshot);
    store.delete(&id).unwrap();
    drop((first, second));
    let restarted = SnapshotStore::new(&root.path().join("layered-store")).unwrap();
    assert_eq!(restarted.collect_abandoned().unwrap(), 0);
    assert_eq!(fs::read(&data).unwrap(), b"immutable baseline");
    assert!(
        restarted.open_for_restore(&id, &compatibility()).is_err(),
        "cached trees must not authorize a deleted snapshot"
    );
    fs::remove_file(references[0].join(&identity)).unwrap();
    assert_eq!(restarted.collect_abandoned().unwrap(), 0);
    fs::remove_file(references[1].join(&identity)).unwrap();
    assert_eq!(restarted.collect_abandoned().unwrap(), 1);
    assert!(!tree.exists());
    assert_eq!(fs::read(private[0].join("private")).unwrap(), b"first edit");
}

#[test]
fn active_layer_owner_fences_gc_even_after_reference_removal() {
    let root = tempfile::tempdir().unwrap();
    let (store, id) = layered_snapshot(root.path());
    let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
    let references = root.path().join("references");
    fs::create_dir(&references).unwrap();
    let layer = snapshot
        .share_readonly_layer(std::path::Path::new("lower"), &references)
        .unwrap();
    fs::remove_file(references.join(&layer.id)).unwrap();
    drop(snapshot);
    store.delete(&id).unwrap();
    assert_eq!(store.collect_abandoned().unwrap(), 0);
    assert!(layer.root().exists());
    let path = layer.root().to_owned();
    drop(layer);
    assert_eq!(store.collect_abandoned().unwrap(), 1);
    assert!(!path.exists());
}

#[test]
fn concurrent_layer_publication_deduplicates_without_collapsing_distinct_roots() {
    use std::{os::unix::fs::MetadataExt, path::Path};
    let root = tempfile::tempdir().unwrap();
    let (_store, id) = layered_snapshot(root.path());
    let store = SnapshotStore::new(&root.path().join("layered-store")).unwrap();
    let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
    let references = [root.path().join("left"), root.path().join("right")];
    for path in &references {
        fs::create_dir(path).unwrap();
    }
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            snapshot
                .share_readonly_layer(Path::new("lower"), &references[0])
                .unwrap()
        });
        let two = scope.spawn(|| {
            snapshot
                .share_readonly_layer(Path::new("lower"), &references[1])
                .unwrap()
        });
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(one.id, two.id);
    let other = snapshot
        .share_readonly_layer(Path::new("same-lower"), &references[0])
        .unwrap();
    assert_ne!(
        one.id, other.id,
        "equal contents at separate overlay roots must preserve inode identity"
    );
    assert_ne!(
        fs::metadata(one.root().join("data")).unwrap().ino(),
        fs::metadata(other.root().join("data")).unwrap().ino()
    );
    assert_eq!(
        pvisor::environment_snapshot::inventory(one.root()).unwrap(),
        pvisor::environment_snapshot::inventory(other.root()).unwrap()
    );
    assert_eq!(
        fs::read_dir(root.path().join("layered-store/filesystems"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn sharing_rejects_corruption_unvisited_changes_symlinks_and_conflicting_references() {
    use std::{os::unix::fs::symlink, path::Path};
    for fault in [
        "unvisited",
        "manifest",
        "marker",
        "reference",
        "symlink",
        "source",
        "escape",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (store, id) = layered_snapshot(root.path());
        let snapshot = store.open_for_restore(&id, &compatibility()).unwrap();
        let references = root.path().join("references");
        fs::create_dir(&references).unwrap();
        let layer = snapshot
            .share_readonly_layer(Path::new("lower"), &references)
            .unwrap();
        let object = layer.root().parent().unwrap();
        let next = root.path().join("next");
        fs::create_dir(&next).unwrap();
        let relative = if fault == "escape" {
            Path::new("../lower")
        } else {
            Path::new("lower")
        };
        match fault {
            "unvisited" => fs::write(layer.root().join("unvisited"), b"bad").unwrap(),
            "manifest" => fs::write(object.join("manifest.json"), b"bad").unwrap(),
            "marker" => fs::write(object.join("reference"), "0".repeat(64)).unwrap(),
            "reference" => fs::write(next.join(&layer.id), layer.id.as_bytes()).unwrap(),
            "symlink" => {
                fs::remove_dir(&next).unwrap();
                symlink(&references, &next).unwrap();
            }
            "source" => fs::write(
                root.path()
                    .join("layered-store/objects")
                    .join(&id)
                    .join("rootfs/lower/unvisited"),
                b"bad",
            )
            .unwrap(),
            _ => {}
        }
        assert!(
            snapshot.share_readonly_layer(relative, &next).is_err(),
            "{fault}"
        );
        if fault != "reference" && fault != "symlink" {
            assert_eq!(fs::read_dir(&next).unwrap().count(), 0, "{fault}");
        }
    }
}

#[test]
fn sparse_ram_copy_and_sealing_preserve_bytes_length_and_detach_source_writers() {
    use std::os::unix::fs::{FileExt, MetadataExt};
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        let capture = tempfile::tempfile().unwrap();
        let size = 8 * 1024 * 1024 + 17;
        capture.set_len(size).unwrap();
        capture.write_all_at(b"first", 7).unwrap();
        capture.write_all_at(b"tail", size - 4).unwrap();
        let store_root = directory.path().join("store");
        let store = SnapshotStore::new(&store_root).unwrap();
        let pending = store.begin().unwrap();
        pending.copy_ram_from(&capture).unwrap();
        assert!(
            pending.copy_ram_from(&capture).is_err(),
            "pending RAM must have one owner"
        );
        let staging = fs::read_dir(store_root.join("pending"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("capture.ram");
        let metadata = fs::metadata(staging).unwrap();
        assert_eq!(metadata.len(), size);
        assert!(metadata.blocks() * 512 < size / 2);
        let id = if compressed {
            pending.publish_compressed(&source, b"machine", compatibility())
        } else {
            pending.publish(&source, b"machine", compatibility())
        }
        .unwrap();
        capture.write_all_at(b"wrong", 7).unwrap();
        let snapshot = store.open(&id, &compatibility()).unwrap();
        let mut reader = snapshot.ram_reader().unwrap();
        let mut bytes = vec![0; size as usize];
        assert_eq!(reader.read_at(0, &mut bytes).unwrap(), bytes.len());
        let mut expected = vec![0; size as usize];
        expected[7..12].copy_from_slice(b"first");
        expected[size as usize - 4..].copy_from_slice(b"tail");
        assert_eq!(bytes, expected);
        if !compressed {
            assert!(snapshot.ram_file().unwrap().metadata().unwrap().blocks() * 512 < size / 2);
        }
    }
}

#[test]
fn published_object_survives_source_removal_and_has_private_worktrees() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    let store = SnapshotStore::new(&directory.path().join("store")).unwrap();
    let pending = store.begin().unwrap();
    let mut writable_capture = pending.create_ram().unwrap();
    writable_capture.write_all(b"RAM").unwrap();
    let id = pending
        .publish(&source, b"machine-state", compatibility())
        .unwrap();
    // A stale capture writer must no longer name the sealed RAM payload.
    writable_capture
        .write_all(b"stale capture modification")
        .unwrap();
    fs::remove_dir_all(source).unwrap();
    let snapshot = store.open(&id, &compatibility()).unwrap();
    assert_eq!(snapshot.machine_bytes().unwrap(), b"machine-state");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    snapshot.materialize(&first).unwrap();
    fs::write(first.join("file"), b"first-private").unwrap();
    snapshot.materialize(&second).unwrap();
    assert_eq!(fs::read(second.join("file")).unwrap(), b"original");
    assert!(store.delete(&id).is_err());
    drop(snapshot);
    store.delete(&id).unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    assert_eq!(fs::read(first.join("file")).unwrap(), b"first-private");
    assert_eq!(fs::read(second.join("file")).unwrap(), b"original");
}

#[test]
fn unpublished_failures_are_cleaned_and_never_openable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    assert!(store.open(&"0".repeat(64), &compatibility()).is_err());
    drop(pending);
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
    let pending = store.begin().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"source preserved").unwrap();
    assert!(
        pending
            .publish(&source, b"machine", compatibility())
            .is_err()
    );
    assert_eq!(fs::read(source.join("data")).unwrap(), b"source preserved");
    assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
}

#[test]
fn payload_and_manifest_corruption_and_incompatible_restore_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(b"RAM").unwrap();
    let id = pending
        .publish(&source, b"machine-state", compatibility())
        .unwrap();
    let mut incompatible = compatibility();
    incompatible.host_boot = "other".into();
    assert!(store.open(&id, &incompatible).is_err());
    let object = root.join("objects").join(&id);
    for name in ["ram.bin", "machine.json", "manifest.json", "rootfs/file"] {
        let path = object.join(name);
        let saved = fs::read(&path).unwrap();
        fs::write(&path, b"corruption").unwrap();
        assert!(store.open(&id, &compatibility()).is_err(), "{name}");
        fs::write(path, saved).unwrap();
        // File timestamps are part of the filesystem seal; restoring bytes
        // alone does not make a modified backing tree valid again.
        if name == "rootfs/file" {
            break;
        }
    }
}

#[test]
fn cleanup_abandoned_objects_preserves_active_writer() {
    const ROLE: &str = "PVISOR_ENVIRONMENT_ABANDONED_WRITER";
    if let Some(root) = std::env::var_os(ROLE) {
        let store = SnapshotStore::new(std::path::Path::new(&root)).unwrap();
        let pending = store.begin().unwrap();
        pending
            .create_ram()
            .unwrap()
            .write_all(b"unfinished RAM")
            .unwrap();
        // Process exit bypasses TempDir Drop but releases its OS file lock.
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    assert!(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cleanup_abandoned_objects_preserves_active_writer",
                "--nocapture"
            ])
            .env(ROLE, &root)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 1);
    let active = store.begin().unwrap();
    active
        .create_ram()
        .unwrap()
        .write_all(b"active RAM")
        .unwrap();
    fs::create_dir(root.join("deleted/tombstone")).unwrap();
    fs::write(root.join("deleted/tombstone/file"), b"deleted payload").unwrap();
    assert_eq!(store.collect_abandoned().unwrap(), 2);
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(root.join("deleted")).unwrap().count(), 0);
    drop(active);
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
}

#[test]
fn compressed_ram_is_durable_shared_and_collected_only_after_last_snapshot() {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    // Repetition within and across snapshots, and a short tail, use one codec.
    let bytes = vec![17; 2 * 65536 + 13];
    let mut identities = Vec::new();
    for machine in [b"first-machine".as_slice(), b"second-machine"] {
        let pending = store.begin().unwrap();
        let mut stale = pending.create_ram().unwrap();
        stale.write_all(&bytes).unwrap();
        identities.push(
            pending
                .publish_compressed(&source, machine, compatibility())
                .unwrap(),
        );
        stale.write_all(b"stale modification").unwrap();
    }
    fs::remove_dir_all(source).unwrap();
    drop(store);
    let store = SnapshotStore::new(&root).unwrap();
    let first = store.open(&identities[0], &compatibility()).unwrap();
    let blocks = first.manifest().ram_blocks.as_ref().unwrap();
    assert_eq!(blocks.blocks.len(), 3);
    assert_eq!(blocks.blocks[0].id, blocks.blocks[1].id);
    assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 2);
    let blob = root.join("content").join(&blocks.blocks[0].id);
    let meta = fs::metadata(&blob).unwrap();
    assert!(meta.len() < 65536);
    assert_eq!(meta.nlink(), 3); // cache + one reference from each snapshot
    let mut reader = first.ram_file().unwrap();
    assert!(reader.write_all(b"write forbidden").is_err());
    let mut restored = Vec::new();
    reader.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, bytes);
    assert!(store.collect_abandoned().is_err());
    drop(first);
    store.delete(&identities[0]).unwrap();
    assert_eq!(store.collect_abandoned().unwrap(), 0);
    let second = store.open(&identities[1], &compatibility()).unwrap();
    assert_eq!(fs::metadata(&blob).unwrap().nlink(), 2);
    // Same read-only artifact is independently materialized for each consumer.
    let mut restored = Vec::new();
    second
        .ram_file()
        .unwrap()
        .read_to_end(&mut restored)
        .unwrap();
    assert_eq!(restored, bytes);
    drop(second);
    store.delete(&identities[1]).unwrap();
    assert_eq!(store.collect_abandoned().unwrap(), 2);
    assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 0);
}

#[test]
fn compressed_ram_corruption_truncation_and_missing_references_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(&vec![23; 65536])
        .unwrap();
    let id = pending
        .publish_compressed(&source, b"machine", compatibility())
        .unwrap();
    let snapshot = store.open(&id, &compatibility()).unwrap();
    let block = snapshot.manifest().ram_blocks.as_ref().unwrap().blocks[0]
        .id
        .clone();
    drop(snapshot);
    let reference = root
        .join("objects")
        .join(&id)
        .join("ram-blocks")
        .join(block);
    let original = fs::read(&reference).unwrap();
    for corrupted in [
        original[..12].to_vec(),
        {
            let mut bytes = original.clone();
            *bytes.last_mut().unwrap() ^= 1;
            bytes
        },
        {
            let mut bytes = original.clone();
            bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
            bytes
        },
    ] {
        fs::write(&reference, corrupted).unwrap();
        assert!(store.open(&id, &compatibility()).is_err());
    }
    fs::write(&reference, original).unwrap();
    assert!(store.open(&id, &compatibility()).is_ok());
    fs::remove_file(reference).unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
}

#[test]
fn durable_codec_roundtrips_fill_zstd_raw_and_short_tail() {
    use std::io::Read;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let mut bytes = vec![0; 65536];
    bytes.extend((0..65536).map(|n| (n % 251) as u8));
    let mut seed = 123456789u64;
    bytes.extend((0..65536).map(|_| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as u8
    }));
    bytes.extend_from_slice(b"short tail");
    let pending = store.begin().unwrap();
    pending.create_ram().unwrap().write_all(&bytes).unwrap();
    let id = pending
        .publish_compressed(&source, b"machine", compatibility())
        .unwrap();
    let snapshot = store.open(&id, &compatibility()).unwrap();
    let blocks = &snapshot.manifest().ram_blocks.as_ref().unwrap().blocks;
    assert_eq!(blocks.len(), 4);
    let encodings: Vec<_> = blocks
        .iter()
        .map(|block| fs::read(root.join("content").join(&block.id)).unwrap()[44])
        .collect();
    assert_eq!(encodings, [0, 2, 1, 1]);
    let mut restored = Vec::new();
    snapshot
        .ram_file()
        .unwrap()
        .read_to_end(&mut restored)
        .unwrap();
    assert_eq!(restored, bytes);
}

#[test]
fn failed_compressed_publication_keeps_live_objects_and_reaps_orphan_content() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let root = directory.path().join("store");
    let store = SnapshotStore::new(&root).unwrap();
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(b"raw baseline")
        .unwrap();
    let baseline = pending
        .publish(&source, b"machine", compatibility())
        .unwrap();
    let reader = store.open(&baseline, &compatibility()).unwrap();
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(&vec![29; 65536])
        .unwrap();
    // Conservative store-wide reader gate rejects final publication, after blocks exist.
    assert!(
        pending
            .publish_compressed(&source, b"second", compatibility())
            .is_err()
    );
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 1);
    drop(reader);
    assert_eq!(store.collect_abandoned().unwrap(), 1);
    assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 0);
    assert!(store.open(&baseline, &compatibility()).is_ok());
}
