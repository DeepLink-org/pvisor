#![cfg(target_os = "linux")]
use pvisor::environment_snapshot::{
    Compatibility, SnapshotRepository, SnapshotStore, SnapshotTransfer, file_hash, inventory,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, PermissionsExt, symlink},
    },
    path::Path,
    sync::{Arc, atomic::Ordering},
};
#[path = "common/s3.rs"]
mod s3;

const PREFIX: &str = "/cache-bucket/team/pvisor-checkpoints-v1";
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "owned-test-boot".into(),
        build: "test-worker".into(),
        firmware: "test-kernel".into(),
        profile: "transfer-test".into(),
    }
}
fn repository(remote: &s3::MockS3, read_only: bool) -> SnapshotRepository {
    let client = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name("cache-bucket")
        .with_region("us-east-1")
        .with_access_key_id("AKIATEST")
        .with_secret_access_key("test-secret")
        .with_token("test-token")
        .with_endpoint(&remote.endpoint)
        .with_allow_http(true)
        .build()
        .unwrap();
    SnapshotRepository::object_store(Arc::new(client), "team", read_only).unwrap()
}
fn fixture(root: &Path, compressed: bool) -> (SnapshotStore, String) {
    let tree = root.join("source");
    fs::create_dir(&tree).unwrap();
    fs::create_dir(tree.join("unvisited")).unwrap();
    fs::write(tree.join("unvisited/private"), b"unvisited owned data").unwrap();
    fs::write(tree.join("data"), b"owned data").unwrap();
    fs::hard_link(tree.join("data"), tree.join("alias")).unwrap();
    symlink("../../outside", tree.join("link")).unwrap();
    fs::hard_link(tree.join("link"), tree.join("link-alias")).unwrap();
    fs::write(
        tree.join(OsStr::from_bytes(b"non-utf8-\xff")),
        b"native filename",
    )
    .unwrap();
    fs::set_permissions(tree.join("data"), fs::Permissions::from_mode(0o640)).unwrap();
    let path = CString::new(tree.join("data").as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::lsetxattr(
                path.as_ptr(),
                c"user.pvisor-transfer".as_ptr(),
                b"metadata".as_ptr().cast(),
                8,
                0,
            )
        },
        0
    );
    // A named POSIX ACL entry exercises metadata beyond chmod.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, unsafe { libc::getuid() }),
        (4, 4, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    assert_eq!(
        unsafe {
            libc::lsetxattr(
                path.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        },
        0
    );
    let sparse = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(tree.join("sparse"))
        .unwrap();
    sparse.set_len(3 * 1024 * 1024 + 17).unwrap();
    sparse.write_all_at(b"first", 0).unwrap();
    sparse.write_all_at(b"tail", 3 * 1024 * 1024 + 13).unwrap();
    let store = SnapshotStore::new(&root.join("source-store")).unwrap();
    let pending = store.begin().unwrap();
    let mut ram = pending.create_ram().unwrap();
    ram.set_len(4 * 1024 * 1024 + 23).unwrap();
    ram.seek(SeekFrom::Start(1_048_600)).unwrap();
    ram.write_all(b"live guest memory").unwrap();
    let id = if compressed {
        pending
            .publish_compressed(&tree, b"opaque machine state", compatibility())
            .unwrap()
    } else {
        pending
            .publish(&tree, b"opaque machine state", compatibility())
            .unwrap()
    };
    (store, id)
}
fn no_publication(root: &Path) {
    assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(root.join("pending")).unwrap().count(), 0);
}

#[test]
fn signed_s3_roundtrip_survives_source_removal_preserves_full_inventory_and_sparse_data() {
    let remote = s3::MockS3::start();
    let writer = repository(&remote, false);
    let reader = repository(&remote, true);
    for compressed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("outside"), b"outside sentinel").unwrap();
        let (source, id) = fixture(root, compressed);
        let published = source.open(&id, &compatibility()).unwrap();
        let expected = inventory(&root.join("source")).unwrap();
        let manifest = fs::read(
            root.join("source-store/objects")
                .join(&id)
                .join("manifest.json"),
        )
        .unwrap();
        let ram_hash = published.manifest().ram_sha256.clone();
        let receipt = writer.publish(&published).unwrap();
        assert_eq!(receipt.snapshot_id, id);
        assert_eq!(writer.publish(&published).unwrap(), receipt);
        assert!(reader.publish(&published).is_err());
        assert!(
            source.delete(&id).is_err(),
            "live export reference must fence deletion"
        );
        drop(published);
        source.delete(&id).unwrap();
        fs::remove_dir_all(root.join("source-store")).unwrap();
        fs::remove_dir_all(root.join("source")).unwrap();
        remote.read_only.store(true, Ordering::SeqCst);
        let before_puts = remote.puts.load(Ordering::SeqCst);
        let target_root = root.join("independent-worker-store");
        let target = SnapshotStore::new(&target_root).unwrap();
        reader.import(&target, &receipt, &compatibility()).unwrap();
        reader.import(&target, &receipt, &compatibility()).unwrap();
        assert_eq!(remote.puts.load(Ordering::SeqCst), before_puts);
        assert_eq!(
            fs::read(target_root.join("objects").join(&id).join("manifest.json")).unwrap(),
            manifest
        );
        let restored = target.open(&id, &compatibility()).unwrap();
        assert_eq!(restored.machine_bytes().unwrap(), b"opaque machine state");
        assert_eq!(restored.manifest().ram_sha256, ram_hash);
        let mut ram = restored.ram_reader().unwrap();
        let mut bytes = [0; 17];
        assert_eq!(ram.read_at(1_048_600, &mut bytes).unwrap(), 17);
        assert_eq!(&bytes, b"live guest memory");
        let materialized = root.join("restored-private-tree");
        restored.materialize(&materialized).unwrap();
        assert_eq!(inventory(&materialized).unwrap(), expected);
        assert_eq!(
            fs::metadata(materialized.join("data")).unwrap().ino(),
            fs::metadata(materialized.join("alias")).unwrap().ino()
        );
        assert_eq!(
            fs::symlink_metadata(materialized.join("link"))
                .unwrap()
                .ino(),
            fs::symlink_metadata(materialized.join("link-alias"))
                .unwrap()
                .ino()
        );
        assert!(
            fs::metadata(target_root.join("objects").join(&id).join("rootfs/sparse"))
                .unwrap()
                .blocks()
                * 512
                < 3 * 1024 * 1024
        );
        assert_eq!(
            file_hash(&materialized.join("sparse")).unwrap(),
            file_hash(&target_root.join("objects").join(&id).join("rootfs/sparse")).unwrap()
        );
        if compressed {
            for entry in fs::read_dir(target_root.join("content")).unwrap() {
                assert!(entry.unwrap().metadata().unwrap().nlink() >= 2);
            }
        }
        drop(ram);
        drop(restored);
        target.delete(&id).unwrap();
        target.collect_abandoned().unwrap();
        assert_eq!(
            fs::read_dir(target_root.join("content")).unwrap().count(),
            0
        );
        assert_eq!(fs::read(root.join("outside")).unwrap(), b"outside sentinel");
        remote.read_only.store(false, Ordering::SeqCst);
    }
    assert!(remote.gets.load(Ordering::SeqCst) > 0);
    assert!(!remote.deny_reads.load(Ordering::SeqCst));
    assert!(!remote.lost_head_ack.load(Ordering::SeqCst));
}

#[test]
fn missing_corrupt_or_truncated_chunks_never_publish_an_import() {
    for fault in ["missing", "corrupt", "truncated", "extended"] {
        let remote = s3::MockS3::start();
        let temp = tempfile::tempdir().unwrap();
        let (source, id) = fixture(temp.path(), false);
        let receipt = repository(&remote, false)
            .publish(&source.open(&id, &compatibility()).unwrap())
            .unwrap();
        let transfer: serde_json::Value = serde_json::from_slice(
            &remote.objects.lock().unwrap()[&format!("{PREFIX}/transfers/{}", receipt.transfer_id)],
        )
        .unwrap();
        let chunk_id = transfer["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|file| file["chunks"].as_array().unwrap())
            .find_map(|chunk| chunk.as_str())
            .unwrap();
        let key = format!("{PREFIX}/chunks/{chunk_id}");
        let mut objects = remote.objects.lock().unwrap();
        match fault {
            "missing" => {
                objects.remove(&key);
            }
            "corrupt" => {
                objects.get_mut(&key).unwrap()[0] ^= 1;
            }
            "truncated" => {
                objects.get_mut(&key).unwrap().pop();
            }
            "extended" => {
                objects.get_mut(&key).unwrap().push(0);
            }
            _ => unreachable!(),
        }
        drop(objects);
        let target_root = temp.path().join("target");
        let target = SnapshotStore::new(&target_root).unwrap();
        assert!(
            repository(&remote, true)
                .import(&target, &receipt, &compatibility())
                .is_err(),
            "{fault}"
        );
        no_publication(&target_root);
    }
}

#[test]
fn incompatible_hosts_fail_before_payload_reads_and_unknown_receipts_fail_closed() {
    let remote = s3::MockS3::start();
    let temp = tempfile::tempdir().unwrap();
    let (source, id) = fixture(temp.path(), true);
    let receipt = repository(&remote, false)
        .publish(&source.open(&id, &compatibility()).unwrap())
        .unwrap();
    let target_root = temp.path().join("target");
    let target = SnapshotStore::new(&target_root).unwrap();
    for field in ["boot", "build", "firmware", "profile"] {
        let mut expected = compatibility();
        match field {
            "boot" => expected.host_boot = "other boot".into(),
            "build" => expected.build = "other build".into(),
            "firmware" => expected.firmware = "other kernel".into(),
            _ => expected.profile = "other profile".into(),
        }
        let before = remote.gets.load(Ordering::SeqCst);
        assert!(
            repository(&remote, true)
                .import(&target, &receipt, &expected)
                .is_err()
        );
        assert_eq!(remote.gets.load(Ordering::SeqCst) - before, 2);
        no_publication(&target_root);
    }
    let invalid = SnapshotTransfer {
        version: 1,
        snapshot_id: "../escape".into(),
        transfer_id: receipt.transfer_id.clone(),
    };
    assert!(
        repository(&remote, true)
            .import(&target, &invalid, &compatibility())
            .is_err()
    );
    let unknown = SnapshotTransfer {
        version: 1,
        snapshot_id: id,
        transfer_id: "0".repeat(64),
    };
    assert!(
        repository(&remote, true)
            .import(&target, &unknown, &compatibility())
            .is_err()
    );
    no_publication(&target_root);
}

fn forged(
    remote: &s3::MockS3,
    original: &SnapshotTransfer,
    mutate: impl FnOnce(&mut serde_json::Value, &mut serde_json::Value),
) -> SnapshotTransfer {
    let mut objects = remote.objects.lock().unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&objects[&format!("{PREFIX}/manifests/{}", original.snapshot_id)])
            .unwrap();
    let mut transfer: serde_json::Value =
        serde_json::from_slice(&objects[&format!("{PREFIX}/transfers/{}", original.transfer_id)])
            .unwrap();
    mutate(&mut manifest, &mut transfer);
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let snapshot_id = hash(&manifest);
    transfer["snapshot_id"] = snapshot_id.clone().into();
    let transfer = serde_json::to_vec(&transfer).unwrap();
    let transfer_id = hash(&transfer);
    objects.insert(format!("{PREFIX}/manifests/{snapshot_id}"), manifest);
    objects.insert(format!("{PREFIX}/transfers/{transfer_id}"), transfer);
    SnapshotTransfer {
        version: 1,
        snapshot_id,
        transfer_id,
    }
}

#[test]
fn unsafe_inventory_geometry_and_false_raw_indexes_are_rejected_without_exposing_objects() {
    let remote = s3::MockS3::start();
    let temp = tempfile::tempdir().unwrap();
    let (source, id) = fixture(temp.path(), false);
    let receipt = repository(&remote, false)
        .publish(&source.open(&id, &compatibility()).unwrap())
        .unwrap();
    for fault in [
        "escape",
        "absolute",
        "duplicate",
        "symlink-parent",
        "hardlink-origin",
        "false-index",
        "oversized",
        "unknown",
        "missing-file",
        "extra-file",
        "wrong-digest",
        "unreadable-mode",
    ] {
        let forged = forged(&remote, &receipt, |manifest, transfer| match fault {
            "escape" => {
                manifest["filesystem"]["entries"][1]["path"] = serde_json::json!(b"../outside")
            }
            "absolute" => {
                manifest["filesystem"]["entries"][1]["path"] = serde_json::json!(b"/outside")
            }
            "duplicate" => {
                manifest["filesystem"]["entries"][2]["path"] =
                    manifest["filesystem"]["entries"][1]["path"].clone()
            }
            "symlink-parent" => {
                manifest["filesystem"]["entries"][2]["path"] = serde_json::json!(b"link/child")
            }
            "hardlink-origin" => {
                manifest["filesystem"]["entries"][1]["object"]["hardlink"] =
                    serde_json::json!(b"outside")
            }
            "false-index" => manifest["ram_index"]["sha256"][0] = "0".repeat(64).into(),
            "oversized" => transfer["ram"]["payload"]["bytes"] = (1u64 << 50).into(),
            "unknown" => transfer["unexpected"] = true.into(),
            "missing-file" => {
                transfer["files"].as_array_mut().unwrap().pop();
            }
            "extra-file" => {
                let extra = transfer["files"][0].clone();
                transfer["files"].as_array_mut().unwrap().push(extra);
            }
            "wrong-digest" => transfer["machine"]["sha256"] = "0".repeat(64).into(),
            "unreadable-mode" => {
                manifest["filesystem"]["entries"][0]["mode"] = libc::S_IFDIR.into()
            }
            _ => unreachable!(),
        });
        let target_root = temp.path().join(format!("target-{fault}"));
        let target = SnapshotStore::new(&target_root).unwrap();
        assert!(
            repository(&remote, true)
                .import(&target, &forged, &compatibility())
                .is_err(),
            "{fault}"
        );
        no_publication(&target_root);
    }
}

#[test]
fn interrupted_publication_has_no_transfer_receipt_and_retry_is_idempotent() {
    let remote = s3::MockS3::start();
    let temp = tempfile::tempdir().unwrap();
    let (source, id) = fixture(temp.path(), false);
    let published = source.open(&id, &compatibility()).unwrap();
    let writer = repository(&remote, false);
    remote.read_only.store(true, Ordering::SeqCst);
    assert!(writer.publish(&published).is_err());
    assert!(
        !remote
            .objects
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.contains("/transfers/"))
    );
    remote.read_only.store(false, Ordering::SeqCst);
    let receipt = writer.publish(&published).unwrap();
    assert_eq!(writer.publish(&published).unwrap(), receipt);
    let target_root = temp.path().join("target");
    let target = SnapshotStore::new(&target_root).unwrap();
    remote.deny_reads.store(true, Ordering::SeqCst);
    assert!(
        repository(&remote, true)
            .import(&target, &receipt, &compatibility())
            .is_err()
    );
    no_publication(&target_root);
    remote.deny_reads.store(false, Ordering::SeqCst);
    repository(&remote, true)
        .import(&target, &receipt, &compatibility())
        .unwrap();
    let held = target.open(&id, &compatibility()).unwrap();
    assert!(
        repository(&remote, true)
            .import(&target, &receipt, &compatibility())
            .is_err()
    );
    drop(held);
    repository(&remote, true)
        .import(&target, &receipt, &compatibility())
        .unwrap();
}

#[test]
fn corrupt_existing_content_cannot_be_acknowledged_by_an_idempotent_publisher() {
    let remote = s3::MockS3::start();
    let temp = tempfile::tempdir().unwrap();
    let (source, id) = fixture(temp.path(), false);
    let published = source.open(&id, &compatibility()).unwrap();
    let writer = repository(&remote, false);
    let receipt = writer.publish(&published).unwrap();
    let mut objects = remote.objects.lock().unwrap();
    let transfer: serde_json::Value =
        serde_json::from_slice(&objects[&format!("{PREFIX}/transfers/{}", receipt.transfer_id)])
            .unwrap();
    let machine = transfer["machine"]["chunks"][0].as_str().unwrap();
    let key = format!("{PREFIX}/chunks/{machine}");
    let correct = objects[&key].clone();
    objects.get_mut(&key).unwrap()[0] ^= 1;
    drop(objects);
    assert!(writer.publish(&published).is_err());
    remote.objects.lock().unwrap().insert(key, correct);
    assert_eq!(writer.publish(&published).unwrap(), receipt);
}

#[test]
fn filesystem_repository_reuses_chunks_and_imported_compressed_frames_across_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (source, first) = fixture(root, true);
    let pending = source.begin().unwrap();
    let mut ram = pending.create_ram().unwrap();
    ram.set_len(4 * 1024 * 1024 + 23).unwrap();
    ram.seek(SeekFrom::Start(1_048_600)).unwrap();
    ram.write_all(b"live guest memory").unwrap();
    let second = pending
        .publish_compressed(
            &root.join("source"),
            b"second machine state",
            compatibility(),
        )
        .unwrap();
    assert_ne!(first, second);
    let remote_root = root.join("remote-volume");
    let writer = SnapshotRepository::filesystem(&remote_root, false).unwrap();
    let first_receipt = writer
        .publish(&source.open(&first, &compatibility()).unwrap())
        .unwrap();
    let chunk_root = remote_root.join("pvisor-checkpoints-v1/chunks");
    let chunks = fs::read_dir(&chunk_root).unwrap().count();
    let second_receipt = writer
        .publish(&source.open(&second, &compatibility()).unwrap())
        .unwrap();
    assert_eq!(
        fs::read_dir(&chunk_root).unwrap().count(),
        chunks + 1,
        "only changed machine bytes need a new chunk"
    );
    fs::remove_dir_all(root.join("source-store")).unwrap();
    fs::remove_dir_all(root.join("source")).unwrap();
    let reader = SnapshotRepository::filesystem(&remote_root, true).unwrap();
    let target_root = root.join("target");
    let target = SnapshotStore::new(&target_root).unwrap();
    reader
        .import(&target, &first_receipt, &compatibility())
        .unwrap();
    let content = target_root.join("content");
    let frames = fs::read_dir(&content).unwrap().count();
    reader
        .import(&target, &second_receipt, &compatibility())
        .unwrap();
    assert_eq!(fs::read_dir(&content).unwrap().count(), frames);
    for frame in fs::read_dir(&content).unwrap() {
        assert_eq!(
            frame.unwrap().metadata().unwrap().nlink(),
            3,
            "two snapshots share one sealed frame inode"
        );
    }
    target.delete(&first).unwrap();
    target.collect_abandoned().unwrap();
    assert_eq!(fs::read_dir(&content).unwrap().count(), frames);
    assert_eq!(
        target
            .open(&second, &compatibility())
            .unwrap()
            .machine_bytes()
            .unwrap(),
        b"second machine state"
    );
    target.delete(&second).unwrap();
    target.collect_abandoned().unwrap();
    assert_eq!(fs::read_dir(&content).unwrap().count(), 0);
}
