//! Owned filesystem copies for the same-host environment snapshot coordinator.
//!
//! Callers must stop guest/device writes and own the entire source tree for the
//! duration of copying. This module does not freeze VMs or publish snapshots.
//! Every restore gets a new writable copy; sealed backing is never guest-writable.
use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as _;
#[cfg(target_os = "linux")]
mod linux;
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

mod blocks;
mod store;
pub use blocks::RamBlocks;
pub use store::{
    Compatibility, EnvironmentManifest, PendingEnvironment, PublishedEnvironment, SnapshotStore,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeInventory {
    pub version: u32,
    pub entries: Vec<TreeEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub path: Vec<u8>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub object: TreeObject,
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
    pub acl: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TreeObject {
    Directory,
    File {
        bytes: u64,
        sha256: String,
        hardlink: Vec<u8>,
    },
    Symlink {
        target: Vec<u8>,
        hardlink: Vec<u8>,
    },
}

pub fn file_hash(path: &Path) -> anyhow::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn native_path(path: &Path) -> anyhow::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

#[cfg(target_os = "macos")]
fn xattrs(path: &Path) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let path = native_path(path)?;
    let size =
        unsafe { libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    ensure!(size >= 0, "listxattr: {}", std::io::Error::last_os_error());
    let mut names = vec![0u8; size as usize];
    let count = unsafe {
        libc::listxattr(
            path.as_ptr(),
            names.as_mut_ptr().cast(),
            names.len(),
            libc::XATTR_NOFOLLOW,
        )
    };
    ensure!(count == size, "extended attributes changed while listing");
    let mut result = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let native = CString::new(name)?;
        let size = unsafe {
            libc::getxattr(
                path.as_ptr(),
                native.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        ensure!(size >= 0, "getxattr: {}", std::io::Error::last_os_error());
        let mut value = vec![0; size as usize];
        let count = unsafe {
            libc::getxattr(
                path.as_ptr(),
                native.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        ensure!(count == size, "extended attribute changed while reading");
        result.push((name.to_vec(), value));
    }
    result.sort();
    Ok(result)
}

#[cfg(target_os = "linux")]
use linux::xattrs;
#[cfg(target_os = "linux")]
fn acl(_path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    // Linux POSIX ACLs are included in the system.posix_acl_* xattrs.
    Ok(None)
}

// Darwin sys/acl.h: ACL_TYPE_EXTENDED = 0x100. These link-specific calls do
// not follow a symlink outside the owned tree. libc does not expose them.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
    fn acl_to_text(acl: *mut libc::c_void, length: *mut libc::ssize_t) -> *mut libc::c_char;
    fn acl_free(pointer: *mut libc::c_void) -> libc::c_int;
}
#[cfg(target_os = "macos")]
fn acl(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let path = native_path(path)?;
    let object = unsafe { acl_get_link_np(path.as_ptr(), 0x100) };
    if object.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(error.into());
    }
    let mut length = 0;
    let text = unsafe { acl_to_text(object, &mut length) };
    let error = std::io::Error::last_os_error();
    let result = if text.is_null() || length < 0 {
        Err(error.into())
    } else {
        Ok(Some(
            unsafe { std::slice::from_raw_parts(text.cast(), length as usize) }.to_vec(),
        ))
    };
    unsafe {
        if !text.is_null() {
            acl_free(text.cast());
        }
        acl_free(object);
    }
    result
}

/// Full inventory includes unvisited files, hardlink topology, ACLs and xattrs.
/// External hardlinks and special objects are rejected, not silently omitted.
pub fn inventory(root: &Path) -> anyhow::Result<TreeInventory> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "tree root must be a directory"
    );
    let mut entries = Vec::new();
    let mut links: BTreeMap<(u64, u64), (Vec<u8>, u64, u64)> = BTreeMap::new();
    fn visit(
        root: &Path,
        path: &Path,
        entries: &mut Vec<TreeEntry>,
        links: &mut BTreeMap<(u64, u64), (Vec<u8>, u64, u64)>,
    ) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(target_os = "macos")]
        ensure!(
            metadata.st_flags() == 0,
            "unsupported BSD file flags at {}",
            path.display()
        );
        let relative = path.strip_prefix(root)?.as_os_str().as_bytes().to_vec();
        let object = if metadata.is_dir() {
            TreeObject::Directory
        } else {
            let link = links.entry((metadata.dev(), metadata.ino())).or_insert((
                relative.clone(),
                0,
                metadata.nlink(),
            ));
            link.1 += 1;
            if metadata.is_file() {
                TreeObject::File {
                    bytes: metadata.len(),
                    sha256: file_hash(path)?,
                    hardlink: link.0.clone(),
                }
            } else if metadata.file_type().is_symlink() {
                TreeObject::Symlink {
                    target: fs::read_link(path)?.as_os_str().as_bytes().to_vec(),
                    hardlink: link.0.clone(),
                }
            } else {
                bail!("unsupported filesystem object at {}", path.display());
            }
        };
        entries.push(TreeEntry {
            path: relative,
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            object,
            xattrs: xattrs(path)?,
            acl: acl(path)?,
        });
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                visit(root, &child.path(), entries, links)?;
            }
        }
        let after = fs::symlink_metadata(path)?;
        ensure!(
            metadata.dev() == after.dev()
                && metadata.ino() == after.ino()
                && metadata.ctime() == after.ctime()
                && metadata.ctime_nsec() == after.ctime_nsec()
                && metadata.mtime() == after.mtime()
                && metadata.mtime_nsec() == after.mtime_nsec()
                && metadata.len() == after.len(),
            "source changed during inventory at {}",
            path.display()
        );
        Ok(())
    }
    visit(root, root, &mut entries, &mut links)?;
    ensure!(
        links.values().all(|(_, count, expected)| count == expected),
        "hardlink escapes owned tree"
    );
    Ok(TreeInventory {
        version: 1,
        entries,
    })
}

pub fn verify_tree(root: &Path, expected: &TreeInventory) -> anyhow::Result<()> {
    ensure!(
        expected.version == 1 && inventory(root)? == *expected,
        "filesystem inventory mismatch"
    );
    Ok(())
}

/// Full data copy, never clonefile. Native metadata copying preserves ACLs,
/// xattrs, permissions and timestamps; our traversal preserves hardlinks.
/// Only the caller's unpublished destination is removed on any failure.
pub fn copy_owned_tree(source: &Path, destination: &Path) -> anyhow::Result<TreeInventory> {
    ensure!(
        fs::symlink_metadata(source)?.is_dir(),
        "source root must not be a symlink"
    );
    let source = source.canonicalize()?;
    let parent = destination
        .parent()
        .context("missing destination parent")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&source),
        "destination lies inside source tree"
    );
    let before = inventory(&source)?;
    fs::create_dir(destination)?;
    let result = (|| {
        fn copy(
            source: &Path,
            destination: &Path,
            links: &mut BTreeMap<(u64, u64), std::path::PathBuf>,
        ) -> anyhow::Result<()> {
            let metadata = fs::symlink_metadata(source)?;
            if metadata.is_dir() {
                for child in fs::read_dir(source)? {
                    let child = child?;
                    let target = destination.join(child.file_name());
                    if fs::symlink_metadata(child.path())?.is_dir() {
                        fs::create_dir(&target)?;
                    }
                    copy(&child.path(), &target, links)?;
                }
            } else if let Some(first) = links.get(&(metadata.dev(), metadata.ino())) {
                fs::hard_link(first, destination)?;
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            {
                let flags = libc::COPYFILE_METADATA
                    | libc::COPYFILE_NOFOLLOW
                    | if metadata.is_dir() {
                        0
                    } else {
                        libc::COPYFILE_DATA | libc::COPYFILE_EXCL
                    };
                let src = native_path(source)?;
                let dst = native_path(destination)?;
                let rc = unsafe {
                    libc::copyfile(src.as_ptr(), dst.as_ptr(), std::ptr::null_mut(), flags)
                };
                ensure!(
                    rc == 0,
                    "copyfile {}: {}",
                    source.display(),
                    std::io::Error::last_os_error()
                );
            }
            #[cfg(target_os = "linux")]
            linux::copy_entry(source, destination, &metadata)?;
            if !metadata.is_dir() {
                links.insert((metadata.dev(), metadata.ino()), destination.to_owned());
            }
            if !metadata.file_type().is_symlink() {
                File::open(destination)?.sync_all()?;
            }
            Ok(())
        }
        copy(&source, destination, &mut BTreeMap::new())?;
        verify_tree(&source, &before).context("source changed during copy")?;
        verify_tree(destination, &before).context("copied tree does not match source")?;
        File::open(&parent)?.sync_all()?;
        Ok(before)
    })();
    if result.is_err() {
        fs::remove_dir_all(destination).context("failed to remove incomplete tree")?;
    }
    result
}
