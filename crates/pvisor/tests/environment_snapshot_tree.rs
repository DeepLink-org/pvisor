#![cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
use pvisor::environment_snapshot::{copy_owned_tree, inventory, verify_tree};
use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, symlink},
        net::UnixListener,
    },
};

#[test]
fn full_copy_preserves_metadata_links_and_unvisited_objects() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::create_dir(source.join("empty")).unwrap();
    fs::write(source.join("data"), b"owned contents").unwrap();
    fs::hard_link(source.join("data"), source.join("alias")).unwrap();
    symlink("data", source.join("link")).unwrap();
    symlink("/guest/absolute/path", source.join("absolute")).unwrap();
    let path = std::ffi::CString::new(source.join("data").as_os_str().as_bytes()).unwrap();
    #[cfg(target_os = "macos")]
    assert_eq!(
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"user.pvisor-test".as_ptr(),
                b"value".as_ptr().cast(),
                5,
                0,
                0,
            )
        },
        0
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"user.pvisor-test".as_ptr(),
                b"value".as_ptr().cast(),
                5,
                0,
            )
        },
        0
    );
    let destination = directory.path().join("copy");
    let saved = copy_owned_tree(&source, &destination).unwrap();
    assert_eq!(saved, inventory(&destination).unwrap());
    assert_ne!(
        fs::metadata(source.join("data")).unwrap().ino(),
        fs::metadata(destination.join("data")).unwrap().ino()
    );
    assert_eq!(
        fs::metadata(destination.join("data")).unwrap().ino(),
        fs::metadata(destination.join("alias")).unwrap().ino()
    );
    fs::remove_dir_all(source).unwrap();
    verify_tree(&destination, &saved).unwrap();
    fs::write(destination.join("data"), b"changed contents").unwrap();
    assert!(verify_tree(&destination, &saved).is_err());
}

#[test]
fn unvisited_and_extra_files_and_broken_hardlinks_are_detected() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"data").unwrap();
    fs::hard_link(source.join("data"), source.join("alias")).unwrap();
    let expected = inventory(&source).unwrap();
    fs::write(source.join("extra"), b"unvisited").unwrap();
    assert!(verify_tree(&source, &expected).is_err());
    fs::remove_file(source.join("extra")).unwrap();
    fs::remove_file(source.join("alias")).unwrap();
    fs::copy(source.join("data"), source.join("alias")).unwrap();
    assert!(verify_tree(&source, &expected).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn copied_acl_and_xattrs_are_bound_to_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"contents").unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity"
            ])
            .arg(source.join("data"))
            .status()
            .unwrap()
            .success()
    );
    let destination = directory.path().join("copy");
    let saved = copy_owned_tree(&source, &destination).unwrap();
    let entry = saved
        .entries
        .iter()
        .find(|entry| entry.path == b"data")
        .unwrap();
    assert!(
        entry
            .acl
            .as_ref()
            .is_some_and(|acl| acl.windows(8).any(|part| part == b"everyone"))
    );
    assert!(
        std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(destination.join("data"))
            .status()
            .unwrap()
            .success()
    );
    assert!(verify_tree(&destination, &saved).is_err());
}

#[test]
fn external_hardlinks_special_files_and_existing_destination_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"data").unwrap();
    fs::hard_link(source.join("data"), directory.path().join("outside")).unwrap();
    assert!(inventory(&source).is_err());
    fs::remove_file(directory.path().join("outside")).unwrap();
    let listener = UnixListener::bind(source.join("socket")).unwrap();
    assert!(inventory(&source).is_err());
    drop(listener);
    fs::remove_file(source.join("socket")).unwrap();
    assert!(copy_owned_tree(&source, &source.join("nested")).is_err());
    let destination = directory.path().join("existing");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("sentinel"), b"keep").unwrap();
    assert!(copy_owned_tree(&source, &destination).is_err());
    assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"keep");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_posix_acl_survives_copy_and_is_bound_to_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"acl contents").unwrap();
    // Encode a valid Linux POSIX ACL: owner, named user, group, mask, other.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, 12345),
        (4, 4, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&permissions.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    let path = std::ffi::CString::new(source.join("data").as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        },
        0
    );
    let destination = directory.path().join("copy");
    let saved = copy_owned_tree(&source, &destination).unwrap();
    assert!(
        saved
            .entries
            .iter()
            .find(|entry| entry.path == b"data")
            .unwrap()
            .xattrs
            .iter()
            .any(|(name, value)| name == b"system.posix_acl_access" && value == &acl)
    );
    let path = std::ffi::CString::new(destination.join("data").as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::removexattr(path.as_ptr(), c"system.posix_acl_access".as_ptr()) },
        0
    );
    assert!(verify_tree(&destination, &saved).is_err());
}
