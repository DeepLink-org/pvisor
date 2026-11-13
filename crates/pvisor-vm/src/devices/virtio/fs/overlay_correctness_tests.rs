// Included in overlay::tests to exercise the private protocol adapter.

// libc S_IF* constants are u16 on macOS and u32 on Linux; local u32
// constants keep assertions and mknod modes cast-free on both platforms.
const S_IFMT: u32 = 0o170000;
const S_IFCHR: u32 = 0o020000;
const S_IFREG: u32 = 0o100000;

fn correctness_fixture(root: &Path) -> OverlayFs {
    let config = parent_cache_fixture(root).snapshot_config.clone();
    let fs = OverlayFs::new(
        Config {
            access_policy: Default::default(),
            excluded_paths: vec![],
            ..config
        },
        Arc::new(InodeAllocator::new()),
    )
    .unwrap();
    fs.init(FsOptions::empty()).unwrap();
    fs
}

fn correctness_read(fs: &OverlayFs, ctx: Context, inode: u64, handle: u64) -> Vec<u8> {
    let mut out = ReadWriter {
        gate: None,
        bytes: Vec::new(),
    };
    fs.read(ctx, inode, handle, &mut out, 4096, 0, None, 0)
        .unwrap();
    out.bytes
}

#[test]
fn linux_append_open_preserves_lower_bytes_and_only_truncate_skips_copying() {
    for extra in [bindings::LINUX_O_APPEND, bindings::LINUX_O_TRUNC] {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"original").unwrap();
        let fs = correctness_fixture(temp.path());
        let ctx = root_context();
        fs.init(FsOptions::empty()).unwrap();
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let flags = (1 | extra) as u32; // Linux O_WRONLY plus the wire flag.
        let handle = fs.open(ctx, inode, false, flags).unwrap().0.unwrap();
        let expected = if extra == bindings::LINUX_O_APPEND {
            b"original".as_slice()
        } else {
            b""
        };
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            expected
        );
        fs.release(ctx, inode, flags, handle, false, false, None)
            .unwrap();
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"original");
    }
}

#[test]
fn legacy_syncfs_returns_linux_enosys_on_every_host() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    assert_eq!(
        fs.syncfs(Context {
            uid: 0,
            gid: 0,
            pid: 1
        })
        .unwrap_err()
        .raw_os_error(),
        Some(38)
    );
}

#[test]
fn lower_hardlinks_keep_one_inode_and_current_bytes_after_copyup_and_unlink() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    std::fs::create_dir(&lower).unwrap();
    std::fs::write(lower.join("file"), b"original").unwrap();
    std::fs::hard_link(lower.join("file"), lower.join("alias")).unwrap();
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap().inode,
        inode
    );
    let writer = fs.open(ctx, inode, false, 2).unwrap().0.unwrap();
    fs.write(
        ctx,
        inode,
        writer,
        WriteReader(io::Cursor::new(b"changed!".to_vec())),
        8,
        0,
        None,
        false,
        false,
        2,
    )
    .unwrap();
    fs.release(ctx, inode, 2, writer, false, false, None)
        .unwrap();
    let alias = fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap();
    assert_eq!(alias.inode, inode);
    let reader = fs.open(ctx, inode, false, 0).unwrap().0.unwrap();
    assert_eq!(correctness_read(&fs, ctx, inode, reader), b"changed!");
    fs.release(ctx, inode, 0, reader, false, false, None)
        .unwrap();
    fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap().inode,
        inode
    );
    let reader = fs.open(ctx, inode, false, 0).unwrap().0.unwrap();
    assert_eq!(correctness_read(&fs, ctx, inode, reader), b"changed!");
    fs.release(ctx, inode, 0, reader, false, false, None)
        .unwrap();
    assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"original");
}

#[test]
fn rename_keeps_a_looked_up_victim_openable_until_forget_without_writeback() {
    for victim_layer in ["upper", "lower"] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = correctness_fixture(temp.path());
        std::fs::write(temp.path().join("upper/source"), b"new").unwrap();
        std::fs::write(temp.path().join(victim_layer).join("victim"), b"old").unwrap();
        let ctx = root_context();
        let victim = fs.lookup(ctx, fuse::ROOT_ID, c"victim").unwrap().inode;
        fs.rename(ctx, fuse::ROOT_ID, c"source", fuse::ROOT_ID, c"victim", 0)
            .unwrap();
        let reader = fs.open(ctx, victim, false, 0).unwrap().0.unwrap();
        assert_eq!(correctness_read(&fs, ctx, victim, reader), b"old");
        fs.release(ctx, victim, 0, reader, false, false, None)
            .unwrap();
        let replacement = fs.lookup(ctx, fuse::ROOT_ID, c"victim").unwrap().inode;
        assert_ne!(replacement, victim);
        let reader = fs.open(ctx, replacement, false, 0).unwrap().0.unwrap();
        assert_eq!(correctness_read(&fs, ctx, replacement, reader), b"new");
        fs.release(ctx, replacement, 0, reader, false, false, None)
            .unwrap();
        fs.forget(ctx, victim, 1);
        assert!(fs.open(ctx, victim, false, 0).is_err());
    }
}

#[test]
fn hardlink_aliases_and_future_lower_identity_survive_snapshot_roundtrip() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let lower = temp.path().join("lower");
    std::fs::create_dir(&lower).unwrap();
    std::fs::write(lower.join("file"), b"original").unwrap();
    for name in ["alias", "future"] {
        std::fs::hard_link(lower.join("file"), lower.join(name)).unwrap();
    }
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap().inode,
        inode
    );
    let handle = fs.open(ctx, inode, false, 2).unwrap().0.unwrap();
    fs.release(ctx, inode, 2, handle, false, false, None)
        .unwrap();
    let saved = fs.capture_state().unwrap();
    fs.restore_state(&saved).unwrap();
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"future").unwrap().inode,
        inode
    );
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap().inode,
        inode
    );
    let super::super::snapshot::FsSnapshot::Overlay(mut malformed) = saved else {
        panic!("overlay");
    };
    std::fs::write(lower.join("unrelated"), b"other").unwrap();
    malformed.node_aliases.push((inode, b"unrelated".to_vec()));
    assert!(fs
        .restore_state(&super::super::snapshot::FsSnapshot::Overlay(malformed))
        .is_err());
}

#[test]
fn failed_rename_pin_preserves_later_append_and_truncate_flags() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    std::fs::write(temp.path().join("upper/victim"), b"old").unwrap();
    let ctx = root_context();
    let inode = fs.lookup(ctx, fuse::ROOT_ID, c"victim").unwrap().inode;
    assert!(fs
        .rename(ctx, fuse::ROOT_ID, c"missing", fuse::ROOT_ID, c"victim", 0)
        .is_err());
    let flags = (1 | bindings::LINUX_O_APPEND) as u32;
    let handle = fs.open(ctx, inode, false, flags).unwrap().0.unwrap();
    fs.write(
        ctx,
        inode,
        handle,
        WriteReader(io::Cursor::new(b"!".to_vec())),
        1,
        3,
        None,
        false,
        false,
        flags,
    )
    .unwrap();
    fs.release(ctx, inode, flags, handle, false, false, None)
        .unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("upper/victim")).unwrap(),
        b"old!"
    );
    let flags = (1 | bindings::LINUX_O_TRUNC) as u32;
    let handle = fs.open(ctx, inode, false, flags).unwrap().0.unwrap();
    fs.release(ctx, inode, flags, handle, false, false, None)
        .unwrap();
    assert!(std::fs::read(temp.path().join("upper/victim"))
        .unwrap()
        .is_empty());
}


#[test]
fn name_only_readdir_is_visible_to_libc_without_preallocating_hardlink_inodes() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    std::fs::create_dir(&lower).unwrap();
    std::fs::write(lower.join("file"), b"data").unwrap();
    std::fs::hard_link(lower.join("file"), lower.join("alias")).unwrap();
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
    let read = |offset| {
        let mut entries = Vec::new();
        fs.readdir(ctx, fuse::ROOT_ID, handle, 4096, offset, |entry| {
            entries.push((entry.name.to_vec(), entry.ino, entry.offset, entry.type_));
            Ok(1)
        })
        .unwrap();
        entries
    };
    let entries = read(0);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.1 != 0), "libc drops zero-inode entries");
    assert_eq!(entries[0].0, b"alias");
    assert_eq!(entries[1].0, b"file");
    assert_eq!(read(entries[0].2), entries[1..]);
    let file = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
    let alias = fs.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap();
    assert_eq!(file.inode, alias.inode);
    assert_eq!(read(0), entries, "LOOKUP must not change the open directory snapshot");
    fs.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn rename_whiteout_leaves_a_char_zero_device_at_the_old_name() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    std::fs::write(temp.path().join("upper/source"), b"payload").unwrap();
    let ctx = root_context();
    // The guest kernel overlayfs mount probe issues exactly this rename.
    // Whiteout devices materialize as host-safe placeholder files (hosts
    // commonly deny mknod of char 0,0 to sandbox processes), so no
    // capability is required on the host any more.
    fs.rename(
        ctx,
        fuse::ROOT_ID,
        c"source",
        fuse::ROOT_ID,
        c"dest",
        bindings::LINUX_RENAME_WHITEOUT as u32,
    )
    .unwrap();
    // The renamed entry keeps its content.
    let dest = fs.lookup(ctx, fuse::ROOT_ID, c"dest").unwrap();
    assert_eq!(dest.attr.st_mode & S_IFMT, S_IFREG);
    let reader = fs.open(ctx, dest.inode, false, 0).unwrap().0.unwrap();
    assert_eq!(correctness_read(&fs, ctx, dest.inode, reader), b"payload");
    fs.release(ctx, dest.inode, 0, reader, false, false, None)
        .unwrap();
    // The old name is a whiteout: char device 0,0, visible to the guest and
    // stored as a placeholder regular file carrying the private record.
    let whiteout = fs.lookup(ctx, fuse::ROOT_ID, c"source").unwrap();
    assert_eq!(whiteout.attr.st_mode & S_IFMT, S_IFCHR);
    assert_eq!(whiteout.attr.st_rdev, 0);
    {
        use std::os::unix::fs::FileTypeExt;
        let metadata = std::fs::symlink_metadata(temp.path().join("upper/source")).unwrap();
        assert!(
            metadata.file_type().is_file(),
            "whiteout placeholder must be a host-safe regular file"
        );
        let record = pvisor_overlay_core::sys::get_xattr(
            &temp.path().join("upper/source"),
            std::ffi::OsStr::new(pvisor_overlay_core::VIRTUAL_NODE_XATTR),
        )
        .unwrap();
        assert_eq!(record, b"c:0:0");
    }
    // Name-only readdir reports the whiteout as DT_CHR, which is what the
    // kernel's d_type support probe depends on.
    let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
    let mut entries = Vec::new();
    fs.readdir(ctx, fuse::ROOT_ID, handle, 4096, 0, |entry| {
        entries.push((entry.name.to_vec(), entry.type_));
        Ok(1)
    })
    .unwrap();
    fs.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
    let whiteout_type = entries
        .iter()
        .find(|(name, _)| name == b"source")
        .map(|(_, kind)| *kind)
        .expect("whiteout must stay listed");
    assert_eq!(whiteout_type, libc::DT_CHR as u32);
}

#[test]
fn guest_overlay_xattrs_round_trip_through_the_stored_namespace() {
    use super::super::filesystem::{GetxattrReply, ListxattrReply};
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    std::fs::create_dir(temp.path().join("lower/lower-dir")).unwrap();
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    let dir = fs
        .mkdir(ctx, fuse::ROOT_ID, c"dir", 0o755, 0, Extensions::default())
        .unwrap();

    // setxattr translates the guest control name into the stored namespace.
    fs.setxattr(ctx, dir.inode, c"trusted.overlay.opaque", b"y", 0)
        .unwrap();
    match fs
        .getxattr(ctx, dir.inode, c"trusted.overlay.opaque", 64)
        .unwrap()
    {
        GetxattrReply::Value(value) => assert_eq!(value, b"y".to_vec()),
        GetxattrReply::Count(count) => panic!("unexpected count reply {count}"),
    }
    // The userxattr spelling reads the same stored name.
    match fs
        .getxattr(ctx, dir.inode, c"user.overlay.opaque", 64)
        .unwrap()
    {
        GetxattrReply::Value(value) => assert_eq!(value, b"y".to_vec()),
        GetxattrReply::Count(count) => panic!("unexpected count reply {count}"),
    }

    let upper_dir = temp.path().join("upper/dir");
    assert_eq!(
        pvisor_overlay_core::sys::get_xattr(
            &upper_dir,
            std::ffi::OsStr::new("user.pvisor.overlay.opaque")
        )
        .unwrap(),
        b"y".to_vec()
    );
    // Core's own opaque interpretation ignores the stored namespace.
    assert!(!pvisor_overlay_core::is_opaque_directory(&upper_dir));

    // listxattr reports guest spellings and hides the stored form.
    let names = match fs.listxattr(ctx, dir.inode, 512).unwrap() {
        ListxattrReply::Names(names) => names,
        ListxattrReply::Count(count) => panic!("unexpected count reply {count}"),
    };
    let names: Vec<&[u8]> = names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect();
    assert!(names.contains(&b"trusted.overlay.opaque".as_slice()));
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with(b"user.pvisor.overlay.")),
        "stored overlay-control names must not be listed: {names:?}"
    );

    // Direct stored-name access cannot forge or observe control state.
    assert_eq!(
        fs.setxattr(ctx, dir.inode, c"user.pvisor.overlay.opaque", b"n", 0)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EPERM)
    );
    assert!(
        fs.getxattr(ctx, dir.inode, c"user.pvisor.overlay.opaque", 64)
            .is_err()
    );
    assert_eq!(
        fs.removexattr(ctx, dir.inode, c"user.pvisor.overlay.opaque")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EPERM)
    );

    // Copy-up carries translated control state onto the upper entry.
    let lower_dir = fs.lookup(ctx, fuse::ROOT_ID, c"lower-dir").unwrap();
    fs.setxattr(ctx, lower_dir.inode, c"trusted.overlay.opaque", b"y", 0)
        .unwrap();
    assert_eq!(
        pvisor_overlay_core::sys::get_xattr(
            &temp.path().join("upper/lower-dir"),
            std::ffi::OsStr::new("user.pvisor.overlay.opaque")
        )
        .unwrap(),
        b"y".to_vec()
    );

    // removexattr removes through the guest spelling.
    fs.removexattr(ctx, dir.inode, c"trusted.overlay.opaque")
        .unwrap();
    assert!(
        fs.getxattr(ctx, dir.inode, c"trusted.overlay.opaque", 64)
            .is_err()
    );
}

#[test]
fn rename_rejects_exchange_combined_with_whiteout() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    std::fs::write(temp.path().join("upper/a"), b"a").unwrap();
    std::fs::write(temp.path().join("upper/b"), b"b").unwrap();
    let ctx = root_context();
    let flags = (bindings::LINUX_RENAME_EXCHANGE | bindings::LINUX_RENAME_WHITEOUT) as u32;
    assert_eq!(
        fs.rename(ctx, fuse::ROOT_ID, c"a", fuse::ROOT_ID, c"b", flags)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(std::fs::read(temp.path().join("upper/b")).unwrap(), b"b");
    assert_eq!(std::fs::read(temp.path().join("upper/a")).unwrap(), b"a");
}

#[cfg(target_os = "linux")]
#[test]
fn standalone_whiteout_nodes_are_visible_and_linkable() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    // vfs_whiteout() creates standalone whiteouts through MKNOD. The device
    // node is virtualized into a host-safe placeholder, so no host
    // capability is required.
    let whiteout = fs
        .mknod(
            ctx,
            fuse::ROOT_ID,
            c"whiteout",
            S_IFCHR | 0o600,
            0,
            0,
            Extensions::default(),
        )
        .unwrap();
    assert_eq!(whiteout.attr.st_mode & S_IFMT, S_IFCHR);
    assert_eq!(whiteout.attr.st_rdev, 0);
    // The kernel links one shared whiteout to every hidden name; the link
    // shares the placeholder inode, so the private record travels with it.
    let shared = fs.link(ctx, whiteout.inode, fuse::ROOT_ID, c"shared").unwrap();
    assert_eq!(shared.attr.st_mode & S_IFMT, S_IFCHR);
    let looked_up = fs.lookup(ctx, fuse::ROOT_ID, c"shared").unwrap();
    assert_eq!(looked_up.inode, whiteout.inode);
    assert_eq!(looked_up.attr.st_mode & S_IFMT, S_IFCHR);
    assert_eq!(looked_up.attr.st_rdev, 0);
}

#[test]
fn guest_whiteout_placeholders_project_across_every_reply_path() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("lower")).unwrap();
    let fs = correctness_fixture(temp.path());
    let ctx = root_context();
    // The tar/docker-store flow: create a whiteout, then verify it through
    // every observation path the guest uses.
    fs.mknod(
        ctx,
        fuse::ROOT_ID,
        c"wh-node",
        S_IFCHR | 0o644,
        0,
        0,
        Extensions::default(),
    )
    .unwrap();
    // Real device numbers still materialize as real host devices; this
    // genuinely requires CAP_MKNOD, so unprivileged dev machines skip only
    // this leg (the virtualized whiteout above needs no privileges).
    let real_device_supported = match fs.mknod(
        ctx,
        fuse::ROOT_ID,
        c"real-null",
        S_IFCHR | 0o644,
        libc::makedev(1, 3) as u32,
        0,
        Extensions::default(),
    ) {
        Ok(_) => true,
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => false,
        Err(error) => panic!("real device mknod failed unexpectedly: {error}"),
    };

    // lookup/getattr projection.
    let node = fs.lookup(ctx, fuse::ROOT_ID, c"wh-node").unwrap();
    assert_eq!(node.attr.st_mode & S_IFMT, S_IFCHR);
    assert_eq!(node.attr.st_rdev, 0);
    let (attr, _) = fs.getattr(ctx, node.inode, None).unwrap();
    assert_eq!(attr.st_mode & S_IFMT, S_IFCHR);
    assert_eq!(attr.st_rdev, 0);
    if real_device_supported {
        let real = fs.lookup(ctx, fuse::ROOT_ID, c"real-null").unwrap();
        assert_eq!(real.attr.st_rdev, libc::makedev(1, 3));
    }

    // readdirplus reports the projected type without forcing a lookup.
    let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
    let mut plus = Vec::new();
    fs.readdirplus(ctx, fuse::ROOT_ID, handle, 8192, 0, |entry, _attr| {
        plus.push((entry.name.to_vec(), entry.type_));
        Ok(1)
    })
    .unwrap();
    fs.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
    assert_eq!(
        plus.iter().find(|(name, _)| name == b"wh-node").unwrap().1,
        libc::DT_CHR as u32
    );
    if real_device_supported {
        assert_eq!(
            plus.iter().find(|(name, _)| name == b"real-null").unwrap().1,
            libc::DT_CHR as u32
        );
    }

    // The private record is invisible and immutable through guest xattrs.
    match fs.getxattr(ctx, node.inode, c"user.pvisor.virtual_node", 64) {
        Err(error) => assert_eq!(error.raw_os_error(), Some(libc::ENODATA)),
        Ok(_) => panic!("private virtual_node record must be hidden"),
    }
    assert!(fs
        .setxattr(
            ctx,
            node.inode,
            c"user.pvisor.virtual_node",
            b"c:1:5",
            0
        )
        .is_err());
    use super::super::filesystem::ListxattrReply;
    let listed = match fs.listxattr(ctx, node.inode, 4096).unwrap() {
        ListxattrReply::Names(names) => names,
        _ => panic!("expected names"),
    };
    assert!(!listed
        .windows(b"user.pvisor.virtual_node".len())
        .any(|w| w == b"user.pvisor.virtual_node"));

    // unlink removes the placeholder like any file.
    fs.unlink(ctx, fuse::ROOT_ID, c"wh-node").unwrap();
    assert!(fs.lookup(ctx, fuse::ROOT_ID, c"wh-node").is_err());
}
