//! Exact-root logical stability must not imply native/physical immutability.
use pvisor_overlay_core::backend::{self, BackendAttachment, FileAttr, ReadOnlyBackend};
use std::{io, path::Path, sync::Arc};

struct Backend;
impl ReadOnlyBackend for Backend {
    fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn prepare_directory(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
        Err(io::Error::other("query must not call backend"))
    }
    fn read_at(&self, _: &Path, _: u64, _: u32) -> io::Result<Vec<u8>> {
        Err(io::Error::other("query must not read contents"))
    }
    fn materialize_file(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn materialize_tree(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn ordinary_attachment_and_readonly_permissions_grant_no_stability() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    assert!(backend::stable_namespace(temp.path()).unwrap().is_none());
    let _attachment = BackendAttachment::new(temp.path(), Arc::new(Backend)).unwrap();
    assert!(backend::stable_namespace(temp.path()).unwrap().is_none());
}

#[test]
fn logical_lease_pins_registration_and_rejects_overlapping_replacements() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let child = root.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let attachment = BackendAttachment::new_stable_namespace(&root, Arc::new(Backend)).unwrap();
    let lease = backend::stable_namespace(&root).unwrap().unwrap();
    assert!(backend::stable_namespace(&child).unwrap().is_none());
    drop(attachment);
    for path in [&root, &child, &temp.path().to_owned()] {
        assert_eq!(
            BackendAttachment::new(path, Arc::new(Backend))
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EEXIST)
        );
    }
    drop(lease);
    let _ordinary = BackendAttachment::new(&root, Arc::new(Backend)).unwrap();
    assert!(backend::stable_namespace(&root).unwrap().is_none());
}

#[test]
fn stable_registration_cannot_cover_an_existing_mutable_attachment() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let _ordinary = BackendAttachment::new(&child, Arc::new(Backend)).unwrap();
    assert_eq!(
        BackendAttachment::new_stable_namespace(temp.path(), Arc::new(Backend))
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EEXIST)
    );
}
