//! Linux metadata-preserving owned tree copies. POSIX ACLs travel as xattrs.
use super::native_path;
use anyhow::ensure;
use std::{
    ffi::CString,
    fs, io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
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
) -> anyhow::Result<()> {
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
        io::copy(&mut input, &mut output)?;
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
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
    }
    for (name, value) in xattrs(source)? {
        let name = CString::new(name)?;
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
    Ok(())
}
