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
#[expect(clippy::large_enum_variant, reason = "Preserve the migrated snapshot representation; boxing is a separate storage change")]
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
    pub link_target: Option<Vec<u8>>,
}
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryEntrySnapshot {
    pub ino: u64,
    pub offset: u64,
    pub type_: u8,
    pub name: Vec<u8>,
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
    #[cfg(target_os = "linux")]
    #[serde(default)]
    pub directory_entries: Option<Vec<DirectoryEntrySnapshot>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
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
            uid: meta.uid(),
            gid: meta.gid(),
            size: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
            nlink: meta.nlink(),
        })
    }
}

impl ServerSnapshot {
    /// Rebind separately verified, exclusively owned copies of every backing
    /// directory. The coordinator must verify full inventories and preserve
    /// cross-directory hard links before calling. No partial or external layer
    /// binding is accepted; failure leaves the original snapshot unchanged.
    pub fn rebind_owned_layers(&mut self, copies: &[(PathBuf, PathBuf)]) -> io::Result<()> {
        let mut roots = std::collections::BTreeMap::new();
        let mut destinations = std::collections::BTreeSet::new();
        for (source, destination) in copies {
            if !source.is_absolute()
                || source
                    .components()
                    .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            {
                return Err(invalid("invalid copied layer source binding"));
            }
            let destination = destination.canonicalize()?;
            if !std::fs::symlink_metadata(&destination)?.is_dir()
                || destination.starts_with(source)
                || source.starts_with(&destination)
                || !destinations.insert(destination.clone())
                || roots.insert(source.clone(), destination).is_some()
            {
                return Err(invalid(
                    "copied layer roots must be distinct and independent",
                ));
            }
        }
        if roots.is_empty() {
            return Err(invalid("missing copied layer bindings"));
        }
        let relocate = |path: &str| -> io::Result<String> {
            roots
                .get(Path::new(path))
                .ok_or_else(|| invalid("overlay backing has no verified owned copy"))?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("copied layer path is not UTF-8"))
        };
        fn rebind(
            state: &mut FsSnapshot,
            relocate: &impl Fn(&str) -> io::Result<String>,
        ) -> io::Result<()> {
            match state {
                FsSnapshot::ReadOnly(inner) | FsSnapshot::Augment { inner, .. } => {
                    rebind(inner, relocate)
                }
                FsSnapshot::Overlay(state) => state.rebind_roots(relocate),
                _ => Err(unsupported("layer copies require an overlay filesystem")),
            }
        }
        let mut rebound = self.fs.clone();
        rebind(&mut rebound, &relocate)?;
        self.fs = rebound;
        Ok(())
    }

    /// Rebind a captured filesystem to an exclusively owned, verified full
    /// copy. The environment coordinator must validate the entire archive
    /// (including objects not looked up by the guest) before calling this.
    /// For an overlay, source/destination are the common owned tree roots;
    /// every lower, upper, work, preimage, target and baseline binding must
    /// belong to that tree. Shared external backing is not relocated here.
    /// This does not change the ordinary restore identity checks or publish
    /// a snapshot. On failure the original server state is unchanged.
    pub fn rebind_owned_copy(&mut self, source: &Path, destination: &Path) -> io::Result<()> {
        let mut rebound = self.fs.clone();
        rebound.rebind_owned_copy(source, destination)?;
        self.fs = rebound;
        Ok(())
    }
}

impl FsSnapshot {
    pub(super) fn rebind_owned_copy(
        &mut self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        match self {
            Self::ReadOnly(inner) | Self::Augment { inner, .. } => {
                inner.rebind_owned_copy(source, destination)
            }
            Self::Passthrough(state) => {
                // Source may already have been removed. Its canonical path is
                // taken from the sealed environment binding, never resolved.
                if source.as_os_str().as_bytes() != state.root {
                    return Err(invalid("copied filesystem source binding mismatch"));
                }
                let root = destination.canonicalize()?;
                if root == source {
                    return Err(invalid("copied filesystem must have an independent root"));
                }
                let mut identities = Vec::with_capacity(state.inodes.len());
                let mut host_inodes = std::collections::BTreeSet::new();
                for saved in &state.inodes {
                    let path = relative_path(
                        &root,
                        saved
                            .path
                            .as_deref()
                            .ok_or_else(|| invalid("missing inode path"))?,
                    )?;
                    let pin = std::fs::OpenOptions::new()
                        .read(true)
                        // O_SYMLINK pins the link itself on macOS. Combining
                        // it with O_NOFOLLOW rejects symlinks with ELOOP.
                        .custom_flags(pin_flags())
                        .open(&path)?;
                    let identity = FileIdentity::read(&pin)?;
                    #[allow(clippy::useless_conversion)] // libc mode constants differ in width across hosts.
                    let (mask, regular_mode, symlink_mode) = (u32::from(libc::S_IFMT), u32::from(libc::S_IFREG), u32::from(libc::S_IFLNK));
                    let kind = identity.mode & mask;
                    let regular = kind == regular_mode;
                    let symlink = kind == symlink_mode;
                    if identity.mode != saved.identity.mode
                        || identity.uid != saved.identity.uid
                        || identity.gid != saved.identity.gid
                        || identity.mtime != saved.identity.mtime
                        || identity.mtime_nsec != saved.identity.mtime_nsec
                        || identity.nlink != saved.identity.nlink
                        || ((regular || symlink) && identity.size != saved.identity.size)
                        || regular != saved.digest.is_some()
                        || symlink != saved.link_target.is_some()
                    {
                        return Err(invalid("copied filesystem metadata mismatch"));
                    }
                    if regular
                        && Some(file_digest(
                            &std::fs::OpenOptions::new()
                                .read(true)
                                .custom_flags(libc::O_NOFOLLOW)
                                .open(&path)?,
                        )?) != saved.digest
                    {
                        return Err(invalid("copied filesystem content mismatch"));
                    }
                    if symlink
                        && Some(std::fs::read_link(&path)?.as_os_str().as_bytes().to_vec())
                            != saved.link_target
                    {
                        return Err(invalid("copied filesystem symlink mismatch"));
                    }
                    if !host_inodes.insert((identity.dev, identity.ino)) {
                        return Err(invalid("copied filesystem collapsed distinct inodes"));
                    }
                    if FileIdentity::read(&pin)? != identity {
                        return Err(invalid("copied filesystem changed during verification"));
                    }
                    identities.push(identity);
                }
                for (saved, identity) in state.inodes.iter_mut().zip(identities) {
                    saved.identity = identity;
                }
                state.root = root.as_os_str().as_bytes().to_vec();
                Ok(())
            }
            Self::Overlay(state) => state.rebind_owned_copy(source, destination),
            _ => Err(unsupported(
                "copied filesystem requires a passthrough or owned overlay root",
            )),
        }
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

pub(crate) fn pin_flags() -> i32 {
    #[cfg(target_os = "macos")]
    {
        libc::O_EVTONLY | libc::O_SYMLINK
    }
    #[cfg(target_os = "linux")]
    {
        libc::O_PATH | libc::O_NOFOLLOW
    }
}
