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
    Overlay(Box<super::overlay::OverlaySnapshot>),
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
    /// Verify the original frozen backing, including exact physical identities
    /// and saved file contents. This neither relocates roots nor grants sharing
    /// rights; saved writable handles remain valid only on this same backing.
    pub fn verify_frozen_backing(&self) -> io::Result<()> {
        self.fs.verify_frozen_backing()
    }
    /// Change only the audit/Attempt binding, preserving every authorization rule.
    pub fn rebind_overlay_policy(
        &mut self,
        policy: &pvisor_overlay_core::FileAccessPolicy,
    ) -> io::Result<()> {
        fn rebind(
            state: &mut FsSnapshot,
            policy: &pvisor_overlay_core::FileAccessPolicy,
        ) -> io::Result<()> {
            match state {
                FsSnapshot::ReadOnly(inner) | FsSnapshot::Augment { inner, .. } => {
                    rebind(inner, policy)
                }
                FsSnapshot::Overlay(state) => state.rebind_policy(policy),
                _ => Err(unsupported(
                    "audit rebinding requires an overlay filesystem",
                )),
            }
        }
        let mut rebound = self.fs.clone();
        rebind(&mut rebound, policy)?;
        self.fs = rebound;
        Ok(())
    }

    /// Rebind separately verified, exclusively owned copies of every backing
    /// directory. The coordinator must verify full inventories and preserve
    /// cross-directory hard links before calling. No partial or external layer
    /// binding is accepted; failure leaves the original snapshot unchanged.
    pub fn rebind_owned_layers(&mut self, copies: &[(PathBuf, PathBuf)]) -> io::Result<()> {
        self.rebind_layer_roots(copies, &[], false, &[])
    }

    /// Explicit shared-lower path: callers verify the entire immutable tree,
    /// retain durable references, and enforce read-only access in the runner.
    /// Upper/work/journal/target/baseline roles and writable saved handles may
    /// never be marked shared. Existing owned-copy rebinding is unchanged.
    /// An explicitly shared lower may keep its original root during recapture;
    /// every saved inode must then retain its exact physical identity.
    pub fn rebind_shared_readonly_layers(
        &mut self,
        copies: &[(PathBuf, PathBuf)],
        shared_lowers: &[PathBuf],
    ) -> io::Result<()> {
        fn validate(state: &FsSnapshot, shared_lowers: &[PathBuf]) -> io::Result<()> {
            match state {
                FsSnapshot::ReadOnly(inner) | FsSnapshot::Augment { inner, .. } => {
                    validate(inner, shared_lowers)
                }
                FsSnapshot::Overlay(state) => state.validate_shared_readonly_layers(shared_lowers),
                _ => Err(unsupported("shared lowers require an overlay filesystem")),
            }
        }
        validate(&self.fs, shared_lowers)?;
        self.rebind_layer_roots(copies, shared_lowers, false, &[])
    }

    /// Supervisor-side relocation of only verified immutable lowers while the
    /// VM remains frozen. Unmapped private backing remains exactly unchanged.
    /// The caller verifies complete source/destination inventories and retains
    /// the destination owners before calling; saved writable handles are refused.
    pub fn rebind_shared_lower_copies(&mut self, copies: &[(PathBuf, PathBuf)]) -> io::Result<()> {
        fn validate(state: &FsSnapshot, copies: &[(PathBuf, PathBuf)]) -> io::Result<()> {
            match state {
                FsSnapshot::ReadOnly(inner) | FsSnapshot::Augment { inner, .. } => {
                    validate(inner, copies)
                }
                FsSnapshot::Overlay(state) => state.validate_shared_lower_copies(copies),
                _ => Err(unsupported(
                    "shared lower copies require an overlay filesystem",
                )),
            }
        }
        validate(&self.fs, copies)?;
        let shared = copies
            .iter()
            .map(|(source, _)| source.clone())
            .collect::<Vec<_>>();
        self.rebind_layer_roots(copies, &shared, true, &[])
    }

    pub fn rebind_stage(
        &mut self,
        copies: &[(PathBuf, PathBuf)],
        immutable_lowers: &[PathBuf],
    ) -> io::Result<()> {
        self.rebind_layer_roots(copies, &[], false, immutable_lowers)
    }

    fn rebind_layer_roots(
        &mut self,
        copies: &[(PathBuf, PathBuf)],
        shared_lowers: &[PathBuf],
        preserve_unmapped: bool,
        immutable_lowers: &[PathBuf],
    ) -> io::Result<()> {
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
            let retained = source == &destination && shared_lowers.contains(source);
            if !std::fs::symlink_metadata(&destination)?.is_dir()
                || (!retained
                    && (destination.starts_with(source) || source.starts_with(&destination)))
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
        for root in immutable_lowers {
            if root.canonicalize()? != *root
                || !std::fs::symlink_metadata(root)?.is_dir()
                || roots
                    .keys()
                    .chain(roots.values())
                    .any(|path| root.starts_with(path) || path.starts_with(root))
                || !destinations.insert(root.clone())
                || roots.insert(root.clone(), root.clone()).is_some()
            {
                return Err(invalid("immutable lower overlaps copied backing"));
            }
        }
        let relocate = |path: &str| -> io::Result<String> {
            roots
                .get(Path::new(path))
                .map(PathBuf::as_path)
                .or_else(|| preserve_unmapped.then_some(Path::new(path)))
                .ok_or_else(|| invalid("overlay backing has no verified owned copy"))?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("copied layer path is not UTF-8"))
        };
        fn rebind(
            state: &mut FsSnapshot,
            relocate: &impl Fn(&str) -> io::Result<String>,
            shared_lowers: &[PathBuf],
            preserve_unmapped: bool,
            immutable_lowers: &[PathBuf],
        ) -> io::Result<()> {
            match state {
                FsSnapshot::ReadOnly(inner) | FsSnapshot::Augment { inner, .. } => rebind(
                    inner,
                    relocate,
                    shared_lowers,
                    preserve_unmapped,
                    immutable_lowers,
                ),
                FsSnapshot::Overlay(state) => {
                    state.rebind_roots(relocate, shared_lowers, preserve_unmapped, immutable_lowers)
                }
                _ => Err(unsupported("layer copies require an overlay filesystem")),
            }
        }
        let mut rebound = self.fs.clone();
        rebind(
            &mut rebound,
            &relocate,
            shared_lowers,
            preserve_unmapped,
            immutable_lowers,
        )?;
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
    pub(super) fn verify_frozen_backing(&self) -> io::Result<()> {
        match self {
            Self::ReadOnly(inner) | Self::Augment { inner, .. } => inner.verify_frozen_backing(),
            Self::Overlay(state) => state.verify_frozen_backing(),
            Self::Passthrough(state) => {
                let root = Path::new(std::ffi::OsStr::from_bytes(&state.root));
                let mut copy = self.clone();
                copy.rebind_verified_layer(root, root, true)
            }
            _ => Err(unsupported(
                "frozen backing verification requires an owned filesystem",
            )),
        }
    }
    pub(super) fn validate_readonly_handles(&self) -> io::Result<()> {
        match self {
            Self::ReadOnly(inner) | Self::Augment { inner, .. } => {
                inner.validate_readonly_handles()
            }
            Self::Passthrough(state) => {
                if state.handles.iter().any(|handle| {
                    handle.flags & libc::O_ACCMODE != libc::O_RDONLY
                        || handle.flags & (libc::O_TRUNC | libc::O_CREAT) != 0
                }) {
                    return Err(invalid("shared lower has a writable saved handle"));
                }
                Ok(())
            }
            _ => Err(unsupported(
                "shared lower handle validation requires passthrough backing",
            )),
        }
    }
    pub(super) fn rebind_owned_copy(
        &mut self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        self.rebind_verified_layer(source, destination, false)
    }

    /// The coordinator pins/authenticates the complete immutable tree. This
    /// verifies that every looked-up inode and saved handle still belongs to
    /// that same root; no copy or writable alias is accepted here.
    pub(super) fn retain_readonly_root(&mut self, root: &Path) -> io::Result<()> {
        self.validate_readonly_handles()?;
        self.rebind_verified_layer(root, root, true)
    }

    fn rebind_verified_layer(
        &mut self,
        source: &Path,
        destination: &Path,
        retained: bool,
    ) -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        match self {
            Self::ReadOnly(inner) | Self::Augment { inner, .. } => {
                inner.rebind_verified_layer(source, destination, retained)
            }
            Self::Passthrough(state) => {
                // Source may already have been removed. Its canonical path is
                // taken from the sealed environment binding, never resolved.
                if source.as_os_str().as_bytes() != state.root {
                    return Err(invalid("copied filesystem source binding mismatch"));
                }
                let root = destination.canonicalize()?;
                if root == source && !retained {
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
                    if retained && identity != saved.identity {
                        return Err(invalid("retained immutable inode identity changed"));
                    }
                    #[allow(clippy::useless_conversion)]
                    // libc mode constants differ in width across hosts.
                    let (mask, regular_mode, symlink_mode, directory_mode) = (
                        u32::from(libc::S_IFMT),
                        u32::from(libc::S_IFREG),
                        u32::from(libc::S_IFLNK),
                        u32::from(libc::S_IFDIR),
                    );
                    let kind = identity.mode & mask;
                    let regular = kind == regular_mode;
                    let symlink = kind == symlink_mode;
                    let directory = kind == directory_mode;
                    if identity.mode != saved.identity.mode
                        || identity.uid != saved.identity.uid
                        || identity.gid != saved.identity.gid
                        || identity.mtime != saved.identity.mtime
                        || identity.mtime_nsec != saved.identity.mtime_nsec
                        // A verified owned copy can cross filesystems. tmpfs
                        // counts directory children in st_nlink, while btrfs
                        // reports 1. Directory topology is authenticated by
                        // the coordinator's complete tree inventory; this is
                        // not a portable hardlink count. Files and symlinks
                        // still require exact nlink, and retained roots above
                        // still require their entire original identity.
                        || (!directory && identity.nlink != saved.identity.nlink)
                        || ((regular || symlink) && identity.size != saved.identity.size)
                        || regular != saved.digest.is_some()
                        || symlink != saved.link_target.is_some()
                    {
                        return Err(invalid(&format!(
                            "copied filesystem metadata mismatch at {}: saved {:?}, copied {:?}",
                            path.display(),
                            saved.identity,
                            identity
                        )));
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
            Self::Overlay(state) if !retained => state.rebind_owned_copy(source, destination),
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

#[cfg(test)]
mod owned_copy_tests {
    use super::*;
    use std::fs::FileTimes;

    fn fixture(path: &Path) -> (FsSnapshot, PathBuf, PathBuf) {
        let source = path.join("source");
        let destination = path.join("copy");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("file"), b"content").unwrap();
        std::fs::copy(source.join("file"), destination.join("file")).unwrap();
        for relative in ["file", ""] {
            let original = File::open(source.join(relative)).unwrap();
            let copied = File::open(destination.join(relative)).unwrap();
            copied
                .set_times(
                    FileTimes::new().set_modified(original.metadata().unwrap().modified().unwrap()),
                )
                .unwrap();
        }
        let mut inodes = Vec::new();
        for (inode, relative) in [(1, ""), (2, "file")] {
            let file = File::open(source.join(relative)).unwrap();
            inodes.push(InodeSnapshot {
                inode,
                refs: 1,
                path: Some(relative.as_bytes().to_vec()),
                identity: FileIdentity::read(&file).unwrap(),
                digest: (!relative.is_empty()).then(|| file_digest(&file).unwrap()),
                link_target: None,
            });
        }
        let state = FsSnapshot::Passthrough(PassthroughSnapshot {
            root: source.as_os_str().as_bytes().to_vec(),
            semantics: 0,
            entry_timeout: std::time::Duration::from_secs(1),
            attr_timeout: std::time::Duration::from_secs(1),
            cache_policy: Default::default(),
            xattr: false,
            inodes,
            handles: Vec::new(),
            next_handle: 1,
            submounts: false,
        });
        (state, source, destination)
    }

    #[test]
    fn verified_owned_directory_copy_accepts_filesystem_link_count_conventions() {
        let temp = tempfile::tempdir().unwrap();
        let (mut snapshot, source, destination) = fixture(temp.path());
        let FsSnapshot::Passthrough(state) = &mut snapshot else {
            unreachable!()
        };
        // Model a directory identity captured on tmpfs and copied to btrfs.
        // Directory size also varies by filesystem; neither describes links
        // between regular files in the authenticated owned tree.
        state.inodes[0].identity.nlink += 3;
        state.inodes[0].identity.size += 128;
        snapshot.rebind_owned_copy(&source, &destination).unwrap();
        let FsSnapshot::Passthrough(state) = snapshot else {
            unreachable!()
        };
        assert_eq!(
            state.root,
            destination.canonicalize().unwrap().as_os_str().as_bytes()
        );
        assert_eq!(
            state.inodes[0].identity,
            FileIdentity::read(&File::open(destination).unwrap()).unwrap()
        );
    }

    #[test]
    fn owned_copy_still_rejects_missing_regular_file_hardlinks() {
        let temp = tempfile::tempdir().unwrap();
        let (mut snapshot, source, destination) = fixture(temp.path());
        std::fs::hard_link(source.join("file"), source.join("alias")).unwrap();
        let FsSnapshot::Passthrough(state) = &mut snapshot else {
            unreachable!()
        };
        state.inodes[1].identity =
            FileIdentity::read(&File::open(source.join("file")).unwrap()).unwrap();
        // Restore directory mtime so the rejection tests the file's lost link.
        File::open(&destination)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(
                    File::open(&source)
                        .unwrap()
                        .metadata()
                        .unwrap()
                        .modified()
                        .unwrap(),
                ),
            )
            .unwrap();
        state.inodes[0].identity = FileIdentity::read(&File::open(&source).unwrap()).unwrap();
        assert!(snapshot
            .rebind_owned_copy(&source, &destination)
            .unwrap_err()
            .to_string()
            .contains("metadata mismatch"));
    }

    #[test]
    fn owned_copy_still_checks_contents_when_size_and_mtime_match() {
        let temp = tempfile::tempdir().unwrap();
        let (mut snapshot, source, destination) = fixture(temp.path());
        std::fs::write(destination.join("file"), b"altered").unwrap();
        let original = File::open(source.join("file")).unwrap();
        File::open(destination.join("file"))
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(original.metadata().unwrap().modified().unwrap()),
            )
            .unwrap();
        assert!(snapshot
            .rebind_owned_copy(&source, &destination)
            .unwrap_err()
            .to_string()
            .contains("content mismatch"));
    }

    #[test]
    fn retained_directory_still_requires_its_exact_identity() {
        let temp = tempfile::tempdir().unwrap();
        let (mut snapshot, source, _) = fixture(temp.path());
        let FsSnapshot::Passthrough(state) = &mut snapshot else {
            unreachable!()
        };
        state.inodes[0].identity.nlink += 3;
        assert!(snapshot
            .retain_readonly_root(&source)
            .unwrap_err()
            .to_string()
            .contains("retained immutable inode identity changed"));
    }
}
