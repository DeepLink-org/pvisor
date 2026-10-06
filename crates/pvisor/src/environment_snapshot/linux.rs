//! Linux metadata-preserving owned tree copies. POSIX ACLs travel as xattrs.
use super::{TreeEntry, TreeInventory, TreeObject, native_path, store};
use anyhow::{Context, ensure};
use std::{
    collections::BTreeMap,
    ffi::{CString, OsStr},
    fs, io,
    os::fd::AsRawFd,
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

pub(super) fn xattrs(path: &Path) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let path = native_path(path)?;
    let size = unsafe { libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
    ensure!(size >= 0, "listxattr: {}", io::Error::last_os_error());
    let mut names = vec![0u8; size as usize];
    let count = unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    ensure!(count == size, "extended attributes changed while listing");
    let mut result = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let native = CString::new(name)?;
        let size =
            unsafe { libc::lgetxattr(path.as_ptr(), native.as_ptr(), std::ptr::null_mut(), 0) };
        ensure!(size >= 0, "getxattr: {}", io::Error::last_os_error());
        let mut value = vec![0; size as usize];
        let count = unsafe {
            libc::lgetxattr(
                path.as_ptr(),
                native.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        ensure!(count == size, "extended attribute changed while reading");
        result.push((name.to_vec(), value));
    }
    result.sort();
    Ok(result)
}

pub(super) fn copy_entry(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> anyhow::Result<bool> {
    let mut cloned = false;
    if metadata.is_file() {
        let mut input = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        // FICLONE shares extents, not writable inode identity. Neither file
        // offset moves. Only capability/geometry failures permit fallback;
        // storage, permission and I/O failures must still abort publication.
        let rc = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
        cloned = rc == 0;
        if !cloned {
            let error = io::Error::last_os_error();
            ensure!(
                matches!(
                    error.raw_os_error(),
                    Some(
                        libc::EXDEV | libc::EOPNOTSUPP | libc::ENOTTY | libc::EINVAL | libc::ENOSYS
                    )
                ),
                "reflink {}: {error}",
                source.display()
            );
            output.set_len(0)?;
            io::copy(&mut input, &mut output)?;
        }
    } else if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    }
    let dst = native_path(destination)?;
    let current = fs::symlink_metadata(destination)?;
    if current.uid() != metadata.uid() || current.gid() != metadata.gid() {
        ensure!(
            unsafe { libc::lchown(dst.as_ptr(), metadata.uid(), metadata.gid()) } == 0,
            "copy ownership: {}",
            io::Error::last_os_error()
        );
    }
    if !metadata.file_type().is_symlink() {
        // Metadata is installed on a private copy. User xattrs require write
        // permission, so defer the captured read-only mode until after xattrs.
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(if metadata.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    let attributes = xattrs(source)?;
    // Installing an access ACL can itself remove owner write permission.
    // Install it last so read-only ACLs do not prevent other xattr writes.
    for (name, value) in attributes
        .iter()
        .filter(|(name, _)| name != b"system.posix_acl_access")
        .chain(
            attributes
                .iter()
                .filter(|(name, _)| name == b"system.posix_acl_access"),
        )
    {
        let name = CString::new(name.as_slice())?;
        ensure!(
            unsafe {
                libc::lsetxattr(
                    dst.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } == 0,
            "copy xattr: {}",
            io::Error::last_os_error()
        );
    }
    if !metadata.file_type().is_symlink() {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
    }
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    ensure!(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                dst.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "copy timestamps: {}",
        io::Error::last_os_error()
    );
    Ok(cloned)
}

const MAX_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn relative(bytes: &[u8]) -> anyhow::Result<PathBuf> {
    ensure!(
        bytes.len() <= 4096 && !bytes.contains(&0),
        "invalid checkpoint tree path"
    );
    let path = PathBuf::from(OsStr::from_bytes(bytes));
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_)))
            && path.as_os_str().as_bytes() == bytes
            && !bytes
                .split(|byte| *byte == b'/')
                .any(|part| part == b"." || part == b".." || part.is_empty()),
        "checkpoint tree path must be canonical and relative"
    );
    Ok(path)
}
pub(super) fn validate_tree_metadata(tree: &TreeInventory) -> anyhow::Result<()> {
    ensure!(
        tree.version == 1 && !tree.entries.is_empty(),
        "invalid checkpoint tree inventory"
    );
    let mut previous = BTreeMap::<PathBuf, &TreeEntry>::new();
    for (index, entry) in tree.entries.iter().enumerate() {
        let path = if index == 0 {
            ensure!(
                entry.path.is_empty() && matches!(entry.object, TreeObject::Directory),
                "missing checkpoint tree root"
            );
            PathBuf::new()
        } else {
            let path = relative(&entry.path)?;
            ensure!(
                previous
                    .get(path.parent().context("missing checkpoint parent")?)
                    .is_some_and(|entry| matches!(entry.object, TreeObject::Directory)),
                "checkpoint parent must be an earlier directory"
            );
            path
        };
        ensure!(
            entry.acl.is_none() && (0..1_000_000_000).contains(&entry.mtime_nsec),
            "invalid Linux checkpoint metadata"
        );
        let kind = entry.mode & libc::S_IFMT;
        match &entry.object {
            TreeObject::Directory => {
                ensure!(kind == libc::S_IFDIR, "checkpoint directory type mismatch")
            }
            TreeObject::File {
                bytes,
                sha256,
                hardlink,
            } => {
                ensure!(
                    kind == libc::S_IFREG && *bytes <= MAX_BYTES,
                    "checkpoint file type/size mismatch"
                );
                store::valid_id(sha256)?;
                validate_link(entry, hardlink, &previous)?;
            }
            TreeObject::Symlink { target, hardlink } => {
                ensure!(
                    kind == libc::S_IFLNK
                        && !target.is_empty()
                        && target.len() <= 4096
                        && !target.contains(&0),
                    "invalid checkpoint symlink"
                );
                validate_link(entry, hardlink, &previous)?;
            }
        }
        ensure!(
            previous.insert(path, entry).is_none(),
            "duplicate checkpoint tree path"
        );
    }
    Ok(())
}
fn validate_link(
    entry: &TreeEntry,
    origin: &[u8],
    previous: &BTreeMap<PathBuf, &TreeEntry>,
) -> anyhow::Result<()> {
    let path = relative(origin)?;
    if origin != entry.path {
        let mut canonical = (*previous
            .get(&path)
            .context("missing earlier checkpoint hardlink origin")?)
        .clone();
        canonical.path = entry.path.clone();
        ensure!(canonical == *entry, "checkpoint hardlink metadata mismatch");
    }
    Ok(())
}
/// Apply a verified entry only to a caller-owned, unpublished staging object.
pub(super) fn restore_metadata(path: &Path, entry: &TreeEntry) -> anyhow::Result<()> {
    let native = native_path(path)?;
    let current = fs::symlink_metadata(path)?;
    if current.uid() != entry.uid || current.gid() != entry.gid {
        ensure!(
            unsafe { libc::lchown(native.as_ptr(), entry.uid, entry.gid) } == 0,
            "restore checkpoint ownership: {}",
            std::io::Error::last_os_error()
        );
    }
    if !matches!(entry.object, TreeObject::Symlink { .. }) {
        // A preceding alias may already have restored this private inode's
        // read-only mode. Xattrs still need write permission; all hard links
        // are closed within staging, and final mode is restored below.
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if current.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    for (name, _) in xattrs(path)? {
        if !entry.xattrs.iter().any(|(expected, _)| *expected == name) {
            let name = CString::new(name)?;
            ensure!(
                unsafe { libc::lremovexattr(native.as_ptr(), name.as_ptr()) } == 0,
                "remove inherited checkpoint xattr: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    // Access ACL restoration may tighten mode before chmod; apply it after
    // every other xattr, including when visiting a read-only hard-link alias.
    for (name, value) in entry
        .xattrs
        .iter()
        .filter(|(name, _)| name != b"system.posix_acl_access")
        .chain(
            entry
                .xattrs
                .iter()
                .filter(|(name, _)| name == b"system.posix_acl_access"),
        )
    {
        let name = CString::new(name.as_slice())?;
        ensure!(
            unsafe {
                libc::lsetxattr(
                    native.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } == 0,
            "restore checkpoint xattr: {}",
            std::io::Error::last_os_error()
        );
    }
    if !matches!(entry.object, TreeObject::Symlink { .. }) {
        fs::set_permissions(path, fs::Permissions::from_mode(entry.mode & 0o7777))?;
    }
    let times = [libc::timespec {
        tv_sec: entry.mtime,
        tv_nsec: entry.mtime_nsec,
    }; 2];
    ensure!(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                native.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "restore checkpoint timestamps: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
