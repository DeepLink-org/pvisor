//! Native filesystem-CoW contracts, independent of FUSE and VM availability.
#![cfg(unix)]
use pvisor_overlay_core::{apply::clone_overlay_upper, sys};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

#[test]
fn cow_fork_preserves_raw_state_and_independent_inode_ownership() {
    // /tmp may be tmpfs, so exercise the repository's backing filesystem.
    let root = tempfile::Builder::new()
        .prefix(".cow-")
        .tempdir_in(
            std::env::var_os("PVISOR_TEST_COW_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into()),
        )
        .unwrap();
    let source = root.path().join("parent");
    let child = root.path().join("child");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), vec![b'x'; 131072]).unwrap();
    fs::hard_link(source.join("file"), source.join("alias")).unwrap();
    fs::set_permissions(source.join("file"), fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("file", source.join("link")).unwrap();
    fs::write(source.join(".wh.deleted"), []).unwrap();
    fs::create_dir(source.join("opaque")).unwrap();
    fs::write(source.join("opaque/.wh..wh..opq"), []).unwrap();
    sys::set_xattr(
        &source.join("file"),
        OsStr::new("user.pvisor.test"),
        b"raw",
        0,
    )
    .unwrap();
    if let Err(error) = clone_overlay_upper(&source, &child) {
        assert!(error.to_string().contains("CoW"), "{error}");
        assert!(!child.exists(), "failed clone must remove its partial tree");
        assert_eq!(fs::read(source.join("file")).unwrap(), vec![b'x'; 131072]);
        assert!(
            std::env::var_os("PVISOR_TEST_REQUIRE_COW").is_none(),
            "native CoW required by this shard: {error}"
        );
        eprintln!("native CoW unavailable on this filesystem; failure/cleanup verified: {error}");
        return;
    }
    let parent_metadata = fs::metadata(source.join("file")).unwrap();
    let child_metadata = fs::metadata(child.join("file")).unwrap();
    assert_ne!(parent_metadata.ino(), child_metadata.ino());
    assert_eq!(child_metadata.mode() & 0o777, 0o640);
    assert_eq!(
        child_metadata.ino(),
        fs::metadata(child.join("alias")).unwrap().ino()
    );
    assert_eq!(
        fs::read_link(child.join("link")).unwrap(),
        Path::new("file")
    );
    assert!(child.join(".wh.deleted").exists());
    assert!(child.join("opaque/.wh..wh..opq").exists());
    assert_eq!(
        sys::get_xattr(&child.join("file"), OsStr::new("user.pvisor.test")).unwrap(),
        b"raw"
    );
    fs::write(source.join("file"), b"parent changed").unwrap();
    assert_eq!(fs::read(child.join("file")).unwrap(), vec![b'x'; 131072]);
    fs::write(child.join("file"), b"child changed").unwrap();
    assert_eq!(fs::read(child.join("alias")).unwrap(), b"child changed");
    assert_eq!(fs::read(source.join("file")).unwrap(), b"parent changed");
    fs::rename(child.join("file"), child.join("renamed")).unwrap();
    fs::remove_file(child.join("alias")).unwrap();
    assert!(source.join("alias").exists());
    fs::remove_dir_all(&source).unwrap();
    assert_eq!(fs::read(child.join("renamed")).unwrap(), b"child changed");
}

#[test]
fn cow_fork_refuses_existing_destination_and_overlap_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("parent");
    let destination = root.path().join("existing");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"parent").unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("unrelated"), b"keep").unwrap();
    assert!(clone_overlay_upper(&source, &destination).is_err());
    assert_eq!(fs::read(destination.join("unrelated")).unwrap(), b"keep");
    assert!(clone_overlay_upper(&source, &source.join("child")).is_err());
    assert!(!source.join("child").exists());
    assert_eq!(fs::read(source.join("file")).unwrap(), b"parent");
}

#[cfg(target_os = "linux")]
#[test]
fn unsupported_cow_does_not_fall_back_to_copying_bytes() {
    // Linux shared memory is tmpfs and deliberately has no reflink support.
    let Ok(root) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let source = root.path().join("source");
    let child = root.path().join("child");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"contents").unwrap();
    let error = clone_overlay_upper(&source, &child).unwrap_err();
    assert!(
        error.to_string().contains("no byte-copy fallback"),
        "{error}"
    );
    assert!(!child.exists());
    assert_eq!(fs::read(source.join("file")).unwrap(), b"contents");
}
