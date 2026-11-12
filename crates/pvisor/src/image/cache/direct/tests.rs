use super::*;
use crate::image::cache::{MAX_READ, backend::tests::fixture};
use pvisor_overlay_core::{OverlayCore, backend, service::FilesystemService};
use std::{fs::File, sync::atomic::Ordering};

#[test]
fn runner_starts_bridge_before_constructing_any_client_transport() {
    let source = include_str!("../direct.rs");
    let handoff = source
        .split_once("let binding: Binding = serde_json::from_reader(file)?;")
        .unwrap()
        .1
        .split_once("owners.read_write.push(parent.to_owned());")
        .unwrap()
        .0;
    let bridge = handoff.find("NetworkAccess::start(").unwrap();
    let client = handoff.find("CacheClient::from_binding(").unwrap();
    assert!(
        bridge < client,
        "remote transports must not start workers before bridge setup"
    );
    assert!(handoff[..bridge].contains("client_binding.resolved_source()?"));
    assert!(!handoff[..bridge].contains("CacheClient::"));
    let resolver = include_str!("../config.rs")
        .split_once("impl ResolvedSource {")
        .unwrap()
        .1
        .split_once("impl CacheConfig {")
        .unwrap()
        .0;
    for forbidden in [
        "Storage::",
        "Http::new(",
        "PoolStorage::",
        "thread::",
        "Runtime::",
    ] {
        assert!(
            !resolver.contains(forbidden),
            "capability parsing must be thread/I/O free: {forbidden}"
        );
    }
}

fn image(client: CacheClient, digest: String, base: &Path) -> DirectImage {
    let source = RemoteFs::new(
        client,
        digest,
        base.join("blocks"),
        Some(base.join("metadata")),
        false,
    )
    .unwrap();
    DirectImage::new(source, base).unwrap()
}

fn branch_image(base: &Path) -> DirectImage {
    use crate::image::oci::{ImageStore, PreparedImage};
    let source = ImageStore::new(Some(base.join("source"))).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&rootfs).unwrap();
    fs::write(rootfs.join("visited"), b"shared image").unwrap();
    fs::hard_link(rootfs.join("visited"), rootfs.join("alias")).unwrap();
    fs::create_dir(rootfs.join("nested")).unwrap();
    fs::write(rootfs.join("nested/unvisited"), b"lazy after source exit").unwrap();
    let prepared = PreparedImage {
        rootfs,
        digest: digest.clone(),
        env: Default::default(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let pool = base.join("pool");
    let handle =
        super::super::pool::publish_local_image(&pool, &source, &prepared, "branch:test", "amd64")
            .unwrap();
    let scratch = base.join("scratch");
    fs::create_dir(&scratch).unwrap();
    super::super::pool::open_local_image(&pool, None, &[], &handle, "amd64", &digest, &scratch)
        .unwrap()
}

#[test]
fn retained_image_keeps_lazy_revision_and_inode_after_source_exit() {
    let temp = tempfile::tempdir().unwrap();
    let mut image = branch_image(temp.path());
    let root = image.root().to_owned();
    backend::materialize_file(&root.join("visited")).unwrap();
    let before = fs::metadata(root.join("visited")).unwrap();
    assert!(backend::snapshot_binding(&root).unwrap().is_none());
    let retained = image.retain_for_branch().unwrap();
    let binding = backend::snapshot_binding(&root).unwrap().unwrap();
    assert!(
        !root.join("nested").exists(),
        "retaining must not traverse unread image content"
    );
    drop(image);
    assert!(backend::attributes(&root).unwrap().is_none());
    let one = retained.open().unwrap();
    assert_eq!(
        backend::snapshot_binding(&root).unwrap().as_ref(),
        Some(&binding)
    );
    assert_eq!(
        backend::read_at(&root.join("nested/unvisited"), 0, 80)
            .unwrap()
            .unwrap(),
        b"lazy after source exit"
    );
    backend::materialize_file(&root.join("alias")).unwrap();
    let alias = fs::metadata(root.join("alias")).unwrap();
    assert_eq!((alias.dev(), alias.ino()), (before.dev(), before.ino()));
    let before = fs::metadata(root.join("visited")).unwrap();
    drop(one);
    // Reopen again after all live attachments disappeared. The baseline has
    // durable directory lifetime without depending on the source VM process.
    let two = retained.open().unwrap();
    backend::materialize_file(&root.join("visited")).unwrap();
    let after = fs::metadata(root.join("visited")).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
    );
    drop(two);
    assert!(root.is_dir());
}

#[test]
fn retained_image_rejects_replaced_descriptor_and_directory() {
    let temp = tempfile::tempdir().unwrap();
    let mut image = branch_image(temp.path());
    let retained = image.retain_for_branch().unwrap();
    let root = image.root().to_owned();
    drop(image);
    let descriptor = root.parent().unwrap().join(DESCRIPTOR);
    let original = fs::read(&descriptor).unwrap();
    fs::write(&descriptor, b"{}").unwrap();
    assert!(retained.open().is_err());
    fs::write(&descriptor, &original).unwrap();
    let old = root.with_file_name("old-lower");
    fs::rename(&root, &old).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(retained.open().is_err());
    fs::remove_dir(&root).unwrap();
    std::os::unix::fs::symlink(&old, &root).unwrap();
    assert!(retained.open().is_err());
}

#[test]
fn ordinary_image_still_removes_projection_and_legacy_cannot_be_retained() {
    let temp = tempfile::tempdir().unwrap();
    let image = branch_image(temp.path());
    let root = image.root().to_owned();
    drop(image);
    assert!(!root.exists());
    let (temp, _server, client, digest) = fixture();
    let mut legacy = self::image(client, digest, temp.path());
    let root = legacy.root().to_owned();
    assert!(legacy.retain_for_branch().is_err());
    drop(legacy);
    assert!(!root.exists());
}

#[test]
fn private_experiments_are_strict_without_changing_process_environment() {
    assert!(!experiment_setting("test", None).unwrap());
    assert!(!experiment_setting("test", Some(OsStr::new("0"))).unwrap());
    assert!(experiment_setting("test", Some(OsStr::new("1"))).unwrap());
    for invalid in ["", "true", "2", " 1"] {
        assert!(experiment_setting("test", Some(OsStr::new(invalid))).is_err());
    }
    assert!(experiment_setting("test", Some(OsStr::from_bytes(&[0xff]))).is_err());
}

#[test]
fn resolved_node_reuse_requires_a_completed_projection_and_keeps_traversal_errors() {
    let (temp, _server, client, digest) = fixture();
    let source = RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
    let root = temp.path().join("projection");
    let objects = temp.path().join("objects");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&objects).unwrap();
    let projection = Projection {
        root,
        objects,
        reader: source.reader.clone(),
        source: Mutex::new(source),
        projected: Mutex::default(),
        complete: Mutex::default(),
        reuse_resolved_nodes: true,
        stream_reads: false,
        direct_copy: false,
        snapshot_owner: Arc::new(Mutex::new(None)),
    };
    let before = projection.project(Path::new("large")).unwrap();
    assert!(
        projection
            .projected
            .lock()
            .unwrap()
            .contains(Path::new("large"))
    );
    let after = projection.node(Path::new("large")).unwrap();
    assert_eq!(
        (before.object_id, before.attr.size),
        (after.object_id, after.attr.size)
    );
    assert!(projection.node(Path::new("alias/child")).is_err());
    assert!(projection.node(Path::new("../large")).is_err());
    assert!(projection.node(Path::new("missing")).is_err());
    assert!(
        !projection
            .projected
            .lock()
            .unwrap()
            .contains(Path::new("missing"))
    );
    assert!(
        !projection
            .projected
            .lock()
            .unwrap()
            .contains(Path::new("alias/child"))
    );
}

#[test]
fn direct_metadata_and_reads_are_lazy_without_a_host_mount() {
    let (temp, server, client, digest) = fixture();
    let image = image(client, digest, temp.path());
    let root = image.root();
    let core = OverlayCore::new(vec![root.to_owned()], temp.path().join("upper"), None).unwrap();
    let service = FilesystemService::new(core);
    let path = Path::new("large");
    let backing = service.prepare_open(path, libc::O_RDONLY).unwrap();
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    let metadata = backend::attributes(&backing).unwrap().unwrap();
    assert_eq!(metadata.size, 3 * u64::from(MAX_READ));
    assert_eq!(
        metadata.nlink, 1,
        "the private object link is not a guest alias"
    );
    let file = File::open(&backing).unwrap();
    let mut bytes = [0; 16];
    assert_eq!(
        service
            .read_at(&backing, &file, &mut bytes, u64::from(MAX_READ) - 8)
            .unwrap(),
        16
    );
    assert_eq!(bytes, [42; 16]);
    assert_eq!(
        server.reads.load(Ordering::Relaxed),
        2,
        "only the two intersected blocks"
    );
    service.read_at(&backing, &file, &mut bytes, 0).unwrap();
    assert_eq!(
        server.reads.load(Ordering::Relaxed),
        2,
        "verified blocks are reused"
    );
    assert_eq!(
        backend::read_link(root.join("alias")).unwrap(),
        Path::new("large")
    );
    #[cfg(target_os = "linux")]
    {
        let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap();
        assert!(
            !mounts
                .lines()
                .any(|line| line.split_whitespace().nth(4) == root.to_str())
        );
    }
    assert_eq!(
        backend::symlink_metadata(root.join("absent"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(
        backend::symlink_metadata(root.join("alias/child")).is_err(),
        "never traverse a remote symlink as a directory"
    );
    assert!(
        backend::read_at(&root.join("../backend.json"), 0, 16).is_err(),
        "guest paths cannot reach the private descriptor"
    );
}

#[test]
fn streamed_reads_keep_block_boundaries_eof_and_sink_failure() {
    let (temp, _server, client, digest) = fixture();
    let mut source =
        RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
    let node = source.child(1, OsStr::new("large")).unwrap();
    let mut sink = Vec::new();
    assert_eq!(
        source
            .reader
            .read_into(&node, u64::from(MAX_READ) - 8, 16, &mut sink)
            .unwrap(),
        16
    );
    assert_eq!(sink, vec![42; 16]);
    sink.clear();
    assert_eq!(
        source
            .reader
            .read_into(&node, node.attr.size - 3, 99, &mut sink)
            .unwrap(),
        3
    );
    assert_eq!(sink, vec![42; 3]);
    assert_eq!(
        source
            .reader
            .read_into(&node, node.attr.size + 1, 99, &mut sink)
            .unwrap(),
        0
    );
    struct Broken;
    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = source
        .reader
        .read_into(&node, 0, 1, &mut Broken)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
        Some(libc::ENOSPC)
    );
}

#[test]
fn copy_up_materializes_bytes_and_preserves_remote_hard_links() {
    let (temp, server, client, digest) = fixture();
    let original = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    fs::hard_link(original.join("large"), original.join("linked")).unwrap();
    fs::set_permissions(original.join("large"), fs::Permissions::from_mode(0o755)).unwrap();
    let image = image(client, digest, temp.path());
    let core = OverlayCore::new(
        vec![image.root().to_owned()],
        temp.path().join("upper"),
        None,
    )
    .unwrap();
    assert_eq!(core.list_names(Path::new("")).unwrap().len(), 3);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    let first = core.copy_up(Path::new("large")).unwrap();
    let second = core.copy_up(Path::new("linked")).unwrap();
    assert_eq!(server.reads.load(Ordering::Relaxed), 3);
    assert_eq!(
        fs::metadata(&first).unwrap().ino(),
        fs::metadata(&second).unwrap().ino()
    );
    assert_eq!(fs::metadata(&first).unwrap().mode() & 0o7777, 0o755);
    assert_eq!(fs::read(first).unwrap(), vec![42; 3 * MAX_READ as usize]);
    fs::write(&second, b"changed upper").unwrap();
    assert_eq!(
        backend::read_at(&image.root().join("large"), 0, 16)
            .unwrap()
            .unwrap(),
        [42; 16]
    );
}

#[test]
fn direct_copy_fills_upper_without_materializing_lower_in_both_copy_paths() {
    for prepared in [false, true] {
        let (temp, server, client, digest) = fixture();
        let source =
            RemoteFs::new(client, digest, temp.path().join("blocks"), None, false).unwrap();
        let root = temp.path().join("projection");
        let objects = temp.path().join("objects");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&objects).unwrap();
        let projection = Arc::new(Projection {
            root: root.clone(),
            objects: objects.clone(),
            reader: source.reader.clone(),
            source: Mutex::new(source),
            projected: Mutex::default(),
            complete: Mutex::default(),
            reuse_resolved_nodes: false,
            stream_reads: false,
            direct_copy: true,
            snapshot_owner: Arc::new(Mutex::new(None)),
        });
        projection.project(Path::new("large")).unwrap();
        let _owner = backend::BackendAttachment::new_stable_namespace(&root, projection).unwrap();
        let core = OverlayCore::new(vec![root.clone()], temp.path().join("upper"), None).unwrap();
        let copies = if prepared {
            core.prepare_copy_ups(&["large".into()], false).unwrap()
        } else {
            vec![]
        };
        let _installed = core.use_prepared_copy_ups(copies).unwrap();
        let upper = core.copy_up(Path::new("large")).unwrap();
        assert_eq!(fs::read(&upper).unwrap(), vec![42; 3 * MAX_READ as usize]);
        assert_eq!(server.reads.load(Ordering::Relaxed), 3);
        assert!(
            fs::read(root.join("large"))
                .unwrap()
                .iter()
                .all(|b| *b == 0)
        );
        assert!(
            objects.read_dir().unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .as_bytes()
                .ends_with(b".complete"))
        );
        assert_eq!(
            backend::read_at(&root.join("large"), 0, 16)
                .unwrap()
                .unwrap(),
            [42; 16]
        );
    }
}

#[test]
fn owned_snapshot_export_includes_unvisited_objects_and_survives_source_removal() {
    let (temp, server, client, digest) = fixture();
    let original = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    fs::create_dir(original.join("nested")).unwrap();
    fs::write(original.join("nested/unvisited"), b"snapshot payload").unwrap();
    let image = image(client, digest, temp.path());
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    let saved = temp.path().join("saved");
    let fingerprint =
        pvisor_overlay_core::fingerprint_at(image.root(), Path::new("large")).unwrap();
    let directory_fingerprint =
        pvisor_overlay_core::fingerprint_at(image.root(), Path::new("")).unwrap();
    crate::environment_snapshot::copy_owned_tree(image.root(), &saved).unwrap();
    assert_eq!(server.reads.load(Ordering::Relaxed), 4);
    drop(image);
    drop(server);
    assert_eq!(
        pvisor_overlay_core::fingerprint_at(&saved, Path::new("large")).unwrap(),
        fingerprint
    );
    assert_eq!(
        pvisor_overlay_core::fingerprint_at(&saved, Path::new("")).unwrap(),
        directory_fingerprint
    );
    assert_eq!(
        fs::read(saved.join("large")).unwrap(),
        vec![42; 3 * MAX_READ as usize]
    );
    assert_eq!(
        fs::read(saved.join("nested/unvisited")).unwrap(),
        b"snapshot payload"
    );
    assert_eq!(
        fs::read_link(saved.join("alias")).unwrap(),
        Path::new("large")
    );
}

#[test]
fn runner_handoff_reopens_the_backend_and_retires_with_its_owner() {
    let (temp, server, client, digest) = fixture();
    let DirectImage {
        attachment,
        _directory,
        ..
    } = image(client, digest, temp.path());
    let root = attachment.root().to_owned();
    backend::materialize_file(&root.join("large")).unwrap();
    let before = fs::metadata(root.join("large")).unwrap();
    drop(attachment);
    assert!(backend::attributes(&root).unwrap().is_none());
    let roots = [root.clone()];
    let owner = attach_runner_lowers(roots.iter()).unwrap();
    assert_eq!(
        backend::read_at(&root.join("large"), 0, 16)
            .unwrap()
            .unwrap(),
        [42; 16]
    );
    assert_eq!(server.reads.load(Ordering::Relaxed), 3);
    backend::materialize_file(&root.join("large")).unwrap();
    let after = fs::metadata(root.join("large")).unwrap();
    assert_eq!(
        (before.ino(), before.ctime(), before.ctime_nsec()),
        (after.ino(), after.ctime(), after.ctime_nsec()),
        "a second runner must not rewrite complete backing"
    );
    assert!(
        !owner.read_write.contains(&temp.path().to_owned()),
        "do not grant the Unix socket's entire parent"
    );
    drop(owner);
    assert!(backend::attributes(&root).unwrap().is_none());
    drop(_directory);
    assert!(!root.exists());
}

#[test]
fn metadata_probes_do_not_project_unvisited_siblings_or_materialize_content() {
    let (temp, server, client, digest) = fixture();
    let original = temp
        .path()
        .join("store/rootfs-v3/sha256")
        .join(&digest[7..]);
    fs::hard_link(original.join("large"), original.join("linked")).unwrap();
    fs::write(original.join("unvisited"), b"stay lazy").unwrap();
    let image = image(client, digest, temp.path());
    let root = image.root();
    backend::symlink_metadata(root.join("large")).unwrap();
    backend::symlink_metadata(root.join("linked")).unwrap();
    let large = backend::attributes(&root.join("large")).unwrap().unwrap();
    let linked = backend::attributes(&root.join("linked")).unwrap().unwrap();
    assert_eq!(large.ino, linked.ino);
    assert_eq!(large.size, linked.size);
    assert_eq!(large.nlink, 2);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    assert!(
        !root.join("unvisited").exists(),
        "metadata prefetch must not project siblings"
    );

    assert!(backend::symlink_metadata(root.join("alias/child")).is_err());
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn hot_metadata_does_not_rewrite_projection_identity() {
    let (temp, _server, client, digest) = fixture();
    let image = image(client, digest, temp.path());
    let path = image.root().join("large");
    let before = backend::symlink_metadata(&path).unwrap();
    for _ in 0..8 {
        backend::symlink_metadata(&path).unwrap();
    }
    let after = fs::symlink_metadata(path).unwrap();
    assert_eq!(
        (before.ino(), before.ctime(), before.ctime_nsec()),
        (after.ino(), after.ctime(), after.ctime_nsec())
    );
}

#[test]
fn index_page_capability_survives_direct_handoff_without_reopening_revision() {
    let (temp, server, client, handle, _pages) =
        crate::image::cache::backend::tests::portable_fixture(false);
    let source = RemoteFs::new(
        client,
        handle.clone(),
        temp.path().join("blocks"),
        Some(temp.path().join("metadata")),
        true,
    )
    .unwrap();
    let DirectImage {
        attachment,
        _directory,
        ..
    } = DirectImage::new(source, temp.path()).unwrap();
    let root = attachment.root().to_owned();
    let descriptor = root.parent().unwrap().join(DESCRIPTOR);
    let binding: Binding = serde_json::from_slice(&fs::read(&descriptor).unwrap()).unwrap();
    assert!(binding.metadata_pages);
    assert_eq!(binding.handle, handle);
    drop(attachment);
    // Reconstruction must use the pinned capability; the fixture supports no OCI preparation.
    let roots = [root.clone()];
    let owner = attach_runner_lowers(roots.iter()).unwrap();
    backend::symlink_metadata(root.join("large")).unwrap();
    backend::symlink_metadata(root.join("linked")).unwrap();
    let large = backend::attributes(&root.join("large")).unwrap().unwrap();
    let linked = backend::attributes(&root.join("linked")).unwrap().unwrap();
    assert_eq!(large.ino, linked.ino);
    assert_eq!(large.perm, 0o640);
    assert_eq!(large.nlink, 2);
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    drop(owner);
    // Pre-capability runner descriptors remain readable and select legacy RPC.
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(descriptor).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("metadata_pages");
    let legacy: Binding = serde_json::from_value(legacy).unwrap();
    assert!(!legacy.metadata_pages);
}

#[test]
fn retained_snapshot_binding_checks_owner_on_every_capture() {
    let temp = tempfile::tempdir().unwrap();
    let mut image = branch_image(temp.path());
    let root = image.root().to_owned();
    image.retain_for_branch().unwrap();
    let binding = backend::snapshot_binding(&root).unwrap();
    assert!(binding.is_some());
    assert!(!root.join("nested").exists());
    assert!(
        backend::snapshot_binding(&root.join("visited"))
            .unwrap()
            .is_none()
    );
    let descriptor = root.parent().unwrap().join(DESCRIPTOR);
    let bytes = fs::read(&descriptor).unwrap();
    fs::write(&descriptor, b"{}").unwrap();
    assert!(backend::snapshot_binding(&root).is_err());
    fs::write(&descriptor, bytes).unwrap();
    assert_eq!(backend::snapshot_binding(&root).unwrap(), binding);
    assert!(!root.join("nested").exists());
}
