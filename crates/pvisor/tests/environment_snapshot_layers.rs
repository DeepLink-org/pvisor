//! Immutable layer references own their data independently of parent snapshots.
#![cfg(target_os = "linux")]
use pvisor::environment_snapshot::{
    Compatibility, SharedFilesystemLayer, SnapshotLayer, SnapshotRepository, SnapshotStore,
    inventory,
};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::Path,
};
#[path = "common/s3.rs"]
mod s3;

fn s3_repository(remote: &s3::MockS3, read_only: bool) -> SnapshotRepository {
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
    SnapshotRepository::object_store(std::sync::Arc::new(client), "team", read_only).unwrap()
}

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "layer-test-boot".into(),
        build: "layer-worker".into(),
        firmware: "layer-kernel".into(),
        profile: "layer-profile".into(),
    }
}

fn source_layers(root: &Path) -> (SnapshotStore, String, Vec<SharedFilesystemLayer>) {
    source_layers_with_permissions(root, false)
}

fn make_private_directories_writable(path: &Path) {
    if fs::symlink_metadata(path).unwrap().is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        for entry in fs::read_dir(path).unwrap() {
            make_private_directories_writable(&entry.unwrap().path());
        }
    }
}

fn set_test_xattr(path: &Path) {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let value = b"preserve read-only metadata";
    assert_eq!(
        unsafe {
            libc::lsetxattr(
                path.as_ptr(),
                c"user.pvisor-layer-metadata".as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

fn set_read_only_acl(path: &Path, directory: bool) {
    let permissions = if directory { 5u16 } else { 4u16 };
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, mode, id) in [
        (1u16, permissions, u32::MAX),
        (2, permissions, 12345),
        (4, permissions, u32::MAX),
        (16, permissions, u32::MAX),
        (32, if directory { 5 } else { 0 }, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(mode.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
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
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

fn source_layers_with_permissions(
    root: &Path,
    read_only: bool,
) -> (SnapshotStore, String, Vec<SharedFilesystemLayer>) {
    let source = root.join("source");
    fs::create_dir_all(source.join("lower/unvisited")).unwrap();
    fs::create_dir(source.join("upper")).unwrap();
    fs::write(source.join("lower/data"), b"immutable lower data").unwrap();
    fs::hard_link(source.join("lower/data"), source.join("lower/alias")).unwrap();
    fs::write(
        source.join("lower/unvisited/file"),
        b"all payloads are retained",
    )
    .unwrap();
    symlink("data", source.join("lower/link")).unwrap();
    fs::hard_link(source.join("lower/link"), source.join("lower/link-alias")).unwrap();
    fs::set_permissions(source.join("lower/data"), fs::Permissions::from_mode(0o640)).unwrap();
    if read_only {
        for name in ["lower", "lower/unvisited", "lower/data"] {
            set_test_xattr(&source.join(name));
            set_read_only_acl(&source.join(name), name != "lower/data");
        }
        fs::set_permissions(source.join("lower/data"), fs::Permissions::from_mode(0o440)).unwrap();
        fs::set_permissions(
            source.join("lower/unvisited"),
            fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        fs::set_permissions(source.join("lower"), fs::Permissions::from_mode(0o555)).unwrap();
    }
    pvisor::environment_snapshot::copy_owned_tree(
        &source.join("lower"),
        &source.join("equal-lower"),
    )
    .unwrap();
    let store =
        SnapshotStore::with_filesystem_pool(&root.join("parent"), &root.join("pool")).unwrap();
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(b"parent RAM")
        .unwrap();
    let id = pending
        .publish(&source, b"parent machine", compatibility())
        .unwrap();
    let snapshot = store.open(&id, &compatibility()).unwrap();
    let references = root.join("source-references");
    fs::create_dir(&references).unwrap();
    let owners = ["lower", "equal-lower"]
        .iter()
        .map(|name| {
            snapshot
                .share_readonly_layer(Path::new(name), &references)
                .unwrap()
        })
        .collect();
    drop(snapshot);
    make_private_directories_writable(&source);
    fs::remove_dir_all(source).unwrap();
    (store, id, owners)
}

fn publish_child(
    root: &Path,
    store: &SnapshotStore,
    owners: &[SharedFilesystemLayer],
    compressed: bool,
) -> String {
    publish_child_with_permissions(root, store, owners, compressed, false)
}

fn publish_child_with_permissions(
    root: &Path,
    store: &SnapshotStore,
    owners: &[SharedFilesystemLayer],
    compressed: bool,
    read_only: bool,
) -> String {
    let private = root.join("private");
    fs::create_dir_all(private.join("upper")).unwrap();
    fs::create_dir(private.join("work")).unwrap();
    fs::create_dir(private.join("a")).unwrap();
    fs::write(private.join("a/child"), b"directory preorder").unwrap();
    fs::hard_link(private.join("a/child"), private.join("a-")).unwrap();
    fs::write(private.join("upper/private"), b"branch writes are private").unwrap();
    if read_only {
        set_test_xattr(&private);
        set_test_xattr(&private.join("upper"));
        set_read_only_acl(&private, true);
        set_read_only_acl(&private.join("upper"), true);
        fs::set_permissions(private.join("upper"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(b"child RAM")
        .unwrap();
    let lower_binding = root.join("removed-original-binding/lower");
    let equal_binding = root.join("removed-original-binding/equal-lower");
    let id = pending
        .publish_layered(
            &private,
            b"child machine",
            compatibility(),
            compressed,
            &[
                SnapshotLayer {
                    path: Path::new("lower"),
                    source: &lower_binding,
                    owner: &owners[0],
                },
                SnapshotLayer {
                    path: Path::new("equal-lower"),
                    source: &equal_binding,
                    owner: &owners[1],
                },
            ],
        )
        .unwrap();
    make_private_directories_writable(&private);
    fs::remove_dir_all(private).unwrap();
    id
}

#[test]
fn read_only_directories_survive_import_and_materialization_and_are_reclaimed_after_last_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (parent, parent_id, owners) = source_layers_with_permissions(root, true);
    let child =
        SnapshotStore::with_filesystem_pool(&root.join("child"), &root.join("pool")).unwrap();
    let id = publish_child_with_permissions(root, &child, &owners, true, true);
    parent.delete(&parent_id).unwrap();
    drop(parent);
    fs::remove_dir_all(root.join("parent")).unwrap();
    let snapshot = child.open(&id, &compatibility()).unwrap();
    let expected = snapshot.complete_inventory().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let writer = SnapshotRepository::filesystem(remote.path(), false).unwrap();
    let receipt = writer.publish(&snapshot).unwrap();
    drop(snapshot);
    drop(owners);
    fs::remove_dir_all(root.join("source-references")).unwrap();
    child.delete(&id).unwrap();
    child.collect_abandoned().unwrap();
    assert_eq!(
        fs::read_dir(root.join("pool/filesystems")).unwrap().count(),
        0
    );

    let imported =
        SnapshotStore::with_filesystem_pool(&root.join("imported"), &root.join("independent-pool"))
            .unwrap();
    let reader = SnapshotRepository::filesystem(remote.path(), true).unwrap();
    for _ in 0..2 {
        reader
            .import(&imported, &receipt, &compatibility())
            .unwrap();
        assert_eq!(
            fs::read_dir(root.join("imported/pending")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(root.join("independent-pool/pending"))
                .unwrap()
                .count(),
            0
        );
    }
    let restored = imported.open(&id, &compatibility()).unwrap();
    assert_eq!(restored.complete_inventory().unwrap(), expected);
    let materialized = root.join("restored");
    restored.materialize(&materialized).unwrap();
    assert_eq!(inventory(&materialized).unwrap(), expected);
    let references = root.join("last-attempt");
    fs::create_dir(&references).unwrap();
    let active = restored
        .share_readonly_layer(Path::new("lower"), &references)
        .unwrap();
    drop(restored);
    imported.delete(&id).unwrap();
    imported.collect_abandoned().unwrap();
    fs::remove_dir_all(references).unwrap();
    imported.collect_abandoned().unwrap();
    assert_eq!(fs::metadata(active.root()).unwrap().mode() & 0o777, 0o555);
    assert_eq!(
        fs::metadata(active.root().join("unvisited"))
            .unwrap()
            .mode()
            & 0o777,
        0o500
    );
    assert_eq!(
        fs::read(active.root().join("data")).unwrap(),
        b"immutable lower data"
    );
    drop(active);
    imported.collect_abandoned().unwrap();
    assert_eq!(
        fs::read_dir(root.join("independent-pool/filesystems"))
            .unwrap()
            .count(),
        0
    );
    make_private_directories_writable(&materialized);
}

#[test]
fn failed_publication_cleans_read_only_private_copies_without_mutating_shared_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (parent, parent_id, owners) = source_layers_with_permissions(root, true);
    let child =
        SnapshotStore::with_filesystem_pool(&root.join("child"), &root.join("pool")).unwrap();
    let private = root.join("private");
    fs::create_dir_all(private.join("upper")).unwrap();
    fs::write(
        private.join("upper/data"),
        b"must be cleaned after RAM failure",
    )
    .unwrap();
    fs::set_permissions(private.join("upper"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o555)).unwrap();
    let binding = root.join("original/lower");
    // Missing RAM fails after the read-only forest and pool references exist.
    assert!(
        child
            .begin()
            .unwrap()
            .publish_layered(
                &private,
                b"machine",
                compatibility(),
                true,
                &[SnapshotLayer {
                    path: Path::new("lower"),
                    source: &binding,
                    owner: &owners[0]
                }],
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(root.join("child/pending")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(root.join("child/objects")).unwrap().count(), 0);
    assert_eq!(
        fs::metadata(owners[0].root()).unwrap().mode() & 0o777,
        0o555
    );
    assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o555);
    make_private_directories_writable(&private);
    parent.delete(&parent_id).unwrap();
    drop(owners);
    fs::remove_dir_all(root.join("source-references")).unwrap();
    child.collect_abandoned().unwrap();
    assert_eq!(
        fs::read_dir(root.join("pool/filesystems")).unwrap().count(),
        0
    );
}

#[test]
fn pooled_checkpoints_retain_inodes_without_data_aliases_and_survive_parent_store_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (parent, parent_id, owners) = source_layers(root);
    let original = owners
        .iter()
        .map(|owner| fs::metadata(owner.root().join("data")).unwrap())
        .collect::<Vec<_>>();
    assert_ne!(
        original[0].ino(),
        original[1].ino(),
        "equal distinct lowers must not collapse origins"
    );
    let child_root = root.join("child");
    let child = SnapshotStore::with_filesystem_pool(&child_root, &root.join("pool")).unwrap();
    let id = publish_child(root, &child, &owners, true);
    parent.delete(&parent_id).unwrap();
    drop(parent);
    fs::remove_dir_all(root.join("parent")).unwrap();
    drop(owners);
    fs::remove_dir_all(root.join("source-references")).unwrap();
    SnapshotStore::new(&root.join("pool"))
        .unwrap()
        .collect_abandoned()
        .unwrap();
    assert!(
        !child_root
            .join("objects")
            .join(&id)
            .join("rootfs/lower")
            .exists()
    );
    let reopened = SnapshotStore::with_filesystem_pool(&child_root, &root.join("pool")).unwrap();
    let snapshot = reopened.open(&id, &compatibility()).unwrap();
    assert_eq!(snapshot.manifest().version, 4);
    assert_eq!(snapshot.manifest().filesystem_layers.len(), 2);
    for (name, before) in ["lower", "equal-lower"].iter().zip(&original) {
        let lower = snapshot.owned_layer_path(Path::new(name)).unwrap();
        let metadata = fs::metadata(lower.join("data")).unwrap();
        assert_eq!(
            metadata.ino(),
            before.ino(),
            "recapture copied immutable data"
        );
        assert_eq!(
            metadata.nlink(),
            2,
            "marker references must not create writable data aliases"
        );
        assert_eq!(
            inventory(&lower).unwrap(),
            snapshot
                .manifest()
                .filesystem_layers
                .iter()
                .find(|layer| layer.path == name.as_bytes())
                .unwrap()
                .filesystem
        );
    }
    let materialized = root.join("materialized");
    snapshot.materialize(&materialized).unwrap();
    assert_eq!(
        inventory(&materialized).unwrap(),
        snapshot.complete_inventory().unwrap()
    );
    fs::write(materialized.join("lower/data"), b"private restored copy").unwrap();
    assert_eq!(
        fs::read(
            snapshot
                .owned_layer_path(Path::new("lower"))
                .unwrap()
                .join("data")
        )
        .unwrap(),
        b"immutable lower data"
    );
    let references = root.join("last-attempt");
    fs::create_dir(&references).unwrap();
    let active = snapshot
        .share_readonly_layer(Path::new("lower"), &references)
        .unwrap();
    drop(snapshot);
    reopened.delete(&id).unwrap();
    reopened.collect_abandoned().unwrap();
    assert_eq!(
        fs::read_dir(root.join("pool/filesystems")).unwrap().count(),
        1
    );
    // Active owners fence GC even if their retained Attempt directory is removed.
    fs::remove_dir_all(references).unwrap();
    reopened.collect_abandoned().unwrap();
    assert!(active.root().exists());
    drop(active);
    reopened.collect_abandoned().unwrap();
    assert_eq!(
        fs::read_dir(root.join("pool/filesystems")).unwrap().count(),
        0
    );
}

#[test]
fn pooled_children_transport_and_reconstruct_in_an_independent_pool_after_all_sources_disappear() {
    for (compressed, signed_s3) in [(false, false), (true, false), (false, true), (true, true)] {
        let source = tempfile::tempdir().unwrap();
        let root = source.path();
        let (_parent, _id, owners) = source_layers(root);
        let child =
            SnapshotStore::with_filesystem_pool(&root.join("child"), &root.join("pool")).unwrap();
        let id = publish_child(root, &child, &owners, compressed);
        let snapshot = child.open(&id, &compatibility()).unwrap();
        let inventory = snapshot.complete_inventory().unwrap();
        let remote = tempfile::tempdir().unwrap();
        let mock = signed_s3.then(s3::MockS3::start);
        if let Some(mock) = &mock {
            // Immutable snapshot publication must not advance a mutable cache
            // HEAD or depend on receiving its acknowledgement.
            mock.lost_head_ack
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let writer = if let Some(mock) = &mock {
            s3_repository(mock, false)
        } else {
            SnapshotRepository::filesystem(remote.path(), false).unwrap()
        };
        let receipt = writer.publish(&snapshot).unwrap();
        if let Some(mock) = &mock {
            assert!(mock.lost_head_ack.load(std::sync::atomic::Ordering::SeqCst));
            let objects = mock.objects.lock().unwrap();
            let manifest: serde_json::Value = serde_json::from_slice(
                &objects[&format!(
                    "/cache-bucket/team/pvisor-checkpoints-v1/manifests/{}",
                    receipt.snapshot_id
                )],
            )
            .unwrap();
            assert_eq!(manifest["version"], 4);
            assert_eq!(manifest["filesystem_layers"].as_array().unwrap().len(), 2);
            assert!(objects.keys().all(|key| !key.ends_with("/HEAD.json")));
        }
        drop(snapshot);
        drop(owners);
        source.close().unwrap();
        let target = tempfile::tempdir().unwrap();
        let imported = SnapshotStore::with_filesystem_pool(
            &target.path().join("snapshot"),
            &target.path().join("independent-pool"),
        )
        .unwrap();
        if let Some(mock) = &mock {
            mock.read_only
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let reader = if let Some(mock) = &mock {
            s3_repository(mock, true)
        } else {
            SnapshotRepository::filesystem(remote.path(), true).unwrap()
        };
        let puts = mock
            .as_ref()
            .map(|mock| mock.puts.load(std::sync::atomic::Ordering::SeqCst));
        if let Some(mock) = &mock {
            mock.deny_reads
                .store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(
                reader
                    .import(&imported, &receipt, &compatibility())
                    .is_err()
            );
            assert_eq!(
                fs::read_dir(target.path().join("snapshot/objects"))
                    .unwrap()
                    .count(),
                0
            );
            mock.deny_reads
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        reader
            .import(&imported, &receipt, &compatibility())
            .unwrap();
        reader
            .import(&imported, &receipt, &compatibility())
            .unwrap();
        let restored = imported.open(&id, &compatibility()).unwrap();
        assert_eq!(restored.complete_inventory().unwrap(), inventory);
        let private = target.path().join("restored");
        restored.materialize(&private).unwrap();
        assert_eq!(
            pvisor::environment_snapshot::inventory(&private).unwrap(),
            inventory
        );
        let mut ram = Vec::new();
        restored.ram_file().unwrap().read_to_end(&mut ram).unwrap();
        assert_eq!(ram, b"child RAM");
        assert_eq!(restored.machine_bytes().unwrap(), b"child machine");
        if let Some(mock) = &mock {
            assert_eq!(
                Some(mock.puts.load(std::sync::atomic::Ordering::SeqCst)),
                puts
            );
            assert!(mock.gets.load(std::sync::atomic::Ordering::SeqCst) > 0);
        }
    }
}

#[test]
fn pooled_publication_rejects_ambiguous_bindings_private_overlap_wrong_pool_and_corrupt_data() {
    for fault in [
        "path",
        "binding",
        "overlap",
        "duplicate",
        "identity",
        "pool",
        "corrupt",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (_parent, _id, mut owners) = source_layers(root);
        let child_root = root.join("child");
        let child = SnapshotStore::with_filesystem_pool(
            &child_root,
            &root.join(if fault == "pool" {
                "other-pool"
            } else {
                "pool"
            }),
        )
        .unwrap();
        let source = root.join("private");
        fs::create_dir_all(source.join("upper")).unwrap();
        if fault == "overlap" {
            fs::create_dir(source.join("lower")).unwrap();
        }
        if fault == "identity" {
            owners[0].id = "0".repeat(64);
        }
        if fault == "corrupt" {
            fs::write(owners[0].root().join("unvisited/file"), b"corrupt").unwrap();
        }
        let binding = if fault == "binding" {
            root.join("x/../hidden")
        } else {
            root.join("original/lower")
        };
        let path = if fault == "path" {
            Path::new("../outside")
        } else {
            Path::new("lower")
        };
        let mut layers = vec![SnapshotLayer {
            path,
            source: &binding,
            owner: &owners[0],
        }];
        let second = root.join("original/other");
        if fault == "duplicate" {
            layers.push(SnapshotLayer {
                path,
                source: &second,
                owner: &owners[1],
            });
        }
        let pending = child.begin().unwrap();
        pending.create_ram().unwrap().write_all(b"RAM").unwrap();
        assert!(
            pending
                .publish_layered(&source, b"machine", compatibility(), true, &layers)
                .is_err(),
            "{fault}"
        );
        assert_eq!(fs::read_dir(child_root.join("objects")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(child_root.join("pending")).unwrap().count(), 0);
    }
}

#[test]
fn pooled_snapshot_rejects_missing_forged_or_symlinked_marker_references() {
    for fault in ["missing", "forged", "symlink"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (_parent, _id, owners) = source_layers(root);
        let child_root = root.join("child");
        let child = SnapshotStore::with_filesystem_pool(&child_root, &root.join("pool")).unwrap();
        let id = publish_child(root, &child, &owners, true);
        let reference = child_root
            .join("objects")
            .join(&id)
            .join("filesystem-references")
            .join(&owners[0].id);
        fs::remove_file(&reference).unwrap();
        match fault {
            "missing" => {}
            "forged" => fs::write(&reference, &owners[0].id).unwrap(),
            "symlink" => symlink(
                root.join("pool/filesystems")
                    .join(&owners[0].id)
                    .join("reference"),
                &reference,
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(child.open(&id, &compatibility()).is_err(), "{fault}");
        assert!(
            child.open_for_restore(&id, &compatibility()).is_err(),
            "{fault}"
        );
    }
}

#[test]
fn concurrent_jobs_share_one_pool_without_copying_or_collapsing_lower_origins() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_parent, _id, owners) = source_layers(root);
    let inodes = owners
        .iter()
        .map(|owner| fs::metadata(owner.root().join("data")).unwrap().ino())
        .collect::<Vec<_>>();
    std::thread::scope(|scope| {
        let mut children = Vec::new();
        for index in 0..8 {
            let directory = root.join(format!("job-{index}"));
            fs::create_dir(&directory).unwrap();
            let owners = &owners;
            children.push(scope.spawn(move || {
                let store_root = directory.join("snapshot");
                let store =
                    SnapshotStore::with_filesystem_pool(&store_root, &root.join("pool")).unwrap();
                let id = publish_child(&directory, &store, owners, index % 2 == 0);
                let snapshot = store.open(&id, &compatibility()).unwrap();
                ["lower", "equal-lower"]
                    .iter()
                    .map(|name| {
                        fs::metadata(
                            snapshot
                                .owned_layer_path(Path::new(name))
                                .unwrap()
                                .join("data"),
                        )
                        .unwrap()
                        .ino()
                    })
                    .collect::<Vec<_>>()
            }));
        }
        for child in children {
            assert_eq!(child.join().unwrap(), inodes);
        }
    });
    assert_eq!(
        fs::read_dir(root.join("pool/filesystems")).unwrap().count(),
        2
    );
    for owner in &owners {
        assert_eq!(fs::metadata(owner.root().join("data")).unwrap().nlink(), 2);
    }
}

#[test]
fn hostile_layer_metadata_fails_before_bulk_reads_or_local_publication() {
    use pvisor::environment_snapshot::{
        FilesystemLayer, SnapshotTransfer, TreeInventory, TreeObject,
    };
    use sha2::{Digest, Sha256};
    use std::sync::atomic::Ordering;
    fn hash(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    for fault in [
        "path",
        "binding",
        "id",
        "overlap",
        "duplicate",
        "escaping-link",
        "forward-link",
        "layer-count",
        "entry-budget",
    ] {
        let remote = s3::MockS3::start();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (_parent, _id, owners) = source_layers(root);
        let child =
            SnapshotStore::with_filesystem_pool(&root.join("child"), &root.join("pool")).unwrap();
        let id = publish_child(root, &child, &owners, false);
        let receipt = s3_repository(&remote, false)
            .publish(&child.open(&id, &compatibility()).unwrap())
            .unwrap();
        let prefix = "/cache-bucket/team/pvisor-checkpoints-v1";
        let mut objects = remote.objects.lock().unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(
            &objects[&format!("{prefix}/manifests/{}", receipt.snapshot_id)],
        )
        .unwrap();
        let mut transfer: serde_json::Value = serde_json::from_slice(
            &objects[&format!("{prefix}/transfers/{}", receipt.transfer_id)],
        )
        .unwrap();
        let mut layer: FilesystemLayer =
            serde_json::from_value(manifest["filesystem_layers"][0].clone()).unwrap();
        match fault {
            "path" => layer.path = b"../outside".to_vec(),
            "binding" => layer.source = b"/original/../outside".to_vec(),
            "id" => layer.id = "0".repeat(64),
            "overlap" => layer.path = b"upper".to_vec(),
            "duplicate" => {
                layer.source =
                    serde_json::from_value(manifest["filesystem_layers"][1]["source"].clone())
                        .unwrap()
            }
            "layer-count" => {
                manifest["filesystem_layers"] = serde_json::json!(vec![layer.clone(); 129]);
            }
            "entry-budget" => {
                let entry = layer.filesystem.entries[0].clone();
                layer.filesystem.entries.resize(65_536, entry);
            }
            "escaping-link" | "forward-link" => {
                let file = layer
                    .filesystem
                    .entries
                    .iter_mut()
                    .find(|entry| matches!(entry.object, TreeObject::File { .. }))
                    .unwrap();
                let TreeObject::File { hardlink, .. } = &mut file.object else {
                    unreachable!()
                };
                *hardlink = if fault == "escaping-link" {
                    b"../upper/private".to_vec()
                } else {
                    b"data".to_vec()
                };
                #[derive(serde::Serialize)]
                struct Seal<'a> {
                    version: u32,
                    logical_root: &'a [u8],
                    filesystem: &'a TreeInventory,
                }
                layer.id = hash(
                    &serde_json::to_vec(&Seal {
                        version: 1,
                        logical_root: &layer.logical_root,
                        filesystem: &layer.filesystem,
                    })
                    .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        manifest["filesystem_layers"][0] = serde_json::to_value(layer).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let snapshot_id = hash(&bytes);
        objects.insert(format!("{prefix}/manifests/{snapshot_id}"), bytes);
        transfer["snapshot_id"] = serde_json::json!(snapshot_id);
        let bytes = serde_json::to_vec(&transfer).unwrap();
        let transfer_id = hash(&bytes);
        objects.insert(format!("{prefix}/transfers/{transfer_id}"), bytes);
        drop(objects);
        let target = root.join("target");
        let imported =
            SnapshotStore::with_filesystem_pool(&target, &root.join("target-pool")).unwrap();
        let reads = remote.gets.load(Ordering::SeqCst);
        let error = s3_repository(&remote, true)
            .import(
                &imported,
                &SnapshotTransfer {
                    version: 1,
                    snapshot_id,
                    transfer_id,
                },
                &compatibility(),
            )
            .unwrap_err();
        if fault == "layer-count" {
            assert!(
                error
                    .to_string()
                    .contains("too many immutable snapshot layers")
            );
        } else if fault == "entry-budget" {
            assert!(error.to_string().contains("inventory limit"));
        }
        assert_eq!(
            remote.gets.load(Ordering::SeqCst) - reads,
            2,
            "{fault} must reject after the two bounded metadata reads"
        );
        assert_eq!(
            fs::read_dir(target.join("objects")).unwrap().count(),
            0,
            "{fault}"
        );
        assert_eq!(
            fs::read_dir(target.join("pending")).unwrap().count(),
            0,
            "{fault}"
        );
        assert_eq!(
            fs::read_dir(root.join("target-pool/filesystems"))
                .unwrap()
                .count(),
            0,
            "{fault}"
        );
    }
}
