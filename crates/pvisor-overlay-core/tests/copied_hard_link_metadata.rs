//! Core-scoped copy-up identity queries, independent of protocol inode owners.
use pvisor_overlay_core::{OverlayCore, service::FilesystemService};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[test]
fn copied_alias_query_tracks_upper_growth_rename_and_removal_without_materializing() {
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("a"), b"abcd").unwrap();
    fs::hard_link(lower.join("a"), lower.join("z")).unwrap();
    let profile = pvisor_overlay_core::profile::Profile::enabled("copied-alias");
    let service =
        FilesystemService::new(OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap())
            .with_profile(profile.clone());
    assert!(
        service
            .metadata_with_copied_hard_link(Path::new("z"))
            .unwrap()
            .1
            .is_none()
    );
    service.copy_up(Path::new("a")).unwrap();
    fs::write(upper.join("a"), b"abcdefgh").unwrap();
    let before = profile.report().unwrap().measurements["resolve"].calls;
    let (physical, metadata) = service
        .metadata_with_copied_hard_link(Path::new("z"))
        .unwrap();
    assert_eq!(
        profile.report().unwrap().measurements["resolve"].calls - before,
        1
    );
    assert_eq!(physical.ino(), fs::metadata(lower.join("z")).unwrap().ino());
    assert_eq!(physical.len(), 4);
    let metadata = metadata.unwrap();
    assert_eq!(metadata.len(), 8);
    assert_eq!(metadata.ino(), fs::metadata(upper.join("a")).unwrap().ino());
    assert!(!upper.join("z").exists(), "query must not create an alias");
    assert_eq!(
        service.metadata(Path::new("z")).unwrap().len(),
        4,
        "generic physical resolution is unchanged"
    );
    service
        .rename(Path::new("a"), Path::new("moved"), false)
        .unwrap();
    assert_eq!(
        service
            .metadata_with_copied_hard_link(Path::new("z"))
            .unwrap()
            .1
            .unwrap()
            .len(),
        8
    );
    assert!(
        service
            .metadata_with_copied_hard_link(Path::new("moved"))
            .unwrap()
            .1
            .is_none(),
        "upper winner needs no alias query"
    );
    service.remove(Path::new("moved"), false).unwrap();
    assert!(
        service
            .metadata_with_copied_hard_link(Path::new("z"))
            .unwrap()
            .1
            .is_none()
    );
    assert_eq!(fs::read(lower.join("z")).unwrap(), b"abcd");
    assert_eq!(
        service
            .metadata_with_copied_hard_link(Path::new("missing"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOENT)
    );
}
