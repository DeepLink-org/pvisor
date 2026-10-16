//! Checked absence and projection ownership through the existing public service.
use pvisor_overlay_core::{
    FileAccessPolicy, OverlayCore, PathFingerprint,
    backend::{self, BackendAttachment, FileAttr, ReadOnlyBackend},
    load_preimages,
    service::FilesystemService,
};
use std::{
    fs, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct MissingBackend(Arc<AtomicUsize>, i32);
impl ReadOnlyBackend for MissingBackend {
    fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(io::Error::from_raw_os_error(self.1))
    }
    fn prepare_directory(&self, _: &Path) -> io::Result<()> {
        self.prepare_metadata(Path::new(""))
    }
    fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
        self.prepare_metadata(Path::new(""))?;
        unreachable!()
    }
    fn read_at(&self, _: &Path, _: u64, _: u32) -> io::Result<Vec<u8>> {
        self.prepare_metadata(Path::new(""))?;
        unreachable!()
    }
    fn materialize_file(&self, _: &Path) -> io::Result<()> {
        self.prepare_metadata(Path::new(""))
    }
    fn materialize_tree(&self, _: &Path) -> io::Result<()> {
        self.prepare_metadata(Path::new(""))
    }
}

#[test]
fn checked_missing_result_publishes_absence_and_keeps_the_old_error_contract() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    fs::create_dir_all(lower.join("dir")).unwrap();
    fs::write(lower.join("dir/file"), b"original").unwrap();
    let journal = temp.path().join("journal");
    let service = FilesystemService::new(
        OverlayCore::new_with_exclusions_and_preimages(
            vec![lower],
            temp.path().join("upper"),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap(),
    );
    let positive = service
        .metadata_for_backing_lookup_optional(Path::new("dir/file"))
        .unwrap()
        .unwrap();
    let ordinary = service
        .metadata_for_backing_lookup(Path::new("dir/file"))
        .unwrap();
    assert_eq!(positive.entry.metadata.len(), 8);
    assert_eq!(positive.parents, ordinary.parents);
    assert!(
        service
            .metadata_for_backing_lookup_optional(Path::new("dir/missing"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service
            .metadata_for_backing_lookup(Path::new("dir/missing"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOENT)
    );
    let observations = load_preimages(&journal).unwrap();
    assert!(
        observations
            .iter()
            .any(|entry| entry.relative_path() == Path::new("dir/missing")
                && entry.state == PathFingerprint::Absent)
    );
    fs::remove_dir_all(&journal).unwrap();
    fs::write(&journal, b"not a directory").unwrap();
    let optional_error = service
        .metadata_for_backing_lookup_optional(Path::new("other"))
        .unwrap_err();
    let ordinary_error = service
        .metadata_for_backing_lookup(Path::new("other"))
        .unwrap_err();
    assert_eq!(optional_error.raw_os_error(), ordinary_error.raw_os_error());
    assert!(optional_error.raw_os_error().is_some());
}

#[test]
fn excluded_denied_and_backend_enoent_remain_errors_instead_of_checked_absence() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    let service = FilesystemService::new(
        OverlayCore::new_with_exclusions_and_preimages(
            vec![lower.clone()],
            temp.path().join("upper"),
            None,
            vec!["hidden".into()],
            None,
        )
        .unwrap(),
    )
    .with_access_policy(&FileAccessPolicy::new(vec!["denied".into()], vec![]).unwrap());
    for (path, errno) in [("hidden", libc::ENOENT), ("denied", libc::EACCES)] {
        assert_eq!(
            service
                .metadata_for_backing_lookup_optional(Path::new(path))
                .unwrap_err()
                .raw_os_error(),
            Some(errno)
        );
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let attachment = BackendAttachment::new(
        &lower,
        Arc::new(MissingBackend(calls.clone(), libc::ENOENT)),
    )
    .unwrap();
    // The existing resolver interprets backend NotFound as a missing physical
    // candidate. This is observed absence, not a permanent remote guarantee.
    assert!(
        service
            .metadata_for_backing_lookup_optional(Path::new("missing"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service
            .metadata_for_backing_lookup(Path::new("missing"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOENT)
    );
    assert!(calls.load(Ordering::Relaxed) > 0);
    drop(attachment);
    for errno in [libc::EIO, libc::EACCES] {
        let _attachment =
            BackendAttachment::new(&lower, Arc::new(MissingBackend(calls.clone(), errno))).unwrap();
        assert_eq!(
            service
                .metadata_for_backing_lookup_optional(Path::new("missing"))
                .unwrap_err()
                .raw_os_error(),
            Some(errno)
        );
        assert_eq!(
            service
                .metadata_for_backing_lookup(Path::new("missing"))
                .unwrap_err()
                .raw_os_error(),
            Some(errno)
        );
    }
}

#[test]
fn projection_overlap_query_is_scoped_readonly_and_does_not_materialize_missing_names() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir(root.join("projection")).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let attachment = BackendAttachment::new(
        &root.join("projection"),
        Arc::new(MissingBackend(calls.clone(), libc::ENOENT)),
    )
    .unwrap();
    assert!(backend::attachment_overlaps(&root).unwrap());
    assert!(backend::attachment_overlaps(&root.join("projection/missing/child")).unwrap());
    assert!(!backend::attachment_overlaps(&root.join("sibling")).unwrap());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        backend::attachment_overlaps(Path::new("relative"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    drop(attachment);
    assert!(!backend::attachment_overlaps(&root).unwrap());
}

#[test]
fn exclusive_namespace_validation_rejects_journal_aliases_without_enabling_a_cache() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    fs::create_dir(&lower).unwrap();
    let upper = temp.path().join("upper");
    let service = FilesystemService::new(
        OverlayCore::new_with_exclusions_and_preimages(
            vec![lower],
            upper.clone(),
            None,
            vec![],
            Some(upper.join("journal")),
        )
        .unwrap(),
    );
    assert_eq!(
        service
            .validate_exclusive_upper_namespace()
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!service.exclusive_upper_absence_cache_enabled());
}
