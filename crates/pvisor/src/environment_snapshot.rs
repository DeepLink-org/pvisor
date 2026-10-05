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

mod base;
pub use base::{BaseReference, SnapshotBase};
mod blocks;
mod lazy;
mod store;
pub use blocks::RamBlocks;
pub(crate) use lazy::watch_mount;
pub use lazy::{RawRamIndex, SnapshotRamMount, SnapshotRamReader};
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileVersion {
    ctime: i64,
    ctime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    bytes: u64,
}
impl FileVersion {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            bytes: metadata.len(),
        }
    }
}
struct CachedFileFingerprint {
    version: FileVersion,
    sha256: String,
}
struct InventoryLink {
    first_path: Vec<u8>,
    seen: u64,
    expected_links: u64,
    // Box only shared files, keeping unique-file topology entries small.
    content: Option<Box<CachedFileFingerprint>>,
}

/// Full inventory includes unvisited files, hardlink topology, ACLs and xattrs.
/// External hardlinks and special objects are rejected, not silently omitted.
/// Shared regular-file content is scanned once per inode within this walk.
/// Callers must keep the tree quiescent; version checks do not make the walk
/// atomic and are not a content cache across requests.
pub fn inventory(root: &Path) -> anyhow::Result<TreeInventory> {
    inventory_with_hash::<true>(root, &mut file_hash)
}

fn inventory_with_hash<const DEDUPLICATE: bool>(
    root: &Path,
    hash: &mut impl FnMut(&Path) -> anyhow::Result<String>,
) -> anyhow::Result<TreeInventory> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "tree root must be a directory"
    );
    let mut entries = Vec::new();
    let mut links: BTreeMap<(u64, u64), InventoryLink> = BTreeMap::new();
    fn visit<const DEDUPLICATE: bool>(
        root: &Path,
        path: &Path,
        entries: &mut Vec<TreeEntry>,
        links: &mut BTreeMap<(u64, u64), InventoryLink>,
        hash: &mut impl FnMut(&Path) -> anyhow::Result<String>,
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
            let link = links
                .entry((metadata.dev(), metadata.ino()))
                .or_insert_with(|| InventoryLink {
                    first_path: relative.clone(),
                    seen: 0,
                    expected_links: metadata.nlink(),
                    content: None,
                });
            link.seen += 1;
            if metadata.is_file() {
                let version = FileVersion::of(&metadata);
                let sha256 = if let Some(content) = &link.content {
                    ensure!(
                        content.version == version,
                        "hardlinked file changed during inventory at {}",
                        path.display()
                    );
                    content.sha256.clone()
                } else {
                    let sha256 = hash(path)?;
                    if DEDUPLICATE && metadata.nlink() > 1 {
                        link.content = Some(Box::new(CachedFileFingerprint {
                            version,
                            sha256: sha256.clone(),
                        }));
                    }
                    sha256
                };
                TreeObject::File {
                    bytes: metadata.len(),
                    sha256,
                    hardlink: link.first_path.clone(),
                }
            } else if metadata.file_type().is_symlink() {
                TreeObject::Symlink {
                    target: fs::read_link(path)?.as_os_str().as_bytes().to_vec(),
                    hardlink: link.first_path.clone(),
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
                visit::<DEDUPLICATE>(root, &child.path(), entries, links, hash)?;
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
    visit::<DEDUPLICATE>(root, root, &mut entries, &mut links, hash)?;
    ensure!(
        links.values().all(|link| link.seen == link.expected_links),
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

/// Independent owned copy, using filesystem COW for regular files when available
/// and falling back to data copying. Native metadata copying preserves ACLs,
/// xattrs, permissions and timestamps; our traversal preserves hardlinks within
/// the destination, never between source and destination. Full inventories still
/// validate the source twice and destination metadata. When every regular file
/// is confirmed cloned by the kernel, destination content inherits the source
/// digest; any data-copy fallback retains full destination content verification.
/// The owned, quiescent source and unpublished destination contract is required.
/// Only the caller's unpublished destination is removed on any failure.
pub fn copy_owned_tree(source: &Path, destination: &Path) -> anyhow::Result<TreeInventory> {
    copy_owned_tree_checked(source, destination, None)
}

/// Reuse the source inventory pass to validate a saved manifest before creating
/// the destination. Post-copy source and destination verification stays intact.
fn copy_owned_tree_checked(
    source: &Path,
    destination: &Path,
    expected: Option<&TreeInventory>,
) -> anyhow::Result<TreeInventory> {
    copy_tree::<false>(source, destination, expected)
}

/// Only a verified PublishedEnvironment may use this path, while retaining its
/// lease. Its sealed payload must remain immutable, just like SnapshotBase.
fn copy_sealed_tree(
    source: &Path,
    destination: &Path,
    expected: &TreeInventory,
) -> anyhow::Result<TreeInventory> {
    copy_tree::<true>(source, destination, Some(expected))
}

fn copy_tree<const SEALED: bool>(
    source: &Path,
    destination: &Path,
    expected: Option<&TreeInventory>,
) -> anyhow::Result<TreeInventory> {
    let profile = pvisor_overlay_core::profile::Profile::from_env(if SEALED {
        "sealed-stage-copy"
    } else {
        "owned-tree-copy"
    });
    let _total = profile.span("total");
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
    let before_span = profile.span("source_before");
    let before = if SEALED {
        let expected = expected.context("sealed copy requires verified inventory")?;
        // The PublishedEnvironment borrow retains the lease for this owned
        // immutable source. Reuse its checked inventory instead of repeating
        // the complete metadata walk immediately before copying. Source-after
        // and destination checks below still reject mismatches and clean up.
        expected.clone()
    } else {
        inventory(&source)?
    };
    if let Some(expected) = expected {
        ensure!(before == *expected, "filesystem inventory mismatch");
    }
    drop(before_span);
    fs::create_dir(destination)?;
    let result = (|| {
        fn copy(
            source: &Path,
            destination: &Path,
            metadata: &fs::Metadata,
            links: &mut BTreeMap<(u64, u64), std::path::PathBuf>,
            content_cloned: &mut bool,
            profile: &pvisor_overlay_core::profile::Profile,
        ) -> anyhow::Result<()> {
            if metadata.is_dir() {
                for child in fs::read_dir(source)? {
                    let child = child?;
                    let source = child.path();
                    let target = destination.join(child.file_name());
                    let entry_span = profile.span("entry_metadata");
                    let metadata = fs::symlink_metadata(&source)?;
                    drop(entry_span);
                    if metadata.is_dir() {
                        fs::create_dir(&target)?;
                    }
                    copy(&source, &target, &metadata, links, content_cloned, profile)?;
                }
            } else if let Some(first) = links.get(&(metadata.dev(), metadata.ino())) {
                let _span = profile.span("hardlink");
                fs::hard_link(first, destination)?;
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            {
                let mut flags = libc::COPYFILE_METADATA
                    | libc::COPYFILE_NOFOLLOW
                    | if metadata.is_dir() {
                        0
                    } else {
                        libc::COPYFILE_DATA | libc::COPYFILE_EXCL
                    };
                if metadata.is_file() {
                    // COPYFILE_CLONE falls back to copying when cloning is not
                    // supported. Keep ACL and no-follow flags on both paths.
                    flags |= libc::COPYFILE_CLONE;
                }
                let src = native_path(source)?;
                let dst = native_path(destination)?;
                struct CopyState(libc::copyfile_state_t);
                impl Drop for CopyState {
                    fn drop(&mut self) {
                        unsafe { libc::copyfile_state_free(self.0) };
                    }
                }
                let state = CopyState(unsafe { libc::copyfile_state_alloc() });
                ensure!(!state.0.is_null(), "allocate copyfile state");
                let native_span = profile.span("native_copy");
                let rc = unsafe { libc::copyfile(src.as_ptr(), dst.as_ptr(), state.0, flags) };
                drop(native_span);
                ensure!(
                    rc == 0,
                    "copyfile {}: {}",
                    source.display(),
                    std::io::Error::last_os_error()
                );
                if metadata.is_file() {
                    let mut cloned = false;
                    ensure!(
                        unsafe {
                            libc::copyfile_state_get(
                                state.0,
                                libc::COPYFILE_STATE_WAS_CLONED as u32,
                                (&raw mut cloned).cast(),
                            )
                        } == 0,
                        "query copyfile clone result: {}",
                        std::io::Error::last_os_error()
                    );
                    *content_cloned &= cloned;
                    profile.add(
                        if cloned {
                            "cloned_files"
                        } else {
                            "copied_files"
                        },
                        1,
                    );
                    // The first copy already includes STAT/XATTR/ACL. Only
                    // privileged bits omitted by cloning need compensation.
                    if metadata.mode() & 0o6000 != 0 {
                        let metadata_span = profile.span("native_metadata_reapply");
                        let rc = unsafe {
                            libc::copyfile(
                                src.as_ptr(),
                                dst.as_ptr(),
                                std::ptr::null_mut(),
                                libc::COPYFILE_METADATA | libc::COPYFILE_NOFOLLOW,
                            )
                        };
                        drop(metadata_span);
                        ensure!(
                            rc == 0,
                            "copy cloned metadata {}: {}",
                            source.display(),
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
            #[cfg(target_os = "linux")]
            {
                let _span = profile.span("native_copy");
                let cloned = linux::copy_entry(source, destination, metadata)?;
                if metadata.is_file() {
                    *content_cloned &= cloned;
                    profile.add(
                        if cloned {
                            "cloned_files"
                        } else {
                            "copied_files"
                        },
                        1,
                    );
                }
            }
            if !metadata.is_dir() {
                links.insert((metadata.dev(), metadata.ino()), destination.to_owned());
            }
            if !metadata.file_type().is_symlink() {
                let _span = profile.span("destination_sync");
                sync_tree_entry(&File::open(destination)?)?;
            }
            Ok(())
        }
        let mut content_cloned = true;
        let copy_span = profile.span("copy_entries");
        let entry_span = profile.span("entry_metadata");
        let metadata = fs::symlink_metadata(&source)?;
        drop(entry_span);
        copy(
            &source,
            destination,
            &metadata,
            &mut BTreeMap::new(),
            &mut content_cloned,
            &profile,
        )?;
        drop(copy_span);
        let source_span = profile.span("source_after");
        if SEALED {
            verify_tree_metadata(&source, &before)
        } else {
            verify_tree(&source, &before)
        }
        .context("source changed during copy")?;
        drop(source_span);
        let destination_span = profile.span("destination_inventory");
        verify_copied_tree(destination, &before, content_cloned)
            .context("copied tree does not match source")?;
        drop(destination_span);
        // Every copied inode was fsync'd on this owned destination device.
        // On macOS sync_all is F_FULLFSYNC: one final device drain persists
        // those prior writes, instead of draining the same device per inode.
        let _span = profile.span("parent_sync");
        File::open(&parent)?.sync_all()?;
        Ok(before)
    })();
    if result.is_err() {
        fs::remove_dir_all(destination).context("failed to remove incomplete tree")?;
    }
    result
}

/// Flush an inode to the device queue. On Apple the final parent sync_all is
/// the required device-wide persistence barrier; this helper alone is NOT a
/// durable commit. The fresh owned destination has no nested mount points.
fn sync_tree_entry(file: &File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

/// Kernel-confirmed COW provides content equality under the caller's exclusive
/// ownership contract. Rewalk all metadata/topology; do not infer equality from
/// timestamps or from a clone request that could have fallen back to copying.
fn verify_copied_tree(
    destination: &Path,
    expected: &TreeInventory,
    content_cloned: bool,
) -> anyhow::Result<()> {
    if !content_cloned {
        return verify_tree(destination, expected);
    }
    verify_tree_metadata(destination, expected)
}

/// Content comes from an independently established immutable generation or
/// kernel clone proof. This walk verifies metadata/topology, not integrity of
/// arbitrary mutable files; timestamps never establish content identity.
fn verify_tree_metadata(destination: &Path, expected: &TreeInventory) -> anyhow::Result<()> {
    let digests: BTreeMap<_, _> = expected
        .entries
        .iter()
        .filter_map(|entry| {
            if let TreeObject::File { sha256, .. } = &entry.object {
                Some((entry.path.as_slice(), sha256))
            } else {
                None
            }
        })
        .collect();
    let actual = inventory_with_hash::<true>(destination, &mut |path| {
        let relative = path.strip_prefix(destination)?.as_os_str().as_bytes();
        Ok(digests
            .get(relative)
            .context("unexpected file in cloned tree")?
            .to_string())
    })?;
    ensure!(
        expected.version == 1 && actual == *expected,
        "filesystem inventory mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod checked_copy_tests {
    use super::*;

    #[test]
    fn inventory_hashes_each_shared_inode_once_without_changing_manifest() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a"), b"shared").unwrap();
        fs::hard_link(temp.path().join("a"), temp.path().join("z")).unwrap();
        fs::write(temp.path().join("m"), b"unique").unwrap();
        let mut calls = 0;
        let actual = inventory_with_hash::<true>(temp.path(), &mut |path| {
            calls += 1;
            file_hash(path)
        })
        .unwrap();
        assert_eq!(calls, 2);
        let expected = inventory_with_hash::<false>(temp.path(), &mut file_hash).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn inventory_rejects_shared_content_changed_between_aliases() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a"), b"shared").unwrap();
        fs::hard_link(temp.path().join("a"), temp.path().join("z")).unwrap();
        fs::write(temp.path().join("m"), b"unique").unwrap();
        let error = inventory_with_hash::<true>(temp.path(), &mut |path| {
            let digest = file_hash(path)?;
            if path.file_name() == Some(std::ffi::OsStr::new("m")) {
                fs::write(temp.path().join("a"), b"changed shared contents")?;
            }
            Ok(digest)
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("hardlinked file changed during inventory")
        );
    }

    #[test]
    fn copied_tree_fallback_detects_same_size_content_corruption() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("file"), b"original").unwrap();
        let expected = inventory(temp.path()).unwrap();
        let entry = expected
            .entries
            .iter()
            .find(|entry| entry.path == b"file")
            .unwrap();
        fs::write(temp.path().join("file"), b"modified").unwrap();
        File::options()
            .write(true)
            .open(temp.path().join("file"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH
                    + std::time::Duration::new(entry.mtime as u64, entry.mtime_nsec as u32),
            ))
            .unwrap();
        assert!(verify_copied_tree(temp.path(), &expected, false).is_err());
    }

    #[test]
    fn cloned_tree_verification_rejects_metadata_and_topology_changes() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a"), b"shared").unwrap();
        fs::hard_link(temp.path().join("a"), temp.path().join("z")).unwrap();
        let expected = inventory(temp.path()).unwrap();
        verify_copied_tree(temp.path(), &expected, true).unwrap();
        fs::set_permissions(temp.path().join("a"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(verify_copied_tree(temp.path(), &expected, true).is_err());
        fs::set_permissions(
            temp.path().join("a"),
            fs::Permissions::from_mode(
                expected
                    .entries
                    .iter()
                    .find(|entry| entry.path == b"a")
                    .unwrap()
                    .mode
                    & 0o7777,
            ),
        )
        .unwrap();
        fs::remove_file(temp.path().join("z")).unwrap();
        fs::write(temp.path().join("z"), b"shared").unwrap();
        assert!(verify_copied_tree(temp.path(), &expected, true).is_err());
    }

    #[test]
    fn checked_copy_rejects_changed_content_before_creating_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"original").unwrap();
        let expected = inventory(&source).unwrap();
        fs::write(source.join("file"), b"modified").unwrap();
        assert!(copy_owned_tree_checked(&source, &destination, Some(&expected)).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn checked_copy_preserves_manifest_and_independent_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"original").unwrap();
        let expected = inventory(&source).unwrap();
        let copied = copy_owned_tree_checked(&source, &destination, Some(&expected)).unwrap();
        assert_eq!(copied, expected);
        verify_tree(&destination, &expected).unwrap();
        fs::write(source.join("file"), b"modified").unwrap();
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"original");
    }
}

#[cfg(test)]
#[test]
#[ignore = "manual paired hardlink inventory performance measurement"]
fn inventory_hardlink_paired_benchmark() {
    use std::time::Instant;
    let temp = tempfile::tempdir().unwrap();
    for (shared, bytes, count) in [
        (false, 16, 2048),
        (true, 16, 2048),
        (true, 1024 * 1024, 128),
    ] {
        let root = temp.path().join(format!("{shared}-{bytes}"));
        fs::create_dir(&root).unwrap();
        let data = vec![b'x'; bytes];
        for file in 0..count {
            let path = root.join(format!("f{file:04}"));
            if shared && file != 0 {
                fs::hard_link(root.join("f0000"), path).unwrap();
            } else {
                fs::write(path, &data).unwrap();
            }
        }
        assert_eq!(
            inventory_with_hash::<false>(&root, &mut file_hash).unwrap(),
            inventory(&root).unwrap()
        );
        for round in 0..18 {
            let order = if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for deduplicate in order {
                let mut hash_calls = 0u64;
                let mut hash = |path: &Path| {
                    hash_calls += 1;
                    file_hash(path)
                };
                let start = Instant::now();
                let inventory = if deduplicate {
                    inventory_with_hash::<true>(&root, &mut hash)
                } else {
                    inventory_with_hash::<false>(&root, &mut hash)
                }
                .unwrap();
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(inventory);
                println!(
                    "PVISOR_INVENTORY_BENCH {}",
                    serde_json::json!({
                        "shared":shared,"bytes_per_file":bytes,"files":count,"round":round,
                        "warmup":round < 3,"deduplicate":deduplicate,"elapsed_ms":elapsed_ms,
                        "hash_calls":hash_calls,"hashed_bytes":hash_calls * bytes as u64,
                        "scope":"paired inventory only, warm host cache, no copy/fsync/VM",
                    })
                );
            }
        }
    }
}

#[cfg(test)]
#[test]
#[ignore = "manual paired cloned-destination verification measurement"]
fn cloned_destination_verification_paired_benchmark() {
    use std::time::Instant;
    for (name, count, bytes) in [("small-2048", 2048, 16), ("large-128", 128, 1024 * 1024)] {
        let temp = tempfile::tempdir_in("/private/tmp").unwrap();
        let data = vec![0x5a; bytes];
        for index in 0..count {
            fs::write(temp.path().join(format!("{index:04}")), &data).unwrap();
        }
        let expected = inventory(temp.path()).unwrap();
        for round in 0..18 {
            let mut elapsed = [0u128; 2];
            let order = if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for cloned in order {
                let start = Instant::now();
                verify_copied_tree(temp.path(), &expected, cloned).unwrap();
                elapsed[usize::from(cloned)] = start.elapsed().as_nanos();
            }
            if round >= 3 {
                println!(
                    "PVISOR_CLONE_VERIFY_BENCH {}",
                    serde_json::json!({
                        "case": name, "round": round - 3,
                        "full_ns": elapsed[0], "cloned_ns": elapsed[1],
                        "files": count, "bytes": count * bytes,
                    })
                );
            }
        }
    }
}
