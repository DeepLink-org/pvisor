#![cfg(target_os = "linux")]
use pvisor::environment_snapshot::{Compatibility, SnapshotRepository, SnapshotStore, inventory};
use std::{
    fs,
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, MetadataExt, PermissionsExt, symlink},
    },
    path::Path,
};
#[path = "common/s3.rs"]
#[allow(dead_code)] // Shared fixture also exposes faults used by other suites.
mod s3;

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "private-blocks-boot".into(),
        build: "private-blocks-build".into(),
        firmware: "private-blocks-kernel".into(),
        profile: "private-blocks-profile".into(),
    }
}
fn make_source(root: &Path) {
    fs::create_dir(root).unwrap();
    fs::create_dir(root.join("upper")).unwrap();
    let data = (0..3 * 65536)
        .map(|index| (index % 251 + index / 65536) as u8)
        .collect::<Vec<_>>();
    fs::write(root.join("upper/data"), data).unwrap();
    fs::hard_link(root.join("upper/data"), root.join("upper/alias")).unwrap();
    fs::write(root.join("upper/empty"), b"").unwrap();
    fs::write(root.join("upper/unvisited"), b"private unseen data").unwrap();
    symlink("../../outside", root.join("upper/link")).unwrap();
    fs::hard_link(root.join("upper/link"), root.join("upper/link-alias")).unwrap();
    fs::write(
        root.join(std::ffi::OsStr::from_bytes(b"upper/native-\xff")),
        b"native path",
    )
    .unwrap();
    let sparse = fs::File::create(root.join("upper/sparse")).unwrap();
    sparse.set_len(1024 * 1024 + 123).unwrap();
    sparse.write_all_at(b"end", 1024 * 1024 + 13).unwrap();
    let path = std::ffi::CString::new(root.join("upper/data").as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::lsetxattr(
                path.as_ptr(),
                c"user.pvisor-private-blocks".as_ptr(),
                b"metadata".as_ptr().cast(),
                8,
                0,
            )
        },
        0
    );
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, mode, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, 12345),
        (4, 4, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(mode.to_le_bytes());
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
    fs::write(root.join("upper/readonly"), b"read only").unwrap();
    fs::set_permissions(
        root.join("upper/readonly"),
        fs::Permissions::from_mode(0o440),
    )
    .unwrap();
}
fn publish(store: &SnapshotStore, root: &Path, compressed: bool) -> String {
    let pending = store.begin().unwrap();
    pending
        .create_ram()
        .unwrap()
        .write_all(&vec![29; 131_073])
        .unwrap();
    pending
        .publish_chunked_filesystem(root, b"opaque machine state", compatibility(), compressed)
        .unwrap()
}

#[test]
fn private_children_share_unchanged_frames_and_restore_independent_inodes_after_parent_store_gc() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    make_source(&source);
    let pool = root.join("pool");
    let parent_store = SnapshotStore::with_filesystem_pool(&root.join("parent"), &pool).unwrap();
    let parent_id = publish(&parent_store, &source, false);
    let parent = parent_store.open(&parent_id, &compatibility()).unwrap();
    assert_eq!(parent.manifest().version, 5);
    assert!(
        !root
            .join("parent/objects")
            .join(&parent_id)
            .join("rootfs")
            .exists()
    );
    let parent_blocks = parent.manifest().filesystem_blocks.clone().unwrap();
    let old = parent_blocks
        .files
        .iter()
        .find(|file| file.path == b"upper/alias")
        .unwrap();
    assert_eq!(old.blocks.len(), 3);
    let before = fs::read_dir(pool.join("content")).unwrap().count();
    let inodes = old
        .blocks
        .iter()
        .map(|id| {
            fs::metadata(
                root.join("parent/objects")
                    .join(&parent_id)
                    .join("filesystem-blocks")
                    .join(id.as_ref().unwrap()),
            )
            .unwrap()
            .ino()
        })
        .collect::<Vec<_>>();
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(source.join("upper/data"))
        .unwrap();
    writer.write_all_at(b"x", 65536 + 17).unwrap();
    drop(writer);
    let expected = inventory(&source).unwrap();
    let child_store = SnapshotStore::with_filesystem_pool(&root.join("child"), &pool).unwrap();
    let child_id = publish(&child_store, &source, true);
    let child = child_store.open(&child_id, &compatibility()).unwrap();
    let blocks = child.manifest().filesystem_blocks.as_ref().unwrap();
    let new = blocks
        .files
        .iter()
        .find(|file| file.path == b"upper/alias")
        .unwrap();
    assert_eq!(old.blocks[0], new.blocks[0]);
    assert_ne!(old.blocks[1], new.blocks[1]);
    assert_eq!(old.blocks[2], new.blocks[2]);
    assert_eq!(
        fs::read_dir(pool.join("content")).unwrap().count(),
        before + 1,
        "one changed 64 KiB frame must create only one new filesystem object"
    );
    for index in [0, 2] {
        assert_eq!(
            fs::metadata(
                root.join("child/objects")
                    .join(&child_id)
                    .join("filesystem-blocks")
                    .join(new.blocks[index].as_ref().unwrap())
            )
            .unwrap()
            .ino(),
            inodes[index]
        );
    }
    let sparse = blocks
        .files
        .iter()
        .find(|file| file.path == b"upper/sparse")
        .unwrap();
    assert!(sparse.blocks[..16].iter().all(Option::is_none));
    assert!(sparse.blocks[16].is_some());
    assert!(
        blocks
            .files
            .iter()
            .find(|file| file.path == b"upper/empty")
            .unwrap()
            .blocks
            .is_empty()
    );
    drop(parent);
    parent_store.delete(&parent_id).unwrap();
    drop(parent_store);
    fs::remove_dir_all(root.join("parent")).unwrap();
    SnapshotStore::new(&pool)
        .unwrap()
        .collect_abandoned()
        .unwrap();
    let first = root.join("restore-first");
    let second = root.join("restore-second");
    child.materialize(&first).unwrap();
    child.materialize(&second).unwrap();
    assert_eq!(inventory(&first).unwrap(), expected);
    assert_eq!(inventory(&second).unwrap(), expected);
    assert_eq!(
        fs::metadata(first.join("upper/data")).unwrap().ino(),
        fs::metadata(first.join("upper/alias")).unwrap().ino()
    );
    assert_ne!(
        fs::metadata(first.join("upper/data")).unwrap().ino(),
        fs::metadata(second.join("upper/data")).unwrap().ino()
    );
    assert_eq!(fs::metadata(first.join("upper/data")).unwrap().nlink(), 2);
    assert!(fs::metadata(first.join("upper/sparse")).unwrap().blocks() * 512 < 65536);
    let layer = root.join("layer");
    child.materialize_layer(Path::new("upper"), &layer).unwrap();
    assert_eq!(
        inventory(&layer).unwrap(),
        inventory(&source.join("upper")).unwrap()
    );
    assert!(child.owned_layer_path(Path::new("upper")).is_err());
    fs::OpenOptions::new()
        .write(true)
        .open(first.join("upper/data"))
        .unwrap()
        .write_all_at(b"private", 0)
        .unwrap();
    assert_eq!(inventory(&second).unwrap(), expected);
    assert_eq!(inventory(&source).unwrap(), expected);
    drop(child);
    child_store.delete(&child_id).unwrap();
    child_store.collect_abandoned().unwrap();
    assert_eq!(fs::read_dir(pool.join("content")).unwrap().count(), 0);
}

fn roundtrip(signed_s3: bool, compressed: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    make_source(&source);
    let expected = inventory(&source).unwrap();
    let publisher =
        SnapshotStore::with_filesystem_pool(&root.join("publisher"), &root.join("publisher-pool"))
            .unwrap();
    let id = publish(&publisher, &source, compressed);
    let snapshot = publisher.open(&id, &compatibility()).unwrap();
    let remote = signed_s3.then(s3::MockS3::start);
    let repository = if let Some(remote) = &remote {
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
        SnapshotRepository::object_store(std::sync::Arc::new(client), "team", false).unwrap()
    } else {
        SnapshotRepository::filesystem(&root.join("remote"), false).unwrap()
    };
    let receipt = repository.publish(&snapshot).unwrap();
    let original = fs::read(
        root.join(if root.join("publisher").exists() {
            "publisher/objects"
        } else {
            "store/objects"
        })
        .join(&id)
        .join("manifest.json"),
    )
    .unwrap();
    drop(snapshot);
    drop(publisher);
    fs::remove_dir_all(root.join("publisher")).unwrap();
    fs::remove_dir_all(root.join("publisher-pool")).unwrap();
    fs::remove_dir_all(&source).unwrap();
    let receiver =
        SnapshotStore::with_filesystem_pool(&root.join("receiver"), &root.join("receiver-pool"))
            .unwrap();
    repository
        .import(&receiver, &receipt, &compatibility())
        .unwrap();
    let imported = receiver.open(&id, &compatibility()).unwrap();
    assert_eq!(
        fs::read(
            root.join("receiver/objects")
                .join(&id)
                .join("manifest.json")
        )
        .unwrap(),
        original
    );
    assert!(
        !root
            .join("receiver/objects")
            .join(&id)
            .join("rootfs")
            .exists()
    );
    imported.materialize(&root.join("restored")).unwrap();
    assert_eq!(inventory(&root.join("restored")).unwrap(), expected);
    assert_eq!(repository.publish(&imported).unwrap(), receipt);
    drop(imported);
    repository
        .import(&receiver, &receipt, &compatibility())
        .unwrap();
    if let Some(remote) = remote {
        assert!(remote.gets.load(std::sync::atomic::Ordering::SeqCst) > 0);
        assert!(remote.puts.load(std::sync::atomic::Ordering::SeqCst) > 0);
    }
}
#[test]
fn raw_private_filesystem_blocks_transfer_without_publisher_storage() {
    roundtrip(false, false);
}
#[test]
fn compressed_private_filesystem_blocks_signed_s3_transfer_without_publisher_storage() {
    roundtrip(true, true);
}

#[test]
fn damaged_private_frames_and_unexpected_tree_data_are_rejected_before_restore() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    make_source(&source);
    let store = SnapshotStore::new(&root.join("store")).unwrap();
    let id = publish(&store, &source, false);
    let snapshot = store.open(&id, &compatibility()).unwrap();
    let path = root
        .join(if root.join("publisher").exists() {
            "publisher/objects"
        } else {
            "store/objects"
        })
        .join(&id)
        .to_owned();
    let frame = snapshot
        .manifest()
        .filesystem_blocks
        .as_ref()
        .unwrap()
        .files
        .iter()
        .flat_map(|file| file.blocks.iter().flatten())
        .next()
        .unwrap();
    let reference = path.join("filesystem-blocks").join(frame);
    let bytes = fs::read(&reference).unwrap();
    drop(snapshot);
    fs::write(&reference, b"truncated frame").unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    fs::write(&reference, &bytes).unwrap();
    fs::remove_file(&reference).unwrap();
    symlink(root.join("outside"), &reference).unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    fs::remove_file(&reference).unwrap();
    fs::write(&reference, bytes).unwrap();
    fs::create_dir(path.join("rootfs")).unwrap();
    assert!(store.open(&id, &compatibility()).is_err());
    fs::remove_dir(path.join("rootfs")).unwrap();
    assert!(store.open(&id, &compatibility()).is_ok());
}

#[test]
fn forged_private_block_indexes_are_rejected_even_with_a_matching_manifest_digest() {
    use sha2::{Digest, Sha256};
    for fault in 0..7 {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        make_source(&source);
        let store = SnapshotStore::new(&root.join("store")).unwrap();
        let id = publish(&store, &source, false);
        let directory = root.join("store/objects").join(&id);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        match fault {
            0 => manifest["version"] = 4.into(),
            1 => {
                manifest
                    .as_object_mut()
                    .unwrap()
                    .remove("filesystem_blocks");
            }
            2 => {
                let files = manifest["filesystem_blocks"]["files"]
                    .as_array_mut()
                    .unwrap();
                files.push(files[0].clone());
            }
            3 => {
                manifest["filesystem_blocks"]["files"][0]["blocks"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            4 => manifest["filesystem_blocks"]["files"][0]["blocks"][0] = "A".repeat(64).into(),
            5 => {
                let path = serde_json::to_value(b"../escape".to_vec()).unwrap();
                manifest["filesystem_blocks"]["files"][0]["path"] = path;
            }
            6 => {
                // Preserve equality of the hard-link group's metadata so
                // rejection requires checking actual complete file data.
                let primary = serde_json::to_value(b"upper/alias".to_vec()).unwrap();
                for entry in manifest["filesystem"]["entries"].as_array_mut().unwrap() {
                    if entry["object"]["kind"] == "File" && entry["object"]["hardlink"] == primary {
                        entry["object"]["sha256"] = "0".repeat(64).into();
                    }
                }
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let forged_id = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_ne!(forged_id, id, "fault must change authenticated metadata");
        let forged = root.join("store/objects").join(&forged_id);
        fs::rename(directory, &forged).unwrap();
        fs::write(forged.join("manifest.json"), bytes).unwrap();
        assert!(
            store
                .open_for_restore(&forged_id, &compatibility())
                .is_err(),
            "forged private index accepted: fault {fault}"
        );
        assert!(!root.join("escape").exists());
    }
}
