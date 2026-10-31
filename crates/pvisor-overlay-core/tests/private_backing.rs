//! Logical identity and private native-VM backing contracts.
#![cfg(target_os = "linux")]

use pvisor_overlay_core::{
    OVERRIDE_STAT_XATTR, OverlayCore, OverlayLayout, PRIVATE_BACKING_XATTR, fingerprint_at,
    is_private_metadata_xattr, logical_identity, sys,
};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

fn layout(lower: &Path) -> OverlayLayout {
    OverlayLayout::new(vec![lower.to_owned()], lower.to_owned()).unwrap()
}

fn open(lower: &Path, upper: &Path, private: bool) -> OverlayCore {
    let layout = layout(lower);
    OverlayCore::new_for_layout(
        if private {
            layout.with_private_backing()
        } else {
            layout
        },
        upper.to_owned(),
        None,
        vec![],
        None,
    )
    .unwrap()
}

#[test]
fn private_root_and_copied_directories_keep_logical_identity_and_owner_access() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    fs::create_dir_all(lower.join("readonly/nested")).unwrap();
    fs::write(lower.join("readonly/file"), b"lower").unwrap();
    // Exercise a foreign logical owner without requiring a privileged test host.
    sys::set_xattr(&lower, OsStr::new(OVERRIDE_STAT_XATTR), b"0:42:40555", 0).unwrap();
    fs::set_permissions(&lower, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(lower.join("readonly"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(
        lower.join("readonly/file"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let before = fingerprint_at(&lower, Path::new("")).unwrap();
    let core = open(&lower, &upper, true);
    assert_eq!(fs::metadata(&upper).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(fs::metadata(&upper).unwrap().uid(), unsafe {
        libc::geteuid()
    });
    let identity = logical_identity(&upper, &fs::metadata(&upper).unwrap()).unwrap();
    assert_eq!(
        (identity.uid, identity.gid, identity.mode),
        (0, 42, 0o40555)
    );
    assert_eq!(fingerprint_at(&upper, Path::new("")).unwrap(), before);
    assert_eq!(
        sys::get_xattr(&upper, OsStr::new(PRIVATE_BACKING_XATTR)).unwrap(),
        b"1"
    );
    fs::create_dir(upper.join("scratch")).unwrap();
    fs::remove_dir(upper.join("scratch")).unwrap();
    core.ensure_upper_parents(Path::new("readonly/nested/new"))
        .unwrap();
    let copied = upper.join("readonly");
    assert_eq!(fs::metadata(&copied).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(
        logical_identity(&copied, &fs::metadata(&copied).unwrap())
            .unwrap()
            .mode
            & 0o7777,
        0o555
    );
    assert!(sys::get_xattr(&copied, OsStr::new(PRIVATE_BACKING_XATTR)).is_err());
    fs::write(copied.join("new"), b"private").unwrap();
    let file = core.copy_up(Path::new("readonly/file")).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(
        fingerprint_at(&upper, Path::new("readonly/file")).unwrap(),
        fingerprint_at(&lower, Path::new("readonly/file")).unwrap()
    );
    fs::write(file, b"changed").unwrap();
    assert_eq!(fs::read(lower.join("readonly/file")).unwrap(), b"lower");
    assert_eq!(fingerprint_at(&lower, Path::new("")).unwrap(), before);
    // Let TempDir clean up the read-only lower as an ordinary user.
    fs::set_permissions(&copied, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&lower, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(lower.join("readonly"), fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn repeated_empty_private_initialization_retains_guest_root_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    fs::create_dir(&lower).unwrap();
    drop(open(&lower, &upper, true));
    sys::set_xattr(&upper, OsStr::new(OVERRIDE_STAT_XATTR), b"123:456:40000", 0).unwrap();
    // Even an initialization-style reopen must not overwrite the guest identity
    // from lower just because upper has no directory entries.
    drop(open(&lower, &upper, true));
    drop(open(&lower, &upper, false));
    let core =
        OverlayCore::open_existing_for_layout(layout(&lower), upper.clone(), None, vec![], None)
            .unwrap();
    let identity = logical_identity(&upper, &fs::metadata(&upper).unwrap()).unwrap();
    assert_eq!(
        (identity.uid, identity.gid, identity.mode),
        (123, 456, 0o40000)
    );
    fs::create_dir(core.upper().join("still-writable")).unwrap();
}

#[test]
fn ordinary_core_stays_native_and_nonempty_upper_cannot_be_converted() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    fs::create_dir(&lower).unwrap();
    fs::set_permissions(&lower, fs::Permissions::from_mode(0o555)).unwrap();
    drop(open(&lower, &upper, false));
    assert_eq!(fs::metadata(&upper).unwrap().mode() & 0o7777, 0o555);
    assert!(sys::get_xattr(&upper, OsStr::new(PRIVATE_BACKING_XATTR)).is_err());
    fs::set_permissions(&upper, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(upper.join("existing"), b"preserve").unwrap();
    assert!(
        OverlayCore::new_for_layout(
            layout(&lower).with_private_backing(),
            upper.clone(),
            None,
            vec![],
            None
        )
        .is_err()
    );
    assert_eq!(fs::read(upper.join("existing")).unwrap(), b"preserve");
    fs::set_permissions(lower, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn control_xattrs_do_not_conflict_but_logical_identity_and_user_xattrs_do() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("file");
    fs::write(&file, b"same").unwrap();
    let metadata = fs::metadata(&file).unwrap();
    let before = fingerprint_at(temp.path(), Path::new("file")).unwrap();
    let value = format!(
        "{}:{}:0{:o}",
        metadata.uid(),
        metadata.gid(),
        metadata.mode()
    );
    sys::set_xattr(&file, OsStr::new(OVERRIDE_STAT_XATTR), value.as_bytes(), 0).unwrap();
    sys::set_xattr(&file, OsStr::new(PRIVATE_BACKING_XATTR), b"1", 0).unwrap();
    assert_eq!(
        before,
        fingerprint_at(temp.path(), Path::new("file")).unwrap()
    );
    sys::set_xattr(&file, OsStr::new(OVERRIDE_STAT_XATTR), b"123:456:444", 0).unwrap();
    assert_eq!(logical_identity(&file, &metadata).unwrap().mode, 0o100444);
    assert_ne!(
        before,
        fingerprint_at(temp.path(), Path::new("file")).unwrap()
    );
    sys::set_xattr(&file, OsStr::new(OVERRIDE_STAT_XATTR), value.as_bytes(), 0).unwrap();
    sys::set_xattr(&file, OsStr::new("user.application"), b"change", 0).unwrap();
    assert_ne!(
        before,
        fingerprint_at(temp.path(), Path::new("file")).unwrap()
    );
    sys::set_xattr(&file, OsStr::new(OVERRIDE_STAT_XATTR), b"corrupt", 0).unwrap();
    assert_eq!(
        logical_identity(&file, &metadata)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EIO)
    );
    assert!(fingerprint_at(temp.path(), Path::new("file")).is_err());
    for name in [OVERRIDE_STAT_XATTR, PRIVATE_BACKING_XATTR] {
        assert!(is_private_metadata_xattr(OsStr::new(name)));
    }
    assert!(!is_private_metadata_xattr(OsStr::new("user.application")));
}

#[test]
fn failed_private_parent_identity_import_does_not_publish_host_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    fs::create_dir_all(lower.join("directory")).unwrap();
    sys::set_xattr(
        &lower.join("directory"),
        OsStr::new(OVERRIDE_STAT_XATTR),
        b"invalid",
        0,
    )
    .unwrap();
    let core = open(&lower, &upper, true);
    assert!(
        core.ensure_upper_parents(Path::new("directory/child"))
            .is_err()
    );
    assert!(!upper.join("directory").exists());
    assert!(lower.join("directory").exists());
}

#[test]
fn raw_snapshot_metadata_keeps_physical_access_and_logical_controls() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    let snapshot = temp.path().join("snapshot");
    fs::create_dir(&lower).unwrap();
    fs::create_dir(&snapshot).unwrap();
    sys::set_xattr(&lower, OsStr::new(OVERRIDE_STAT_XATTR), b"123:456:40555", 0).unwrap();
    drop(open(&lower, &upper, true));
    pvisor_overlay_core::apply::copy_snapshot_metadata(&upper, &snapshot).unwrap();
    let metadata = fs::metadata(&snapshot).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o700);
    assert_eq!(metadata.uid(), fs::metadata(&upper).unwrap().uid());
    let logical = logical_identity(&snapshot, &metadata).unwrap();
    assert_eq!(
        (logical.uid, logical.gid, logical.mode),
        (123, 456, 0o40555)
    );
    assert_eq!(
        sys::get_xattr(&snapshot, OsStr::new(PRIVATE_BACKING_XATTR)).unwrap(),
        b"1"
    );
    fs::create_dir(snapshot.join("still-private-writable")).unwrap();
}

// Linux's POSIX ACL xattr ABI: little-endian version 2, then
// (u16 tag, u16 permissions, u32 id) entries in canonical tag/id order.
// A named user keeps the access ACL nontrivial, so Linux stores the xattr
// instead of folding it into mode bits. No libacl or setfacl is needed.
fn posix_acl(owner_permissions: u16, other_permissions: u16) -> Vec<u8> {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (0x01u16, owner_permissions, u32::MAX),
        (0x02, 7, 424242),
        (0x04, 0, u32::MAX),
        (0x10, 7, u32::MAX),
        (0x20, other_permissions, u32::MAX),
    ] {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&permissions.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    bytes
}

#[test]
fn private_copy_up_rejects_real_posix_acls_without_publishing_partial_entries() {
    for (attribute, directory) in [
        ("system.posix_acl_access", false),
        ("system.posix_acl_access", true),
        ("system.posix_acl_default", true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir(&lower).unwrap();
        let source = lower.join("entry");
        if directory {
            fs::create_dir(&source).unwrap();
        } else {
            fs::write(&source, b"unchanged lower bytes").unwrap();
        }
        // Access ACLs revoke owner write and grant others access. Installing
        // this after private chmod would undo both backing-mode guarantees.
        let acl = posix_acl(5, 7);
        sys::set_xattr(&source, OsStr::new("user.fixture"), b"retain", 0).unwrap();
        sys::set_xattr(&source, OsStr::new(attribute), &acl, 0).unwrap();
        assert_eq!(sys::get_xattr(&source, OsStr::new(attribute)).unwrap(), acl);
        if attribute == "system.posix_acl_access" {
            assert_eq!(fs::metadata(&source).unwrap().mode() & 0o777, 0o577);
        }
        let before = fingerprint_at(&lower, Path::new("entry")).unwrap();
        let core = open(&lower, &upper, true);
        let error = core.copy_up(Path::new("entry")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{attribute}");
        assert!(error.to_string().contains("POSIX ACL"));
        assert_eq!(
            fs::read_dir(&upper).unwrap().count(),
            0,
            "leftover copy-up entry"
        );
        if directory {
            let error = core
                .ensure_upper_parents(Path::new("entry/child"))
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        } else {
            let error = match core.prepare_copy_ups(&["entry".into()], false) {
                Err(error) => error,
                Ok(_) => panic!("ACL preparation must fail"),
            };
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
            assert_eq!(fs::read(&source).unwrap(), b"unchanged lower bytes");
        }
        assert_eq!(
            fs::read_dir(&upper).unwrap().count(),
            0,
            "leftover parent or preparation"
        );
        assert_eq!(fs::metadata(&upper).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(fingerprint_at(&lower, Path::new("entry")).unwrap(), before);
        assert_eq!(sys::get_xattr(&source, OsStr::new(attribute)).unwrap(), acl);
    }
}

#[test]
fn private_root_acl_rejection_does_not_change_upper_permissions_or_publish_identity() {
    for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir(&lower).unwrap();
        fs::create_dir(&upper).unwrap();
        fs::set_permissions(&upper, fs::Permissions::from_mode(0o700)).unwrap();
        sys::set_xattr(&upper, OsStr::new("user.existing"), b"untouched", 0).unwrap();
        sys::set_xattr(&lower, OsStr::new("user.source"), b"not-published", 0).unwrap();
        sys::set_xattr(&lower, OsStr::new(OVERRIDE_STAT_XATTR), b"0:0:40577", 0).unwrap();
        let acl = posix_acl(5, 7);
        sys::set_xattr(&lower, OsStr::new(attribute), &acl, 0).unwrap();
        assert_eq!(sys::get_xattr(&lower, OsStr::new(attribute)).unwrap(), acl);
        let lower_before = fingerprint_at(&lower, Path::new("")).unwrap();
        let upper_before = fingerprint_at(&upper, Path::new("")).unwrap();
        let error = match OverlayCore::new_for_layout(
            layout(&lower).with_private_backing(),
            upper.clone(),
            None,
            vec![],
            None,
        ) {
            Err(error) => error,
            Ok(_) => panic!("root ACL import must fail"),
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(fingerprint_at(&upper, Path::new("")).unwrap(), upper_before);
        assert_eq!(fingerprint_at(&lower, Path::new("")).unwrap(), lower_before);
        assert_eq!(fs::metadata(&upper).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(fs::metadata(&upper).unwrap().uid(), unsafe {
            libc::geteuid()
        });
        assert_eq!(fs::read_dir(&upper).unwrap().count(), 0);
        for control in [
            OVERRIDE_STAT_XATTR,
            PRIVATE_BACKING_XATTR,
            attribute,
            "user.source",
        ] {
            assert!(
                sys::get_xattr(&upper, OsStr::new(control)).is_err(),
                "published {control}"
            );
        }
    }
}

#[test]
fn ordinary_overlay_still_copies_real_posix_acls() {
    for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(lower.join("entry")).unwrap();
        let acl = posix_acl(7, 0);
        sys::set_xattr(&lower, OsStr::new(attribute), &acl, 0).unwrap();
        sys::set_xattr(&lower.join("entry"), OsStr::new(attribute), &acl, 0).unwrap();
        let core = open(&lower, &upper, false);
        assert_eq!(sys::get_xattr(&upper, OsStr::new(attribute)).unwrap(), acl);
        core.copy_up(Path::new("entry")).unwrap();
        assert_eq!(
            sys::get_xattr(&upper.join("entry"), OsStr::new(attribute)).unwrap(),
            acl
        );
        assert!(sys::get_xattr(&upper, OsStr::new(PRIVATE_BACKING_XATTR)).is_err());
    }
}

#[test]
fn invalid_private_marker_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    fs::create_dir(&lower).unwrap();
    fs::create_dir(&upper).unwrap();
    sys::set_xattr(&upper, OsStr::new(PRIVATE_BACKING_XATTR), b"2", 0).unwrap();
    assert!(OverlayCore::new(vec![lower], upper, None).is_err());
}
