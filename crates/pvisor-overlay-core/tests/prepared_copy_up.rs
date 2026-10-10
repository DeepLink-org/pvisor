use pvisor_overlay_core::{OverlayCore, profile::Profile, service::FilesystemService};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

fn fixture() -> (tempfile::TempDir, FilesystemService, Profile) {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    fs::set_permissions(lower.join("file"), fs::Permissions::from_mode(0o640)).unwrap();
    let profile = Profile::enabled("prepared-copy");
    let core = OverlayCore::new(vec![lower], root.path().join("upper"), None)
        .unwrap()
        .with_profile(profile.clone());
    (root, FilesystemService::new(core), profile)
}

#[test]
fn preparation_is_invisible_and_unused_copies_are_removed() {
    let (root, service, _) = fixture();
    let copy = service
        .prepare_copy_ups(&["file".into()], false)
        .unwrap()
        .pop()
        .unwrap();
    assert!(!root.path().join("upper/file").exists());
    assert_eq!(
        service.directory_candidates(Path::new("")).unwrap().len(),
        1
    );
    assert_eq!(fs::read_dir(root.path().join("upper")).unwrap().count(), 1);
    let guard = service.use_prepared_copy_ups(vec![copy]).unwrap();
    assert_eq!(service.metadata(Path::new("file")).unwrap().len(), 8);
    assert_eq!(
        service
            .use_prepared_copy_ups(vec![])
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EBUSY)
    );
    drop(guard);
    assert_eq!(fs::read_dir(root.path().join("upper")).unwrap().count(), 0);
}

#[test]
fn publication_reuses_bytes_and_metadata_without_copying_again() {
    let (root, service, profile) = fixture();
    let copy = service
        .prepare_copy_ups(&["file".into()], false)
        .unwrap()
        .pop()
        .unwrap();
    let guard = service.use_prepared_copy_ups(vec![copy]).unwrap();
    let upper = service.copy_up(Path::new("file")).unwrap();
    assert_eq!(fs::read(&upper).unwrap(), b"original");
    assert_eq!(fs::metadata(&upper).unwrap().mode() & 0o777, 0o640);
    assert_eq!(
        profile.report().unwrap().measurements["copy_up_bytes"].units,
        8
    );
    drop(guard);
    assert_eq!(fs::read_dir(root.path().join("upper")).unwrap().count(), 1);
}

#[test]
fn changed_sources_and_foreign_owners_are_rejected_without_publication() {
    let (root, service, _) = fixture();
    let (_, foreign, _) = fixture();
    let copy = service
        .prepare_copy_ups(&["file".into()], false)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        foreign
            .use_prepared_copy_ups(vec![copy])
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(fs::read_dir(root.path().join("upper")).unwrap().count(), 0);
    let copy = service
        .prepare_copy_ups(&["file".into()], false)
        .unwrap()
        .pop()
        .unwrap();
    fs::write(root.path().join("lower/file"), b"new contents").unwrap();
    let guard = service.use_prepared_copy_ups(vec![copy]).unwrap();
    assert_eq!(
        service
            .copy_up(Path::new("file"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EAGAIN)
    );
    assert!(!root.path().join("upper/file").exists());
    drop(guard);
    assert_eq!(fs::read_dir(root.path().join("upper")).unwrap().count(), 0);
}

#[test]
fn late_hardlink_alias_consumes_the_prepared_inode_and_preserves_sharing() {
    let (root, service, profile) = fixture();
    fs::hard_link(
        root.path().join("lower/file"),
        root.path().join("lower/alias"),
    )
    .unwrap();
    let copy = service
        .prepare_copy_ups(&["file".into()], false)
        .unwrap()
        .pop()
        .unwrap();
    let guard = service.use_prepared_copy_ups(vec![copy]).unwrap();
    let alias = service.copy_up(Path::new("alias")).unwrap();
    fs::write(&alias, b"updated").unwrap();
    let file = service.copy_up(Path::new("file")).unwrap();
    assert_eq!(
        fs::metadata(&alias).unwrap().ino(),
        fs::metadata(&file).unwrap().ino()
    );
    assert_eq!(fs::read(&file).unwrap(), b"updated");
    assert_eq!(
        fs::read(root.path().join("lower/file")).unwrap(),
        b"original"
    );
    assert_eq!(
        profile.report().unwrap().measurements["copy_up_bytes"].units,
        8
    );
    drop(guard);
}

#[test]
fn prepared_truncation_skips_only_unshared_contents() {
    for linked in [false, true] {
        let (root, service, profile) = fixture();
        if linked {
            fs::hard_link(
                root.path().join("lower/file"),
                root.path().join("lower/alias"),
            )
            .unwrap();
        }
        let copy = service
            .prepare_copy_ups(&["file".into()], true)
            .unwrap()
            .pop()
            .unwrap();
        let guard = service.use_prepared_copy_ups(vec![copy]).unwrap();
        let path = service
            .prepare_open(Path::new("file"), libc::O_RDWR | libc::O_TRUNC)
            .unwrap();
        // The adapter still performs the actual truncation after rebinding.
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            if linked { 8 } else { 0 }
        );
        assert_eq!(
            profile
                .report()
                .unwrap()
                .measurements
                .get("copy_up_bytes")
                .map_or(0, |m| m.units),
            if linked { 8 } else { 0 }
        );
        assert_eq!(
            fs::read(root.path().join("lower/file")).unwrap(),
            b"original"
        );
        drop(guard);
    }
}

#[test]
fn batch_preparation_copies_a_hardlink_group_once() {
    let (root, service, profile) = fixture();
    fs::hard_link(
        root.path().join("lower/file"),
        root.path().join("lower/alias"),
    )
    .unwrap();
    let copies = service
        .prepare_copy_ups(&["file".into(), "alias".into()], false)
        .unwrap();
    assert_eq!(copies.len(), 1);
    let guard = service.use_prepared_copy_ups(copies).unwrap();
    let alias = service.copy_up(Path::new("alias")).unwrap();
    let file = service.copy_up(Path::new("file")).unwrap();
    assert_eq!(
        fs::metadata(alias).unwrap().ino(),
        fs::metadata(file).unwrap().ino()
    );
    assert_eq!(
        profile.report().unwrap().measurements["copy_up_bytes"].units,
        8
    );
    drop(guard);
}

#[test]
fn baseline_observation_precedes_publication_and_survives_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    let journal = root.path().join("preimages");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    let core = OverlayCore::new_with_exclusions_and_preimages(
        vec![lower],
        upper.clone(),
        None,
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    let service = FilesystemService::new(core);
    let copies = service.prepare_copy_ups(&["file".into()], false).unwrap();
    assert!(
        pvisor_overlay_core::load_preimages(&journal)
            .unwrap()
            .iter()
            .any(|entry| entry.relative_path() == Path::new("file"))
    );
    assert!(!upper.join("file").exists());
    drop(copies);
    assert!(
        pvisor_overlay_core::load_preimages(&journal)
            .unwrap()
            .iter()
            .any(|entry| entry.relative_path() == Path::new("file"))
    );
    assert_eq!(fs::read_dir(upper).unwrap().count(), 0);
}

#[test]
fn remote_projection_aliases_preserve_copies_but_logical_changes_are_rejected() {
    use pvisor_overlay_core::backend::{BackendAttachment, FileAttr, FileType, ReadOnlyBackend};
    use std::{
        io,
        os::unix::fs::FileExt,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::UNIX_EPOCH,
    };
    struct Projection {
        root: PathBuf,
        attr: Mutex<FileAttr>,
    }
    impl ReadOnlyBackend for Projection {
        fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn prepare_directory(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
            Ok(self.attr.lock().unwrap().clone())
        }
        fn read_at(&self, path: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>> {
            let mut bytes = vec![0; size as usize];
            let n = fs::File::open(self.root.join(path))?.read_at(&mut bytes, offset)?;
            bytes.truncate(n);
            Ok(bytes)
        }
        fn materialize_file(&self, _: &Path) -> io::Result<()> {
            Ok(())
        }
        fn materialize_tree(&self, _: &Path) -> io::Result<()> {
            unreachable!()
        }
    }
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    fs::create_dir(&lower).unwrap();
    let lower = lower.canonicalize().unwrap();
    fs::write(lower.join("file"), b"original").unwrap();
    let metadata = fs::metadata(lower.join("file")).unwrap();
    let profile = Profile::enabled("projected-alias");
    let service = FilesystemService::new(
        OverlayCore::new(vec![lower.clone()], root.path().join("upper"), None)
            .unwrap()
            .with_profile(profile.clone()),
    );
    let projection = Arc::new(Projection {
        root: lower.clone(),
        attr: Mutex::new(FileAttr {
            ino: metadata.ino(),
            size: 8,
            blocks: 1,
            atime: UNIX_EPOCH,
            mtime: metadata.modified().unwrap(),
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: (metadata.mode() & 0o7777) as u16,
            uid: metadata.uid(),
            gid: metadata.gid(),
            nlink: 2,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }),
    });
    let _attachment = BackendAttachment::new(&lower, projection.clone()).unwrap();
    let copies = service.prepare_copy_ups(&["file".into()], false).unwrap();
    // Lazy projection changes the native cache inode's nlink/ctime, while its
    // remote metadata and contents still identify the same immutable object.
    fs::hard_link(lower.join("file"), lower.join("alias")).unwrap();
    projection.attr.lock().unwrap().mtime += std::time::Duration::from_secs(1);
    let guard = service.use_prepared_copy_ups(copies).unwrap();
    assert_eq!(
        service
            .copy_up(Path::new("alias"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EAGAIN)
    );
    drop(guard);
    projection.attr.lock().unwrap().mtime -= std::time::Duration::from_secs(1);
    let copies = service.prepare_copy_ups(&["file".into()], false).unwrap();
    let guard = service.use_prepared_copy_ups(copies).unwrap();
    let alias = service.copy_up(Path::new("alias")).unwrap();
    let file = service.copy_up(Path::new("file")).unwrap();
    assert_eq!(
        fs::metadata(alias).unwrap().ino(),
        fs::metadata(&file).unwrap().ino()
    );
    assert_eq!(fs::read(file).unwrap(), b"original");
    assert_eq!(
        profile.report().unwrap().measurements["copy_up_bytes"].units,
        16
    );
    drop(guard);
}

#[test]
fn large_preparation_batch_works_under_a_small_descriptor_limit() {
    const CHILD: &str = "PVISOR_PREPARED_BATCH_LIMIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "large_preparation_batch_works_under_a_small_descriptor_limit",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let limit = libc::rlimit {
        rlim_cur: 48,
        rlim_max: 48,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let (root, service, _) = fixture();
    let mut paths = Vec::new();
    for index in 0..300 {
        let name = format!("item-{index}");
        fs::write(root.path().join("lower").join(&name), b"data").unwrap();
        paths.push(name.into());
    }
    let copies = service.prepare_copy_ups(&paths, false).unwrap();
    assert_eq!(copies.len(), 300);
    let guard = service.use_prepared_copy_ups(copies).unwrap();
    for path in paths {
        assert_eq!(fs::read(service.copy_up(&path).unwrap()).unwrap(), b"data");
    }
    drop(guard);
    assert_eq!(
        fs::read_dir(root.path().join("upper")).unwrap().count(),
        300
    );
}
