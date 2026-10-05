//! Transfer a trusted native capture's private forest without copying it again.
//! Legacy captures contain copied backing; direct captures contain only the
//! empty forest root. The supervisor keeps all devices frozen during sealing.
use super::{TreeInventory, TreeObject, inventory, native_path, store, verify_tree};
use anyhow::{Context, ensure};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

pub(super) fn take_native_tree(
    store_root: &Path,
    source: &Path,
    destination: &Path,
) -> anyhow::Result<TreeInventory> {
    let _moving = store::gate(store_root, false)?;
    ensure!(
        source.file_name().is_some_and(|name| name == "rootfs")
            && source.parent().and_then(Path::parent)
                == Some(store_root.join("captures").as_path())
            && fs::symlink_metadata(source)?.is_dir()
            && source.canonicalize()? == source,
        "ownership transfer requires a canonical native capture tree"
    );
    let source_parent = source.parent().context("missing capture parent")?;
    let destination_parent = destination.parent().context("missing pending parent")?;
    ensure!(
        fs::metadata(source)?.dev() == fs::metadata(destination_parent)?.dev(),
        "native capture ownership transfer requires the same filesystem"
    );
    let expected = inventory(source)?;
    // Inventory includes unvisited data, metadata, ACLs/xattrs and topology.
    // Sync files and directories from the leaves up, without following links.
    for entry in expected.entries.iter().rev() {
        if !matches!(entry.object, TreeObject::Symlink { .. }) {
            use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(source.join(std::ffi::OsStr::from_bytes(&entry.path)))?
                .sync_all()?;
        }
    }
    let old = native_path(source)?;
    let new = native_path(destination)?;
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(old.as_ptr(), new.as_ptr(), libc::RENAME_EXCL) };
    ensure!(
        rc == 0,
        "native capture ownership transfer: {}",
        std::io::Error::last_os_error()
    );
    fs::File::open(source_parent)?.sync_all()?;
    fs::File::open(destination_parent)?.sync_all()?;
    verify_tree(destination, &expected)
        .context("native capture changed during ownership transfer")?;
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_snapshot::{Compatibility, SnapshotStore};
    use std::{
        io::{Read, Seek, SeekFrom, Write},
        os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        },
        path::PathBuf,
    };

    #[test]
    fn guest_projection_preserves_visible_content_and_rejects_cross_boundary_links() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("stage")).unwrap();
        fs::write(source.join("visible"), b"all guest bytes").unwrap();
        fs::write(source.join("stage/control"), b"host management").unwrap();
        let excluded = vec![PathBuf::from("stage")];
        let expected = super::super::inventory_projected(&source, &excluded).unwrap();
        let copied =
            super::super::copy_guest_tree(&source, &temp.path().join("copy"), &excluded).unwrap();
        assert_eq!(copied, expected);
        assert!(!temp.path().join("copy/stage").exists());
        fs::write(source.join("stage/control"), b"updated host receipt").unwrap();
        assert_eq!(
            super::super::inventory_projected(&source, &excluded).unwrap(),
            expected
        );
        fs::write(source.join("visible"), b"changed guest bytes").unwrap();
        assert_ne!(
            super::super::inventory_projected(&source, &excluded).unwrap(),
            expected
        );
        fs::hard_link(source.join("visible"), source.join("stage/alias")).unwrap();
        assert!(super::super::inventory_projected(&source, &excluded).is_err());
        assert!(
            super::super::inventory_projected(&source, &[PathBuf::from("../visible")]).is_err()
        );
    }

    #[test]
    fn guest_projection_excludes_hidden_sockets_without_weakening_owned_tree_audit() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("stage")).unwrap();
        fs::write(source.join("visible"), b"preserved").unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(source.join("stage/control.sock")).unwrap();
        assert!(super::super::copy_owned_tree(&source, &temp.path().join("full")).is_err());
        super::super::copy_guest_tree(
            &source,
            &temp.path().join("guest"),
            &[PathBuf::from("stage")],
        )
        .unwrap();
        assert_eq!(
            fs::read(temp.path().join("guest/visible")).unwrap(),
            b"preserved"
        );
    }

    fn fixture() -> (tempfile::TempDir, SnapshotStore, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let store_root = temp.path().canonicalize().unwrap().join("store");
        let store = SnapshotStore::new(&store_root).unwrap();
        let capture = store_root.join("captures/capture-test");
        fs::create_dir_all(&capture).unwrap();
        let source = capture.join("rootfs");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), b"owned capture").unwrap();
        fs::hard_link(source.join("data"), source.join("alias")).unwrap();
        symlink("data", source.join("link")).unwrap();
        fs::create_dir(source.join("unvisited")).unwrap();
        fs::write(source.join("unvisited/private"), b"full inventory").unwrap();
        fs::set_permissions(source.join("data"), fs::Permissions::from_mode(0o640)).unwrap();
        #[cfg(target_os = "linux")]
        {
            let file = native_path(&source.join("data")).unwrap();
            assert_eq!(
                unsafe {
                    libc::lsetxattr(
                        file.as_ptr(),
                        c"user.pvisor-capture".as_ptr(),
                        b"metadata".as_ptr().cast(),
                        8,
                        0,
                    )
                },
                0
            );
        }
        (temp, store, store_root, source)
    }

    fn compatibility() -> Compatibility {
        Compatibility {
            host_boot: "boot".into(),
            build: "build".into(),
            firmware: "firmware".into(),
            profile: "native-owned-test".into(),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn direct_native_private_sources_restore_after_original_backing_is_deleted() {
        use crate::environment_snapshot::{CapturedFilesystemSource, NativeLayerCapture};
        for compressed in [false, true] {
            let (temp, _old_store, store_root, capture) = fixture();
            let root = temp.path().canonicalize().unwrap();
            let private = root.join("original-private");
            fs::rename(&capture, &private).unwrap();
            fs::create_dir(&capture).unwrap();
            let expected = inventory(&private).unwrap();
            let before = fs::metadata(private.join("data")).unwrap();
            let pool = root.join("pool");
            let store = SnapshotStore::with_filesystem_pool(&store_root, &pool).unwrap();
            let ram = capture.parent().unwrap().join("capture.ram");
            fs::write(&ram, b"direct native RAM").unwrap();
            let id = store
                .begin()
                .unwrap()
                .publish_native_retained(
                    &capture,
                    &fs::File::open(&ram).unwrap(),
                    b"direct native machine",
                    compatibility(),
                    compressed,
                    NativeLayerCapture {
                        private_sources: &[CapturedFilesystemSource {
                            path: "layer-001".into(),
                            source: private.clone(),
                            excluded: Vec::new(),
                        }],
                        layers: &[],
                        delta: None,
                    },
                )
                .unwrap()
                .id;
            assert!(!capture.exists());
            let after = fs::metadata(private.join("data")).unwrap();
            assert_eq!(
                (
                    before.ino(),
                    before.nlink(),
                    before.mode(),
                    before.ctime(),
                    before.ctime_nsec()
                ),
                (
                    after.ino(),
                    after.nlink(),
                    after.mode(),
                    after.ctime(),
                    after.ctime_nsec()
                )
            );
            assert_eq!(inventory(&private).unwrap(), expected);
            fs::remove_dir_all(&private).unwrap();
            let published = store.open(&id, &compatibility()).unwrap();
            assert_eq!(published.manifest().version, 5);
            assert!(!published.directory().join("rootfs").exists());
            let first = root.join("restored-first");
            let second = root.join("restored-second");
            published
                .materialize_layer(Path::new("layer-001"), &first)
                .unwrap();
            published
                .materialize_layer(Path::new("layer-001"), &second)
                .unwrap();
            assert_eq!(inventory(&first).unwrap(), expected);
            assert_eq!(inventory(&second).unwrap(), expected);
            assert_eq!(fs::metadata(first.join("data")).unwrap().nlink(), 2);
            assert_eq!(
                fs::metadata(first.join("data")).unwrap().ino(),
                fs::metadata(first.join("alias")).unwrap().ino()
            );
            assert_ne!(
                fs::metadata(first.join("data")).unwrap().ino(),
                fs::metadata(second.join("data")).unwrap().ino()
            );
            fs::write(first.join("alias"), b"first attempt mutation").unwrap();
            assert_eq!(fs::read(second.join("data")).unwrap(), b"owned capture");
            let mut bytes = Vec::new();
            published
                .open_file(Path::new("layer-001/unvisited/private"))
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"full inventory");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn direct_native_private_sources_reject_mixed_forests_aliases_and_storage_overlap() {
        use crate::environment_snapshot::{CapturedFilesystemSource, NativeLayerCapture};
        for fault in [
            "mixed",
            "duplicate",
            "trailing-slash",
            "store",
            "pool",
            "symlink",
        ] {
            let (temp, _old_store, store_root, capture) = fixture();
            let root = temp.path().canonicalize().unwrap();
            let private = root.join("original-private");
            fs::rename(&capture, &private).unwrap();
            fs::create_dir(&capture).unwrap();
            let pool = root.join("pool");
            let store = SnapshotStore::with_filesystem_pool(&store_root, &pool).unwrap();
            let ram = capture.parent().unwrap().join("capture.ram");
            fs::write(&ram, b"direct native RAM").unwrap();
            let mut sources = vec![CapturedFilesystemSource {
                path: "layer-001".into(),
                source: private.clone(),
                excluded: Vec::new(),
            }];
            match fault {
                "mixed" => fs::write(capture.join("unexpected-data"), b"private copy").unwrap(),
                "duplicate" => sources.push(sources[0].clone()),
                "trailing-slash" => sources[0].path = "layer-001/".into(),
                "store" => sources[0].source = store_root.clone(),
                "pool" => sources[0].source = pool.clone(),
                "symlink" => {
                    let alias = root.join("source-alias");
                    symlink(&private, &alias).unwrap();
                    sources[0].source = alias;
                }
                _ => unreachable!(),
            }
            assert!(
                store
                    .begin()
                    .unwrap()
                    .publish_native_retained(
                        &capture,
                        &fs::File::open(&ram).unwrap(),
                        b"machine",
                        compatibility(),
                        false,
                        NativeLayerCapture {
                            private_sources: &sources,
                            layers: &[],
                            delta: None
                        },
                    )
                    .is_err(),
                "{fault}"
            );
            assert_eq!(fs::read(private.join("data")).unwrap(), b"owned capture");
            assert_eq!(fs::read_dir(store_root.join("objects")).unwrap().count(), 0);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn concurrent_first_lower_seals_create_one_tree_and_cache_hits_create_no_temporary_copies() {
        use std::{
            io::Read,
            os::{fd::FromRawFd, unix::fs::PermissionsExt},
            sync::{Arc, Barrier},
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), vec![37; 1024 * 1024]).unwrap();
        fs::hard_link(source.join("data"), source.join("alias")).unwrap();
        fs::write(source.join("unvisited"), b"complete immutable source").unwrap();
        fs::set_permissions(source.join("data"), fs::Permissions::from_mode(0o440)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o500)).unwrap();
        let pool = root.join("pool");
        let collector = SnapshotStore::new(&pool).unwrap();
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        assert!(
            raw >= 0,
            "inotify init: {}",
            std::io::Error::last_os_error()
        );
        let mut events = unsafe { fs::File::from_raw_fd(raw) };
        let pending = native_path(&pool.join("pending")).unwrap();
        assert!(unsafe { libc::inotify_add_watch(raw, pending.as_ptr(), libc::IN_CREATE) } >= 0);
        let barrier = Arc::new(Barrier::new(8));
        let owners = std::thread::scope(|threads| {
            (0..8)
                .map(|index| {
                    let source = &source;
                    let pool = &pool;
                    let root = &root;
                    let barrier = barrier.clone();
                    threads.spawn(move || {
                        let job = root.join(format!("job-{index}"));
                        let store = SnapshotStore::with_filesystem_pool(&job, pool).unwrap();
                        let references = job.join("references");
                        fs::create_dir(&references).unwrap();
                        barrier.wait();
                        store
                            .retain_live_lower(source, Path::new("layer-000"), &references)
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        let inode = fs::metadata(owners[0].root().join("data")).unwrap().ino();
        for owner in &owners {
            assert_eq!(owner.id, owners[0].id);
            assert_eq!(
                fs::metadata(owner.root().join("data")).unwrap().ino(),
                inode
            );
            owner.verify_source(&source).unwrap();
        }
        assert_eq!(fs::read_dir(pool.join("filesystems")).unwrap().count(), 1);
        let mut created = 0;
        let mut buffer = vec![0; 65536];
        loop {
            let count = match events.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("inotify read: {error}"),
            };
            let mut offset = 0;
            while offset < count {
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                assert_eq!(event.mask & libc::IN_Q_OVERFLOW, 0);
                let start = offset + std::mem::size_of::<libc::inotify_event>();
                let name = &buffer[start..start + event.len as usize];
                if name.starts_with(b"filesystem-") {
                    created += 1;
                }
                offset = start + event.len as usize;
            }
        }
        assert_eq!(
            created, 1,
            "concurrent misses must create only one data tree, including temporary copies"
        );
        collector.collect_abandoned().unwrap();
        assert_eq!(
            fs::read_dir(pool.join("filesystem-build-locks"))
                .unwrap()
                .count(),
            0
        );
        assert!(owners[0].root().exists());
        let extra = SnapshotStore::with_filesystem_pool(&root.join("another-job"), &pool).unwrap();
        let references = root.join("another-references");
        fs::create_dir(&references).unwrap();
        let hit = extra
            .retain_live_lower(&source, Path::new("layer-000"), &references)
            .unwrap();
        assert_eq!(fs::metadata(hit.root().join("data")).unwrap().ino(), inode);
        assert!(
            matches!(events.read(&mut buffer), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        let distinct = extra
            .retain_live_lower(&source, Path::new("layer-001"), &references)
            .unwrap();
        assert_ne!(
            distinct.id, hit.id,
            "equal contents at different positions must remain distinct"
        );
        assert_ne!(
            fs::metadata(distinct.root().join("data")).unwrap().ino(),
            inode
        );
        assert_eq!(fs::metadata(source.join("data")).unwrap().nlink(), 2);
        assert_eq!(
            fs::metadata(source.join("data")).unwrap().mode() & 0o777,
            0o440
        );
        assert_eq!(fs::metadata(&source).unwrap().mode() & 0o777, 0o500);
        super::super::layers::remove_private_tree(&source).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn native_owner_handoff_fences_gc_and_verifies_unvisited_live_source_data() {
        use crate::environment_snapshot::{
            CapturedFilesystemLayer, NativeLayerCapture, copy_owned_tree,
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = root.join("original");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("data"), b"immutable lower").unwrap();
        fs::hard_link(original.join("data"), original.join("alias")).unwrap();
        fs::write(original.join("unvisited"), b"original unseen content").unwrap();
        let pool = root.join("pool");
        let job = root.join("job");
        let store = SnapshotStore::with_filesystem_pool(&job, &pool).unwrap();
        let capture = job.join("captures/first/rootfs");
        fs::create_dir_all(&capture).unwrap();
        copy_owned_tree(&original, &capture.join("layer-000")).unwrap();
        fs::write(
            capture.parent().unwrap().join("capture.ram"),
            b"captured RAM",
        )
        .unwrap();
        let sealed = store
            .begin()
            .unwrap()
            .publish_native_layered_retained(
                &capture,
                &fs::File::open(capture.parent().unwrap().join("capture.ram")).unwrap(),
                b"opaque native state",
                compatibility(),
                true,
                NativeLayerCapture {
                    private_sources: &[],
                    layers: &[CapturedFilesystemLayer {
                        path: "layer-000".into(),
                        source: capture.join("layer-000"),
                        id: None,
                    }],
                    delta: None,
                },
            )
            .unwrap();
        assert_eq!(sealed.lowers.len(), 1);
        let owner = sealed.lowers.into_iter().next().unwrap();
        owner.verify_source(&original).unwrap();
        let inode = fs::metadata(owner.root().join("data")).unwrap().ino();
        assert_ne!(inode, fs::metadata(original.join("data")).unwrap().ino());
        store.delete(&sealed.id).unwrap();
        drop(store);
        fs::remove_dir_all(&job).unwrap();
        let collector = SnapshotStore::new(&pool).unwrap();
        collector.collect_abandoned().unwrap();
        assert_eq!(
            fs::metadata(owner.root().join("data")).unwrap().ino(),
            inode
        );
        owner.verify_source(&original).unwrap();
        fs::write(original.join("unvisited"), b"modified unseen content").unwrap();
        assert!(
            owner.verify_source(&original).is_err(),
            "unvisited data must be authenticated before reuse"
        );
        let retained_root = owner.root().to_owned();
        drop(owner);
        collector.collect_abandoned().unwrap();
        assert!(!retained_root.exists());
        assert_eq!(fs::read(original.join("data")).unwrap(), b"immutable lower");
        assert_eq!(fs::metadata(original.join("data")).unwrap().nlink(), 2);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn native_layered_publication_retains_lower_inodes_after_complete_parent_store_deletion() {
        use crate::environment_snapshot::{
            CapturedFilesystemLayer, NativeLayerCapture, SnapshotRepository,
        };
        for compressed in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let pool = root.join("pool");
            let parent_root = root.join("parent");
            let parent = SnapshotStore::with_filesystem_pool(&parent_root, &pool).unwrap();
            let capture = parent_root.join("captures/first/rootfs");
            fs::create_dir_all(capture.join("lower/unvisited")).unwrap();
            fs::create_dir(capture.join("upper")).unwrap();
            fs::write(capture.join("lower/data"), b"native immutable data").unwrap();
            fs::hard_link(capture.join("lower/data"), capture.join("lower/alias")).unwrap();
            fs::write(
                capture.join("lower/unvisited/file"),
                b"complete native lower",
            )
            .unwrap();
            fs::write(capture.join("upper/private"), b"private branch data").unwrap();
            let original_inode = fs::metadata(capture.join("lower/data")).unwrap().ino();
            let ram_path = capture.parent().unwrap().join("capture.ram");
            fs::write(&ram_path, b"native RAM").unwrap();
            let fresh = CapturedFilesystemLayer {
                path: "lower".into(),
                source: capture.join("lower"),
                id: None,
            };
            let id = parent
                .begin()
                .unwrap()
                .publish_native_layered(
                    &capture,
                    &fs::File::open(ram_path).unwrap(),
                    b"native machine",
                    compatibility(),
                    compressed,
                    NativeLayerCapture {
                        private_sources: &[],
                        layers: &[fresh],
                        delta: None,
                    },
                )
                .unwrap();
            let published = parent.open(&id, &compatibility()).unwrap();
            assert_eq!(published.manifest().version, 5);
            assert!(published.manifest().filesystem_blocks.is_some());
            assert!(!published.directory().join("rootfs").exists());
            let references = root.join("attempt");
            fs::create_dir(&references).unwrap();
            let owner = published
                .share_readonly_layer(Path::new("lower"), &references)
                .unwrap();
            let expected_lower = published.manifest().filesystem_layers[0].filesystem.clone();
            assert_eq!(
                fs::metadata(owner.root().join("data")).unwrap().ino(),
                original_inode,
                "native adoption added a data copy"
            );
            drop(published);
            parent.delete(&id).unwrap();
            drop(parent);
            fs::remove_dir_all(&parent_root).unwrap();

            let child_root = root.join("child");
            let child = SnapshotStore::with_filesystem_pool(&child_root, &pool).unwrap();
            let capture = child_root.join("captures/second/rootfs");
            fs::create_dir_all(capture.join("upper")).unwrap();
            fs::write(capture.join("upper/private"), b"private branch data").unwrap();
            let private_inventory =
                crate::environment_snapshot::inventory(&capture.join("upper")).unwrap();
            let ram_path = capture.parent().unwrap().join("capture.ram");
            fs::write(&ram_path, b"native RAM").unwrap();
            let retained = CapturedFilesystemLayer {
                path: "lower".into(),
                source: owner.root().to_owned(),
                id: Some(owner.id.clone()),
            };
            let id = child
                .begin()
                .unwrap()
                .publish_native_layered(
                    &capture,
                    &fs::File::open(ram_path).unwrap(),
                    b"native child machine",
                    compatibility(),
                    compressed,
                    NativeLayerCapture {
                        private_sources: &[],
                        layers: &[retained],
                        delta: None,
                    },
                )
                .unwrap();
            drop(owner);
            fs::remove_dir_all(references).unwrap();
            child.collect_abandoned().unwrap();
            let published = child.open(&id, &compatibility()).unwrap();
            let lower = published.owned_layer_path(Path::new("lower")).unwrap();
            assert_eq!(
                fs::metadata(lower.join("data")).unwrap().ino(),
                original_inode
            );
            assert_eq!(fs::metadata(lower.join("data")).unwrap().nlink(), 2);
            assert!(published.manifest().filesystem_blocks.is_some());
            assert!(!published.directory().join("rootfs").exists());
            let writable = root.join("restored-private");
            published
                .materialize_layer(Path::new("upper"), &writable)
                .unwrap();
            assert_eq!(
                crate::environment_snapshot::inventory(&writable).unwrap(),
                private_inventory
            );
            assert_eq!(fs::metadata(writable.join("private")).unwrap().nlink(), 1);
            assert!(
                !child_root
                    .join("objects")
                    .join(&id)
                    .join("rootfs/lower")
                    .exists()
            );
            assert_eq!(inventory(&lower).unwrap(), expected_lower);
            let remote = tempfile::tempdir().unwrap();
            let receipt = SnapshotRepository::filesystem(remote.path(), false)
                .unwrap()
                .publish(&published)
                .unwrap();
            drop(published);
            temp.close().unwrap();
            let receiver = tempfile::tempdir().unwrap();
            let receiver_store = SnapshotStore::new(&receiver.path().join("snapshot")).unwrap();
            SnapshotRepository::filesystem(remote.path(), true)
                .unwrap()
                .import(&receiver_store, &receipt, &compatibility())
                .unwrap();
            let imported = receiver_store.open(&id, &compatibility()).unwrap();
            assert_eq!(
                fs::read(
                    imported
                        .owned_layer_path(Path::new("lower"))
                        .unwrap()
                        .join("unvisited/file")
                )
                .unwrap(),
                b"complete native lower"
            );
            assert_eq!(imported.machine_bytes().unwrap(), b"native child machine");
            let mut ram = Vec::new();
            imported.ram_file().unwrap().read_to_end(&mut ram).unwrap();
            assert_eq!(ram, b"native RAM");
        }
    }

    fn pinned_base() -> (
        tempfile::TempDir,
        std::sync::Arc<super::super::PinnedRamBlocks>,
        Vec<u8>,
    ) {
        let (temp, store, _root, source) = fixture();
        let block = crate::ram_backing::BLOCK_BYTES;
        let bytes: Vec<_> = (0..6 * block + 317)
            .map(|index| (17 + index / block) as u8)
            .collect();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(&bytes).unwrap();
        let id = pending
            .publish_compressed(&source, b"base-machine", compatibility())
            .unwrap();
        let published = store.open(&id, &compatibility()).unwrap();
        let reader = published.ram_reader().unwrap();
        let base = reader.compressed_base().unwrap();
        drop(reader);
        drop(published);
        store.delete(&id).unwrap();
        store.collect_abandoned().unwrap();
        assert!(!temp.path().join("store/objects").join(id).exists());
        (temp, base, bytes)
    }

    fn delta_capture_source(store_root: &Path) -> (PathBuf, fs::File) {
        let capture = store_root.join("captures/capture-delta");
        fs::create_dir_all(capture.join("rootfs")).unwrap();
        fs::write(capture.join("rootfs/data"), b"complete private forest").unwrap();
        let ram = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(capture.join("capture.ram"))
            .unwrap();
        (capture.join("rootfs"), ram)
    }

    #[test]
    fn incremental_native_ram_survives_parent_deletion_and_owns_inherited_frames() {
        use crate::environment_snapshot::RamDelta;
        use crate::ram_backing::BLOCK_BYTES;
        use std::os::unix::fs::FileExt;
        // /tmp and the workspace exercise both hard links and EXDEV where the
        // host puts them on different volumes. The assertions follow actual devs.
        for (workspace, changed) in [(false, false), (false, true), (true, true)] {
            let (parent, base, mut expected) = pinned_base();
            let child = if workspace {
                tempfile::Builder::new()
                    .prefix(".pvisor-delta-test-")
                    .tempdir_in(env!("CARGO_MANIFEST_DIR"))
                    .unwrap()
            } else {
                tempfile::tempdir().unwrap()
            };
            let root = child.path().canonicalize().unwrap().join("store");
            let store = SnapshotStore::new(&root).unwrap();
            let (source, ram) = delta_capture_source(&root);
            ram.set_len(expected.len() as u64).unwrap();
            let dirty: &[u64] = if changed { &[1, 3, 6] } else { &[] };
            if changed {
                expected[BLOCK_BYTES..2 * BLOCK_BYTES].fill(0);
                expected[3 * BLOCK_BYTES..4 * BLOCK_BYTES].fill(91);
                expected[6 * BLOCK_BYTES..].fill(73);
                for &index in dirty {
                    let start = index as usize * BLOCK_BYTES;
                    let end = (start + BLOCK_BYTES).min(expected.len());
                    ram.write_all_at(&expected[start..end], start as u64)
                        .unwrap();
                }
            }
            let delta = RamDelta {
                version: 1,
                length: expected.len() as u64,
                block_bytes: BLOCK_BYTES as u32,
                base_sha256: &base.sha256,
                changed_blocks: dirty,
            };
            let id = store
                .begin()
                .unwrap()
                .publish_native_delta(
                    &source,
                    &ram,
                    b"delta-machine",
                    compatibility(),
                    &base,
                    &delta,
                )
                .unwrap();
            assert!(!source.exists());
            let published = store.open(&id, &compatibility()).unwrap();
            assert_eq!(published.manifest().version, 2);
            let inventory = published.manifest().ram_blocks.as_ref().unwrap();
            for (index, frame) in inventory.blocks.iter().enumerate() {
                if !dirty.contains(&(index as u64)) {
                    assert_eq!(frame.id, base.blocks.blocks[index].id);
                    let inherited = fs::metadata(
                        base.references
                            .directory()
                            .join("ram-blocks")
                            .join(&frame.id),
                    )
                    .unwrap();
                    let own = fs::metadata(
                        root.join("objects")
                            .join(&id)
                            .join("ram-blocks")
                            .join(&frame.id),
                    )
                    .unwrap();
                    if inherited.dev() == own.dev() {
                        assert_eq!(inherited.ino(), own.ino(), "clean frame copied");
                    } else {
                        assert_ne!(inherited.dev(), own.dev());
                    }
                }
            }
            let mut actual = Vec::new();
            published
                .ram_file()
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, expected, "zeroing and partial tails must survive");
            assert_eq!(
                fs::read(root.join("objects").join(&id).join("rootfs/data")).unwrap(),
                b"complete private forest"
            );
            drop(published);
            // A stale native writer and physical removal of ALL parent-store
            // data cannot change or make the self-contained child unavailable.
            ram.write_all_at(b"late native writer", 0).unwrap();
            drop(base);
            parent.close().unwrap();
            let published = store.open(&id, &compatibility()).unwrap();
            let mut actual = Vec::new();
            published
                .ram_file()
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, expected);
            #[cfg(target_os = "linux")]
            let remote = {
                let directory = tempfile::tempdir().unwrap();
                let repository =
                    super::super::SnapshotRepository::filesystem(directory.path(), false).unwrap();
                let receipt = repository.publish(&published).unwrap();
                (directory, receipt)
            };
            drop(published);
            store.delete(&id).unwrap();
            store.collect_abandoned().unwrap();
            assert_eq!(fs::read_dir(root.join("content")).unwrap().count(), 0);
            #[cfg(target_os = "linux")]
            {
                drop(ram);
                child.close().unwrap();
                let repository =
                    super::super::SnapshotRepository::filesystem(remote.0.path(), true).unwrap();
                let replica = tempfile::tempdir().unwrap();
                let imported = SnapshotStore::new(&replica.path().join("store")).unwrap();
                repository
                    .import(&imported, &remote.1, &compatibility())
                    .unwrap();
                let published = imported.open(&id, &compatibility()).unwrap();
                let mut actual = Vec::new();
                published
                    .ram_file()
                    .unwrap()
                    .read_to_end(&mut actual)
                    .unwrap();
                assert_eq!(
                    actual, expected,
                    "transport must retain the self-contained delta child"
                );
                assert_eq!(published.machine_bytes().unwrap(), b"delta-machine");
            }
        }
    }

    #[test]
    fn incremental_native_ram_rejects_unbound_inventory_and_corrupt_inherited_content() {
        use crate::environment_snapshot::RamDelta;
        use crate::ram_backing::BLOCK_BYTES;
        use std::os::unix::fs::FileExt;
        for fault in [
            "version",
            "hash",
            "length",
            "block-size",
            "duplicate",
            "ordering",
            "range",
            "undeclared",
            "missing",
            "corrupt",
            "collision",
        ] {
            let (_parent, base, expected) = pinned_base();
            let child = tempfile::tempdir().unwrap();
            let root = child.path().canonicalize().unwrap().join("store");
            let store = SnapshotStore::new(&root).unwrap();
            let (source, ram) = delta_capture_source(&root);
            ram.set_len(expected.len() as u64).unwrap();
            let mut delta = RamDelta {
                version: 1,
                length: expected.len() as u64,
                block_bytes: BLOCK_BYTES as u32,
                base_sha256: &base.sha256,
                changed_blocks: &[],
            };
            let inherited = base
                .references
                .directory()
                .join("ram-blocks")
                .join(&base.blocks.blocks[0].id);
            match fault {
                "version" => delta.version = 2,
                "hash" => delta.base_sha256 = "bad",
                "length" => delta.length -= 1,
                "block-size" => delta.block_bytes *= 2,
                "duplicate" => delta.changed_blocks = &[1, 1],
                "ordering" => delta.changed_blocks = &[2, 1],
                "range" => delta.changed_blocks = &[7],
                "undeclared" => ram.write_all_at(b"not declared dirty", 0).unwrap(),
                "missing" => fs::remove_file(inherited).unwrap(),
                "corrupt" => fs::write(inherited, b"corrupt pinned frame").unwrap(),
                "collision" => fs::write(
                    root.join("content").join(&base.blocks.blocks[0].id),
                    b"corrupt destination CAS",
                )
                .unwrap(),
                _ => unreachable!(),
            }
            assert!(
                store
                    .begin()
                    .unwrap()
                    .publish_native_delta(
                        &source,
                        &ram,
                        b"delta-machine",
                        compatibility(),
                        &base,
                        &delta,
                    )
                    .is_err(),
                "{fault}"
            );
            assert_eq!(
                fs::read_dir(root.join("objects")).unwrap().count(),
                0,
                "{fault}"
            );
            assert_eq!(
                fs::read_dir(root.join("pending")).unwrap().count(),
                0,
                "{fault}"
            );
        }
    }

    #[test]
    fn native_publication_transfers_inodes_preserves_complete_seal_and_detaches_ram() {
        for compressed in [false, true] {
            let (_temp, store, store_root, source) = fixture();
            let expected = inventory(&source).unwrap();
            let identities: Vec<_> = expected
                .entries
                .iter()
                .map(|entry| {
                    let metadata =
                        fs::symlink_metadata(source.join(std::ffi::OsStr::from_bytes(&entry.path)))
                            .unwrap();
                    (metadata.dev(), metadata.ino())
                })
                .collect();
            let pending = store.begin().unwrap();
            let mut writer = fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(source.parent().unwrap().join("capture.ram"))
                .unwrap();
            writer.write_all(b"saved RAM").unwrap();
            let mut ram = fs::File::open(source.parent().unwrap().join("capture.ram")).unwrap();
            ram.seek(SeekFrom::Start(3)).unwrap(); // The sealer resets its stream.
            let id = pending
                .publish_native_capture(
                    &source,
                    &ram,
                    b"opaque machine",
                    compatibility(),
                    compressed,
                )
                .unwrap();
            assert!(
                !source.exists(),
                "native capture ownership must be consumed"
            );
            assert!(source.parent().unwrap().is_dir());
            let sealed = store_root.join("objects").join(&id).join("rootfs");
            assert!(!sealed.parent().unwrap().join("capture.ram").exists());
            verify_tree(&sealed, &expected).unwrap();
            for (entry, identity) in expected.entries.iter().zip(identities) {
                let metadata =
                    fs::symlink_metadata(sealed.join(std::ffi::OsStr::from_bytes(&entry.path)))
                        .unwrap();
                assert_eq!(
                    (metadata.dev(), metadata.ino()),
                    identity,
                    "second data copy: {:?}",
                    entry.path
                );
            }
            let published = store.open(&id, &compatibility()).unwrap();
            assert_eq!(
                published.manifest().source_root,
                source.as_os_str().as_bytes()
            );
            assert_eq!(published.machine_bytes().unwrap(), b"opaque machine");
            writer.seek(SeekFrom::Start(0)).unwrap();
            writer.write_all(b"changed!!").unwrap();
            let mut ram = Vec::new();
            published.ram_file().unwrap().read_to_end(&mut ram).unwrap();
            assert_eq!(ram, b"saved RAM");
            let private = store_root.parent().unwrap().join("private");
            published.materialize(&private).unwrap();
            assert_ne!(
                fs::metadata(private.join("data")).unwrap().ino(),
                fs::metadata(sealed.join("data")).unwrap().ino(),
            );
            fs::write(private.join("data"), b"private writes").unwrap();
            assert_eq!(fs::read(sealed.join("data")).unwrap(), b"owned capture");
            drop(published);
            fs::write(sealed.join("unvisited/private"), b"corruption").unwrap();
            assert!(store.open(&id, &compatibility()).is_err());
        }
    }

    #[test]
    fn native_transfer_rejects_borrowed_symlinked_and_externally_linked_sources() {
        for fault in [
            "borrowed",
            "symlink",
            "ancestor",
            "hardlink",
            "spelling",
            "destination",
        ] {
            let (_temp, _store, store_root, source) = fixture();
            let original = source.clone();
            let destination_parent = store_root.join("pending/test");
            fs::create_dir(&destination_parent).unwrap();
            let destination = destination_parent.join("rootfs");
            let source = match fault {
                "borrowed" => {
                    let borrowed = store_root.parent().unwrap().join("borrowed");
                    super::super::copy_owned_tree(&source, &borrowed).unwrap();
                    borrowed
                }
                "symlink" => {
                    let renamed = source.with_file_name("owned");
                    fs::rename(&source, &renamed).unwrap();
                    symlink(&renamed, &source).unwrap();
                    source
                }
                "ancestor" => {
                    let linked = store_root.join("captures/linked");
                    symlink(source.parent().unwrap(), &linked).unwrap();
                    linked.join("rootfs")
                }
                "hardlink" => {
                    fs::hard_link(
                        source.join("data"),
                        store_root.parent().unwrap().join("external"),
                    )
                    .unwrap();
                    source
                }
                "spelling" => source.join("../rootfs"),
                "destination" => {
                    fs::create_dir(&destination).unwrap();
                    fs::write(destination.join("existing"), b"untouched").unwrap();
                    source
                }
                _ => unreachable!(),
            };
            assert!(
                take_native_tree(&store_root, &source, &destination).is_err(),
                "{fault}"
            );
            assert!(original.exists(), "{fault} consumed the capture");
            if fault == "destination" {
                assert_eq!(
                    fs::read(destination.join("existing")).unwrap(),
                    b"untouched"
                );
            } else {
                assert!(!destination.exists(), "{fault} exposed a destination");
            }
        }
    }

    #[test]
    fn failed_native_seal_collects_consumed_pending_tree_without_exposing_an_object() {
        let (_temp, store, store_root, source) = fixture();
        let pending = store.begin().unwrap();
        // Fail after tree transfer: the native RAM file is empty.
        let ram = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(source.parent().unwrap().join("capture.ram"))
            .unwrap();
        assert!(
            pending
                .publish_native_capture(&source, &ram, b"machine", compatibility(), false)
                .is_err()
        );
        assert!(!source.exists());
        assert_eq!(fs::read_dir(store_root.join("objects")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(store_root.join("pending")).unwrap().count(), 0);
        assert_eq!(store.collect_abandoned().unwrap(), 0);
    }

    #[test]
    fn native_ram_binding_rejects_foreign_replaced_symlinked_and_pending_writers() {
        for fault in ["foreign", "replaced", "symlink", "directory", "pending"] {
            let (_temp, store, store_root, source) = fixture();
            let path = source.parent().unwrap().join("capture.ram");
            fs::write(&path, b"RAM").unwrap();
            let mut ram = fs::File::open(&path).unwrap();
            let pending = store.begin().unwrap();
            match fault {
                "foreign" => {
                    let foreign = store_root.join("foreign");
                    fs::write(&foreign, b"RAM").unwrap();
                    ram = fs::File::open(foreign).unwrap();
                }
                "replaced" => {
                    fs::rename(&path, path.with_extension("old")).unwrap();
                    fs::write(&path, b"RAM").unwrap();
                }
                "symlink" => {
                    let old = path.with_extension("old");
                    fs::rename(&path, &old).unwrap();
                    symlink(&old, &path).unwrap();
                }
                "directory" => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                    ram = fs::File::open(&path).unwrap();
                }
                "pending" => {
                    pending
                        .create_ram()
                        .unwrap()
                        .write_all(b"unsealed writer")
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                pending
                    .publish_native_capture(&source, &ram, b"machine", compatibility(), false)
                    .is_err(),
                "{fault}"
            );
            assert!(source.is_dir(), "{fault} consumed the source");
            assert_eq!(fs::read_dir(store_root.join("objects")).unwrap().count(), 0);
        }
    }

    #[test]
    fn borrowed_publication_still_copies_and_never_consumes_a_native_shaped_path() {
        let (_temp, store, store_root, source) = fixture();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(b"RAM").unwrap();
        let id = pending
            .publish(&source, b"machine", compatibility())
            .unwrap();
        let sealed = store_root.join("objects").join(&id).join("rootfs");
        assert!(source.is_dir());
        assert_ne!(
            fs::metadata(source.join("data")).unwrap().ino(),
            fs::metadata(sealed.join("data")).unwrap().ino()
        );
        fs::write(source.join("data"), b"source can change later").unwrap();
        assert_eq!(fs::read(sealed.join("data")).unwrap(), b"owned capture");
        store.open(&id, &compatibility()).unwrap();
    }
}
