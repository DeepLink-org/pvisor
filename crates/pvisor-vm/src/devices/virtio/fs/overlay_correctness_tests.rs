// Included in overlay::tests to exercise the private protocol adapter.

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
