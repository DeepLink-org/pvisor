//! Frozen virtio-fs state. Linked objects bind to an unchanged, exclusively
//! owned host tree; this is not a filesystem fork or portable disk archive.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io;
use std::os::unix::{
    ffi::OsStrExt,
    fs::{FileExt, MetadataExt},
};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "state", deny_unknown_fields)]
pub enum FsSnapshot {
    Passthrough(PassthroughSnapshot),
    ReadOnly(Box<FsSnapshot>),
    Overlay(super::overlay::OverlaySnapshot),
    Augment {
        inner: Box<FsSnapshot>,
        names: Vec<(u64, Vec<u8>, u64)>,
        inodes: Vec<(u64, u32, bool, Option<[u8; 32]>)>,
    },
    Null,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSnapshot {
    pub(crate) options: u64,
    pub(crate) next_inode: u64,
    pub(crate) fs: FsSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassthroughSnapshot {
    pub(crate) root: Vec<u8>,
    pub(crate) semantics: u8,
    pub(crate) entry_timeout: std::time::Duration,
    pub(crate) attr_timeout: std::time::Duration,
    pub(crate) cache_policy: super::passthrough::CachePolicy,
    pub(crate) xattr: bool,
    pub(crate) inodes: Vec<InodeSnapshot>,
    pub(crate) handles: Vec<HandleSnapshot>,
    pub(crate) next_handle: u64,
    pub(crate) submounts: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InodeSnapshot {
    pub inode: u64,
    pub refs: u64,
    pub path: Option<Vec<u8>>,
    pub identity: FileIdentity,
    pub digest: Option<[u8; 32]>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandleSnapshot {
    pub handle: u64,
    pub inode: u64,
    pub flags: i32,
    pub offset: u64,
    pub entries: Vec<(u64, Vec<u8>, u8)>,
    pub directory_ready: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
    pub nlink: u64,
}
impl FileIdentity {
    pub fn read(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            size: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
            nlink: meta.nlink(),
        })
    }
}
pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub(crate) fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
pub(crate) fn relative_path(root: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let relative = Path::new(std::ffi::OsStr::from_bytes(bytes));
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("invalid snapshot relative path"));
    }
    // Check resolved parent too: a replaced directory symlink must not escape
    // the trusted root. The final object can itself be a symlink inode.
    let path = root.join(relative);
    if !relative.as_os_str().is_empty()
        && !path
            .parent()
            .ok_or_else(|| invalid("missing parent"))?
            .canonicalize()?
            .starts_with(root)
    {
        return Err(invalid("snapshot path escapes filesystem root"));
    }
    Ok(path)
}
pub(crate) fn file_digest(file: &File) -> io::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    let mut offset = 0;
    loop {
        let count = file.read_at(&mut buffer, offset)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(digest.finalize().into())
}

impl FsSnapshot {
    pub(crate) fn contains_inode(&self, inode: u64) -> bool {
        match self {
            Self::Passthrough(s) => s.inodes.iter().any(|i| i.inode == inode),
            Self::ReadOnly(s) => s.contains_inode(inode),
            Self::Overlay(s) => s.contains_inode(inode),
            Self::Augment { inner, inodes, .. } => {
                inner.contains_inode(inode) || inodes.iter().any(|i| i.0 == inode)
            }
            Self::Null => false,
        }
    }

    pub(crate) fn max_inode(&self) -> u64 {
        match self {
            Self::Passthrough(s) => s.inodes.iter().map(|i| i.inode).max().unwrap_or(1),
            Self::ReadOnly(s) => s.max_inode(),
            Self::Overlay(s) => s.max_inode(),
            Self::Augment { inner, inodes, .. } => inner
                .max_inode()
                .max(inodes.iter().map(|i| i.0).max().unwrap_or(1)),
            Self::Null => 1,
        }
    }
}
