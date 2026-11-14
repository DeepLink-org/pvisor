use super::super::super::filesystem::root_context;
use super::*;
use std::path::Path;

fn mount(root: &Path) -> PassthroughFs {
    let fs = PassthroughFs::new(
        Config {
            root_dir: root.to_string_lossy().into_owned(),
            managed: true,
            attr_timeout: Duration::ZERO,
            entry_timeout: Duration::ZERO,
            ..Default::default()
        },
        Arc::new(InodeAllocator::new()),
    )
    .unwrap();
    fs.init(FsOptions::empty()).unwrap();
    fs
}

fn owner(entry: &Entry) -> (u32, u32) {
    (entry.attr.st_uid, entry.attr.st_gid)
}

#[test]
fn managed_symlink_owners_survive_rename_hardlink_reopen_and_unlink() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let before = outside.as_file().metadata().unwrap();
    let fs = mount(root.path());
    let ctx = root_context();
    let mut user = ctx;
    user.uid = 1234;
    user.gid = 2345;
    let target = CString::new(outside.path().as_os_str().as_encoded_bytes()).unwrap();
    let entry = fs
        .symlink(user, &target, fuse::ROOT_ID, c"link", Default::default())
        .unwrap();
    assert_eq!(owner(&entry), (1234, 2345));
    assert_eq!(entry.attr.st_mode & 0o7777, 0o777);
    let mut attr = entry.attr;
    attr.st_uid = 33;
    attr.st_gid = 44;
    assert_eq!(
        fs.setattr(user, entry.inode, attr, None, SetattrValid::UID)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EPERM)
    );
    fs.setattr(
        ctx,
        entry.inode,
        attr,
        None,
        SetattrValid::UID | SetattrValid::GID,
    )
    .unwrap();
    fs.rename(ctx, fuse::ROOT_ID, c"link", fuse::ROOT_ID, c"renamed", 0)
        .unwrap();
    let alias = fs.link(ctx, entry.inode, fuse::ROOT_ID, c"alias").unwrap();
    assert_eq!(owner(&alias), (33, 44));
    assert_eq!(fs.readlink(ctx, alias.inode).unwrap(), target.to_bytes());
    let second = mount(root.path());
    assert_eq!(
        owner(&second.lookup(ctx, fuse::ROOT_ID, c"renamed").unwrap()),
        (33, 44)
    );
    // Updating via another VM and alias must retain the other identity field.
    let mut attr = alias.attr;
    attr.st_uid = 55;
    second
        .setattr(
            ctx,
            second.lookup(ctx, fuse::ROOT_ID, c"alias").unwrap().inode,
            attr,
            None,
            SetattrValid::UID,
        )
        .unwrap();
    let attr = fs.getattr(ctx, entry.inode, None).unwrap().0;
    assert_eq!((attr.st_uid, attr.st_gid), (55, 44));
    fs.unlink(ctx, fuse::ROOT_ID, c"renamed").unwrap();
    fs.unlink(ctx, fuse::ROOT_ID, c"alias").unwrap();
    assert_eq!(fs.getattr(ctx, entry.inode, None).unwrap().0.st_uid, 55);
    let fresh = fs
        .symlink(
            ctx,
            c"missing",
            fuse::ROOT_ID,
            c"renamed",
            Default::default(),
        )
        .unwrap();
    assert_eq!(owner(&fresh), (0, 0));
    // Neither physical host ownership nor the target is changed.
    let after = outside.as_file().metadata().unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.mode(), after.mtime()),
        (before.uid(), before.gid(), before.mode(), before.mtime())
    );
    assert_eq!(
        std::fs::symlink_metadata(root.path().join("renamed"))
            .unwrap()
            .uid(),
        unsafe { libc::geteuid() }
    );
}

#[test]
fn managed_symlink_concurrent_owner_updates_do_not_lose_fields() {
    let root = tempfile::tempdir().unwrap();
    let first = mount(root.path());
    let second = mount(root.path());
    let ctx = root_context();
    let entry = first
        .symlink(ctx, c"missing", fuse::ROOT_ID, c"link", Default::default())
        .unwrap();
    let other = second.lookup(ctx, fuse::ROOT_ID, c"link").unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for uid in 1..=32 {
                let mut attr = entry.attr;
                attr.st_uid = uid;
                first
                    .setattr(ctx, entry.inode, attr, None, SetattrValid::UID)
                    .unwrap();
            }
        });
        scope.spawn(|| {
            barrier.wait();
            for gid in 101..=132 {
                let mut attr = other.attr;
                attr.st_gid = gid;
                second
                    .setattr(ctx, other.inode, attr, None, SetattrValid::GID)
                    .unwrap();
            }
        });
    });
    let attr = first.getattr(ctx, entry.inode, None).unwrap().0;
    assert_eq!((attr.st_uid, attr.st_gid), (32, 132));
}

#[test]
fn managed_dangling_symlink_inherits_setgid_and_updates_its_own_times() {
    let root = tempfile::tempdir().unwrap();
    let fs = mount(root.path());
    let ctx = root_context();
    let mut root_attr = fs.getattr(ctx, fuse::ROOT_ID, None).unwrap().0;
    root_attr.st_gid = 42;
    root_attr.st_mode |= libc::S_ISGID;
    fs.setattr(
        ctx,
        fuse::ROOT_ID,
        root_attr,
        None,
        SetattrValid::GID | SetattrValid::MODE,
    )
    .unwrap();
    let mut user = ctx;
    user.uid = 33;
    user.gid = 44;
    let link = fs
        .symlink(user, c"missing", fuse::ROOT_ID, c"link", Default::default())
        .unwrap();
    assert_eq!(owner(&link), (33, 42));
    let mut attr = link.attr;
    attr.st_mtime = 12345;
    attr.st_mtime_nsec = 0;
    fs.setattr(user, link.inode, attr, None, SetattrValid::MTIME)
        .unwrap();
    assert_eq!(
        std::fs::symlink_metadata(root.path().join("link"))
            .unwrap()
            .mtime(),
        12345
    );
    assert!(!root.path().join("missing").exists());
}

#[test]
fn managed_catalog_is_hidden_and_every_namespace_mutation_is_denied() {
    let root = tempfile::tempdir().unwrap();
    let fs = mount(root.path());
    let ctx = root_context();
    let entry = fs
        .symlink(
            ctx,
            c"missing",
            fuse::ROOT_ID,
            c"visible",
            Default::default(),
        )
        .unwrap();
    let name = managed::DIRECTORY;
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"..").unwrap().inode,
        fuse::ROOT_ID
    );
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c".pvisor-managed/record")
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, name)
            .err()
            .unwrap()
            .raw_os_error(),
        Some(libc::ENOENT)
    );
    let errors = [
        fs.mkdir(ctx, fuse::ROOT_ID, name, 0o700, 0, Default::default())
            .err()
            .unwrap(),
        fs.create(
            ctx,
            fuse::ROOT_ID,
            name,
            0o600,
            false,
            libc::O_RDWR as u32,
            0,
            Default::default(),
        )
        .err()
        .unwrap(),
        fs.mknod(
            ctx,
            fuse::ROOT_ID,
            name,
            libc::S_IFREG | 0o600,
            0,
            0,
            Default::default(),
        )
        .err()
        .unwrap(),
        fs.symlink(ctx, c"missing", fuse::ROOT_ID, name, Default::default())
            .err()
            .unwrap(),
        fs.link(ctx, entry.inode, fuse::ROOT_ID, name)
            .err()
            .unwrap(),
        fs.rename(ctx, fuse::ROOT_ID, c"visible", fuse::ROOT_ID, name, 0)
            .unwrap_err(),
        fs.rename(ctx, fuse::ROOT_ID, name, fuse::ROOT_ID, c"stolen", 0)
            .unwrap_err(),
        fs.unlink(ctx, fuse::ROOT_ID, name).unwrap_err(),
        fs.rmdir(ctx, fuse::ROOT_ID, name).unwrap_err(),
    ];
    assert!(
        errors
            .iter()
            .all(|error| error.raw_os_error() == Some(libc::EPERM))
    );
    for plus in [false, true] {
        let (handle, _) = fs
            .opendir(ctx, fuse::ROOT_ID, libc::O_RDONLY as u32)
            .unwrap();
        let handle = handle.unwrap();
        let mut names = Vec::new();
        if plus {
            fs.readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 0, |dir, _entry| {
                names.push(dir.name.to_vec());
                Ok(1)
            })
            .unwrap();
        } else {
            fs.readdir(ctx, fuse::ROOT_ID, handle, 4096, 0, |dir| {
                names.push(dir.name.to_vec());
                Ok(1)
            })
            .unwrap();
        }
        assert_eq!(names, [b"visible".to_vec()]);
        fs.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
    }
    // The same spelling below the root is ordinary guest data.
    let subdir = fs
        .mkdir(ctx, fuse::ROOT_ID, c"subdir", 0o700, 0, Default::default())
        .unwrap();
    fs.mkdir(ctx, subdir.inode, name, 0o700, 0, Default::default())
        .unwrap();
}

#[test]
fn managed_catalog_collision_and_corruption_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".pvisor-managed");
    std::fs::create_dir(&directory).unwrap();
    let cfg = Config {
        root_dir: root.path().to_string_lossy().into_owned(),
        managed: true,
        ..Default::default()
    };
    assert!(PassthroughFs::new(cfg, Arc::new(InodeAllocator::new())).is_err());
    std::fs::remove_dir(&directory).unwrap();
    let fs = mount(root.path());
    let ctx = root_context();
    fs.symlink(ctx, c"missing", fuse::ROOT_ID, c"link", Default::default())
        .unwrap();
    let record = std::fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&record, b"corrupt").unwrap();
    assert_eq!(
        fs.lookup(ctx, fuse::ROOT_ID, c"link").err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    std::fs::remove_file(&record).unwrap();
    // A symlink in the private catalog must not redirect reads/writes.
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), &record).unwrap();
    assert!(fs.lookup(ctx, fuse::ROOT_ID, c"link").is_err());
    assert_eq!(outside.as_file().metadata().unwrap().len(), 0);
}

#[test]
fn managed_legacy_symlink_records_keep_their_native_mode() {
    let root = tempfile::tempdir().unwrap();
    let fs = mount(root.path());
    let ctx = root_context();
    fs.symlink(ctx, c"missing", fuse::ROOT_ID, c"link", Default::default())
        .unwrap();
    let directory = root.path().join(".pvisor-managed");
    let record = std::fs::read_dir(&directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(record, b"123:456").unwrap();
    let second = mount(root.path());
    let link = second.lookup(ctx, fuse::ROOT_ID, c"link").unwrap();
    assert_eq!(owner(&link), (123, 456));
    assert_eq!(link.attr.st_mode, libc::S_IFLNK | 0o777);
}
