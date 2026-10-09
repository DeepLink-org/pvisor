use super::*;
use crate::image::cache::{MAX_READ, backend::tests::fixture};
use pvisor_overlay_core::{OverlayCore, backend, service::FilesystemService};
use std::{fs::File, sync::atomic::Ordering};

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
