//! Portable copy-on-write overlay served directly over virtio-fs.
//!
//! `pvisor-overlay-core` owns union semantics. Platform passthrough adapters
//! provide Linux permission emulation and FUSE request I/O on each resolved layer.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use pvisor_overlay_core::{
    backend,
    service::{FilesystemService, OpenBacking, UpperNamespaceMutation},
};
use pvisor_overlay_core::{BackingIdentity, BackingResolution, OverlayCore};

use super::super::linux_errno::linux_error;
use super::bindings;
use super::filesystem::{
    Context, DirEntry, Entry, Extensions, FileSystem, FsOptions, GetxattrReply, ListxattrReply,
    LowerDataObject, OpenOptions, OpenReply, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::fuse;
use super::inode_alloc::InodeAllocator;
use super::passthrough::{self, PassthroughFs};
use super::write_buffer::{Target as BufferedWriteTarget, WriteBuffer, WriteBufferOptions};

const TTL: Duration = Duration::from_secs(1);
pub(super) const DEFAULT_DIRECTORY_CACHE_CAPACITY: usize = 256;
pub(super) const MAX_DIRECTORY_CACHE_CAPACITY: usize = 4096;
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

fn buffered_request_flags(flags: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        flags & (libc::O_DIRECT | libc::O_SYNC | libc::O_DSYNC | libc::O_APPEND) as u32 == 0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = flags;
        false
    }
}

fn shared_open_qualification_enabled(setting: Option<&OsStr>) -> io::Result<bool> {
    match setting {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION must be 0 or 1",
        )),
    }
}

/// Stack-owned evidence crossing only OPEN's native and response assembly.
/// It is never inserted into a handle, cache, or snapshot.
struct NativeOpenReply {
    reply: OpenReply,
    #[cfg(target_os = "linux")]
    data_identity: Option<Option<passthrough::LowerDataIdentity>>,
}

fn negative_entry_cache_local_roots(roots: &[PathBuf]) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        roots.iter().all(|root| {
            let Ok(file) = std::fs::File::open(root) else {
                return false;
            };
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
            // SAFETY: the opened directory is pinned and output is writable.
            if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
                return false;
            }
            // SAFETY: successful fstatfs initialized the output structure.
            let magic = unsafe { stat.assume_init() }.f_type as u64 & 0xffff_ffff;
            matches!(magic, 0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = roots;
        false
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    serde(deny_unknown_fields)
)]
pub struct Config {
    pub lower_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lower_mutability: Vec<pvisor_overlay_core::LayerMutability>,
    #[serde(default)]
    pub apply_target: Option<String>,
    #[serde(default)]
    pub baseline_lower: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_content_index: Option<crate::api::BaselineContentIndex>,
    pub upper_dir: String,
    pub work_dir: Option<String>,
    pub preimage_dir: Option<String>,
    pub excluded_paths: Vec<String>,
    pub access_policy: pvisor_overlay_core::FileAccessPolicy,
    pub semantics: passthrough::PermissionSemantics,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    derive(serde::Serialize, serde::Deserialize)
)]
struct Layer(usize);

#[derive(Clone, Debug)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    serde(deny_unknown_fields)
)]
struct FileHandle {
    overlay_inode: u64,
    layer: Layer,
    inode: u64,
    handle: u64,
    #[serde(default)]
    relative_path: Vec<u8>,
}

/// The handle map and admission state jointly protect descriptor acquisition.
/// Namespace remains serialized, but a fence closes only affected handle IDs.
#[derive(Default)]
struct IoState {
    active: HashMap<u64, usize>,
    blocked: HashMap<u64, usize>,
    global: usize,
}

#[derive(Default)]
struct IoActivity {
    state: Mutex<IoState>,
    ready: Condvar,
}

struct IoFence {
    activity: Arc<IoActivity>,
    handles: Vec<u64>,
    global: bool,
}

impl IoActivity {
    fn fence(self: &Arc<Self>, handles: Vec<u64>, global: bool) -> IoFence {
        let fence = IoFence {
            activity: self.clone(),
            handles,
            global,
        };
        if !global && fence.handles.is_empty() {
            return fence;
        }
        let mut state = self.state.lock().unwrap();
        if global {
            state.global += 1;
        }
        for id in &fence.handles {
            *state.blocked.entry(*id).or_default() += 1;
        }
        fence
    }
}

impl IoFence {
    fn drain(&self) {
        if !self.global && self.handles.is_empty() {
            return;
        }
        let mut state = self.activity.state.lock().unwrap();
        while if self.global {
            !state.active.is_empty()
        } else {
            self.handles.iter().any(|id| state.active.contains_key(id))
        } {
            state = self.activity.ready.wait(state).unwrap();
        }
    }
}

impl Drop for IoFence {
    fn drop(&mut self) {
        if !self.global && self.handles.is_empty() {
            return;
        }
        let mut state = self.activity.state.lock().unwrap();
        if self.global {
            state.global -= 1;
        }
        for id in &self.handles {
            let count = state.blocked.get_mut(id).expect("missing I/O fence");
            *count -= 1;
            if *count == 0 {
                state.blocked.remove(id);
            }
        }
        self.activity.ready.notify_all();
    }
}

struct FileIoLease {
    id: u64,
    // Native descriptor ownership outlives removal from the backing map.
    native: passthrough::HandleLease,
    handle: Arc<FileHandle>,
    activity: Arc<IoActivity>,
}

impl Drop for FileIoLease {
    fn drop(&mut self) {
        let mut state = self.activity.state.lock().unwrap();
        let count = state
            .active
            .get_mut(&self.id)
            .expect("missing I/O admission");
        *count -= 1;
        if *count == 0 {
            state.active.remove(&self.id);
            // Only a global drain or a fence on this handle can be waiting
            // for its count to reach zero. Ordinary uncontended I/O neither
            // opens admission nor needs to wake unrelated fenced handles.
            if state.global != 0 || state.blocked.contains_key(&self.id) {
                self.activity.ready.notify_all();
            }
        }
    }
}

/// Owns native object admission and bytes without retaining a guest RAM lease.
struct BufferedWrite {
    lease: FileIoLease,
    fs: Arc<PassthroughFs>,
    kill_priv: bool,
    profile: pvisor_overlay_core::profile::Profile,
}

impl BufferedWriteTarget for BufferedWrite {
    fn merge_key(&self) -> (u64, bool) {
        (self.lease.id, self.kill_priv)
    }

    fn write(&self, chunks: &[&[u8]], offset: u64) -> io::Result<usize> {
        self.profile.add("host_writeback_backing_batches", 1);
        let result =
            self.fs
                .write_buffered_leased(&self.lease.native, chunks, offset, self.kill_priv);
        match &result {
            Ok(bytes) => {
                self.profile
                    .add("host_writeback_backing_bytes", *bytes as u64);
                if *bytes != chunks.iter().map(|chunk| chunk.len()).sum::<usize>() {
                    self.profile.add("host_writeback_backing_errors", 1);
                }
            }
            Err(_) => self.profile.add("host_writeback_backing_errors", 1),
        }
        result
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    serde(deny_unknown_fields)
)]
struct DirectoryItem {
    ino: u64,
    name: Vec<u8>,
    type_: u32,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    serde(tag = "kind", content = "state", deny_unknown_fields)
)]
enum Handle {
    // Immutable open metadata is shared by descriptor leases. Cloning a lease
    // must not allocate and copy the saved path for every WRITE/FLUSH/FSYNC.
    // Arc serializes transparently, preserving the persisted handle format.
    File(Arc<FileHandle>),
    Directory(Arc<Vec<DirectoryItem>>),
    // Keep the old Directory variant for snapshots with materialized entries.
    // New handles defer metadata/inode work until the caller consumes a page.
    DirectoryNames {
        parent: u64,
        names: Arc<Vec<Vec<u8>>>,
        #[serde(default)]
        types: Arc<Vec<u32>>,
    },
}

#[derive(Default)]
struct Nodes {
    by_inode: HashMap<u64, PathBuf>,
    // Guest LOOKUP references differ from the single native lookup lease held
    // for a detached writeback object. Namespace removal must not erase them.
    lookup_refs: HashMap<u64, u64>,
    // Old snapshots did not count guest references. Only this finite restored
    // set conservatively retains detached identities until session teardown.
    legacy_lookup_refs: BTreeSet<u64>,
    // Retain constant-time lookups on the hot path. The separate component-ordered
    // index makes namespace updates visit only the affected subtree.
    by_path: HashMap<PathBuf, u64>,
    ordered_paths: BTreeSet<PathBuf>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlaySnapshot {
    config: Config,
    hard_links: Vec<(u64, u64, Vec<PathBuf>)>,
    /// Physical lower identities must be relocated too: a future copy-up of a
    /// previously unvisited alias must join the already copied upper inode.
    #[serde(default)]
    hard_link_origins: Vec<(u64, u64, usize, Vec<u8>)>,
    layers: Vec<super::snapshot::FsSnapshot>,
    nodes: Vec<(u64, Vec<u8>)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lookup_refs: Option<Vec<(u64, u64)>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    legacy_lookup_refs: Vec<u64>,
    handles: Vec<(u64, Handle)>,
    next_handle: u64,
}

impl OverlaySnapshot {
    pub(super) fn validate_writeback(&self, enabled: bool) -> io::Result<()> {
        for layer in &self.layers {
            layer.validate_writeback(enabled)?;
        }
        Ok(())
    }

    pub(super) fn verify_frozen_backing(&self) -> io::Result<()> {
        for layer in &self.layers {
            layer.verify_frozen_backing()?;
        }
        // Validate lower origin/upper alias topology too, without changing the
        // saved snapshot or relaxing any ordinary copied-root checks.
        let mut copy = self.clone();
        copy.rebind_roots(|root| Ok(root.to_owned()), &[], true, &[])
    }
    pub(super) fn validate_shared_readonly_layers(
        &self,
        shared_lowers: &[PathBuf],
    ) -> io::Result<()> {
        use super::snapshot::invalid;
        let mut seen = std::collections::BTreeSet::new();
        if self.layers.len() != self.config.lower_dirs.len() + 1 {
            return Err(invalid("overlay snapshot layer count mismatch"));
        }
        for source in shared_lowers {
            if !seen.insert(source) {
                return Err(invalid("duplicate shared lower binding"));
            }
            if std::iter::once(self.config.upper_dir.as_str())
                .chain(self.config.work_dir.as_deref())
                .chain(self.config.preimage_dir.as_deref())
                .chain(self.config.apply_target.as_deref())
                .chain(self.config.baseline_lower.as_deref())
                .any(|mutable| {
                    Path::new(mutable).starts_with(source) || source.starts_with(mutable)
                })
            {
                return Err(invalid("shared lower overlaps a private backing role"));
            }
            let index = self
                .config
                .lower_dirs
                .iter()
                .position(|lower| Path::new(lower) == source)
                .ok_or_else(|| invalid("shared backing is not a lower layer"))?;
            self.layers[index + 1].validate_readonly_handles()?;
        }
        Ok(())
    }

    pub(super) fn rebind_owned_copy(
        &mut self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        use super::snapshot::invalid;
        if !source.is_absolute()
            || source.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err(invalid("invalid overlay source binding"));
        }
        let destination = destination.canonicalize()?;
        if destination.starts_with(source) || source.starts_with(&destination) {
            return Err(invalid("copied overlay must have an independent root"));
        }
        let relocate = |path: &str| -> io::Result<String> {
            let original = Path::new(path);
            let relative = original
                .strip_prefix(source)
                .map_err(|_| invalid("overlay backing escapes owned source tree"))?;
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(invalid("invalid overlay backing relative path"));
            }
            let copied = destination.join(relative);
            if !std::fs::symlink_metadata(&copied)?.is_dir()
                || !copied.canonicalize()?.starts_with(&destination)
            {
                return Err(invalid("copied overlay backing is not an owned directory"));
            }
            copied
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("overlay backing path is not UTF-8"))
        };
        self.rebind_roots(relocate, &[], false, &[])
    }

    pub(super) fn validate_shared_lower_copies(
        &self,
        copies: &[(PathBuf, PathBuf)],
    ) -> io::Result<()> {
        use super::snapshot::invalid;
        let shared = copies
            .iter()
            .map(|(source, _)| source.clone())
            .collect::<Vec<_>>();
        self.validate_shared_readonly_layers(&shared)?;
        for (source, destination) in copies {
            let destination = destination.canonicalize()?;
            for other in std::iter::once(self.config.upper_dir.as_str())
                .chain(self.config.work_dir.as_deref())
                .chain(self.config.preimage_dir.as_deref())
                .chain(self.config.apply_target.as_deref())
                .chain(self.config.baseline_lower.as_deref())
                .chain(
                    self.config
                        .lower_dirs
                        .iter()
                        .map(String::as_str)
                        .filter(|root| Path::new(root) != source),
                )
            {
                let other = Path::new(other);
                if destination.starts_with(other) || other.starts_with(&destination) {
                    return Err(invalid(
                        "shared lower destination overlaps unchanged backing",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn rebind_roots(
        &mut self,
        relocate: impl Fn(&str) -> io::Result<String>,
        shared_lowers: &[PathBuf],
        preserve_unmapped: bool,
        retained: &[PathBuf],
    ) -> io::Result<()> {
        use super::snapshot::invalid;
        use std::os::unix::fs::MetadataExt;
        for root in retained {
            if !self
                .config
                .lower_dirs
                .iter()
                .any(|lower| Path::new(lower) == root)
                || std::iter::once(Some(self.config.upper_dir.as_str()))
                    .chain([
                        self.config.work_dir.as_deref(),
                        self.config.preimage_dir.as_deref(),
                        self.config.apply_target.as_deref(),
                    ])
                    .flatten()
                    .any(|mutable| {
                        let mutable = Path::new(mutable);
                        mutable.starts_with(root) || root.starts_with(mutable)
                    })
            {
                return Err(invalid("retained binding must be an immutable lower"));
            }
        }
        let original_roots = std::iter::once(self.config.upper_dir.clone())
            .chain(self.config.lower_dirs.iter().cloned())
            .collect::<Vec<_>>();
        if self.layers.len() != original_roots.len() || self.config.lower_dirs.is_empty() {
            return Err(invalid("overlay snapshot layer count mismatch"));
        }
        let mut config = self.config.clone();
        config.upper_dir = relocate(&config.upper_dir)?;
        config.lower_dirs = config
            .lower_dirs
            .iter()
            .map(|root| relocate(root))
            .collect::<io::Result<_>>()?;
        // Copies begin a new backing lifetime. Only explicitly retained, unchanged
        // lower owners may keep an existing stability promise.
        if !config.lower_mutability.is_empty() {
            if config.lower_mutability.len() != config.lower_dirs.len() {
                return Err(invalid("lower mutability length mismatch"));
            }
            for (index, promise) in config.lower_mutability.iter_mut().enumerate() {
                let old = &self.config.lower_dirs[index];
                if old != &config.lower_dirs[index]
                    || !retained.iter().any(|root| root == Path::new(old))
                {
                    *promise = pvisor_overlay_core::LayerMutability::Mutable;
                }
            }
        }
        config.work_dir = config.work_dir.as_deref().map(&relocate).transpose()?;
        config.preimage_dir = config.preimage_dir.as_deref().map(&relocate).transpose()?;
        config.apply_target = config.apply_target.as_deref().map(&relocate).transpose()?;
        if config
            .baseline_content_index
            .as_ref()
            .is_some_and(|index| !retained.iter().any(|root| root == &index.root))
        {
            // A copied generation has no lease on the original receipt. Its
            // fingerprints remain identical, but must read the copied bytes.
            config.baseline_content_index = None;
        }
        config.baseline_lower = config
            .baseline_lower
            .as_deref()
            .map(&relocate)
            .transpose()?;
        let roots = std::iter::once(config.upper_dir.clone())
            .chain(config.lower_dirs.iter().cloned())
            .collect::<Vec<_>>();
        let mut layers = self.layers.clone();
        for ((layer, original), copied) in layers.iter_mut().zip(&original_roots).zip(&roots) {
            if original == copied && retained.iter().any(|root| root == Path::new(original)) {
                continue;
            }
            if original == copied && shared_lowers.contains(&PathBuf::from(original)) {
                layer.retain_readonly_root(Path::new(original))?;
            } else if original == copied && preserve_unmapped {
                // Only the explicitly selected read-only lower roots change.
                // Private data/state was already captured and verified.
            } else {
                layer.rebind_owned_copy(Path::new(original), Path::new(copied))?;
            }
        }
        let mut origins = std::collections::BTreeMap::new();
        let mut rebound_origins = Vec::new();
        for (dev, ino, layer, relative) in &self.hard_link_origins {
            if *layer == 0 || *layer >= roots.len() || origins.contains_key(&(*dev, *ino)) {
                return Err(invalid("invalid overlay hard-link origin"));
            }
            let path = super::snapshot::relative_path(Path::new(&roots[*layer]), relative)?;
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.nlink() < 2 {
                return Err(invalid("copied overlay lost its lower hard-link origin"));
            }
            let identity = (metadata.dev(), metadata.ino());
            origins.insert((*dev, *ino), identity);
            rebound_origins.push((identity.0, identity.1, *layer, relative.clone()));
        }
        if origins.len() != self.hard_links.len() {
            return Err(invalid(
                "overlay hard-link origins are missing or duplicated",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut hard_links = Vec::new();
        for (dev, ino, paths) in &self.hard_links {
            let &(dev, ino) = origins
                .get(&(*dev, *ino))
                .ok_or_else(|| invalid("overlay hard-link origin missing"))?;
            if !seen.insert((dev, ino)) {
                return Err(invalid("copied overlay collapsed hard-link origins"));
            }
            hard_links.push((dev, ino, paths.clone()));
        }
        // Validate upper aliases against their new physical inode before changing
        // any state. This also rejects escaping, absent or split link groups.
        let layout = pvisor_overlay_core::OverlayLayout::with_baseline(
            config.lower_dirs.iter().map(PathBuf::from).collect(),
            config
                .apply_target
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(config.lower_dirs.last().unwrap())),
            config.baseline_lower.as_deref().map(Path::new),
        )?
        .with_lower_mutability(config.lower_mutability.clone())?;
        let core = OverlayCore::open_existing_for_layout(
            layout,
            PathBuf::from(&config.upper_dir),
            config.work_dir.as_ref().map(PathBuf::from),
            config.excluded_paths.iter().map(PathBuf::from).collect(),
            config.preimage_dir.as_ref().map(PathBuf::from),
        )?;
        let core = if let Some(index) = &config.baseline_content_index {
            core.with_immutable_content_index(&index.root, index.file.clone(), &index.sha256)?
        } else {
            core
        };
        core.restore_hard_links(&hard_links)?;
        self.config = config;
        self.layers = layers;
        self.hard_links = hard_links;
        self.hard_link_origins = rebound_origins;
        Ok(())
    }
    pub(super) fn rebind_exclusions(&mut self, paths: &[PathBuf]) -> io::Result<()> {
        use super::snapshot::invalid;
        let names = paths
            .iter()
            .map(|path| {
                if path.as_os_str().is_empty()
                    || path
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                {
                    return Err(invalid("invalid restored exclusion path"));
                }
                path.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("non-UTF8 exclusion path"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        if self
            .config
            .excluded_paths
            .iter()
            .any(|old| !names.contains(old))
        {
            return Err(invalid(
                "restored exclusions cannot remove an existing rule",
            ));
        }
        for path in paths {
            if !self
                .config
                .excluded_paths
                .iter()
                .any(|old| Path::new(old) == path)
                && self
                    .nodes
                    .iter()
                    .any(|(_, node)| Path::new(OsStr::from_bytes(node)).starts_with(path))
            {
                return Err(invalid("restored exclusion would hide a saved inode"));
            }
        }
        self.config.excluded_paths = names;
        Ok(())
    }

    pub(super) fn rebind_policy(
        &mut self,
        policy: &pvisor_overlay_core::FileAccessPolicy,
    ) -> io::Result<()> {
        if !self.config.access_policy.same_rules(policy) {
            return Err(super::snapshot::invalid(
                "restored filesystem authorization rules changed",
            ));
        }
        self.config.access_policy = policy.clone();
        Ok(())
    }

    pub(crate) fn contains_inode(&self, inode: u64) -> bool {
        self.nodes.iter().any(|n| n.0 == inode)
            || self.layers.iter().any(|s| s.contains_inode(inode))
    }

    pub(crate) fn max_inode(&self) -> u64 {
        self.layers
            .iter()
            .map(super::snapshot::FsSnapshot::max_inode)
            .chain(self.nodes.iter().map(|n| n.0))
            .max()
            .unwrap_or(1)
    }
}

/// Own one native lookup reference. A borrower retains the inode even if its
/// cache entry is invalidated/evicted before the backing syscall completes.
struct DirectoryReference {
    fs: Arc<PassthroughFs>,
    inode: u64,
}

// A successful namespace removal retires a guest identity, not its metadata.
// Upper objects retain one native O_PATH lookup reference until guest FORGET.
// Immutable lower objects retain only their guest metadata view: writeback of
// unlink/rename timestamps must never modify the physical immutable lower.
enum DetachedWritebackInode {
    Upper {
        reference: DirectoryReference,
        projected: bindings::stat64,
        virtual_mode: bool,
    },
    Lower(Mutex<bindings::stat64>),
}
impl Drop for DirectoryReference {
    fn drop(&mut self) {
        if self.inode != fuse::ROOT_ID {
            self.fs.forget(
                Context {
                    uid: 0,
                    gid: 0,
                    pid: 0,
                },
                self.inode,
                1,
            );
        }
    }
}

struct CachedDirectory {
    identity: BackingIdentity,
    reference: Arc<DirectoryReference>,
    used: u64,
}

enum BackingLookup {
    Native(Entry),
    #[cfg(target_os = "linux")]
    RegularAttributes {
        attr: bindings::stat64,
        attr_timeout: Duration,
        entry_timeout: Duration,
    },
}

#[derive(Default)]
struct DirectoryCache {
    entries: HashMap<(usize, PathBuf), CachedDirectory>,
    clock: u64,
}

// Fields drop in declaration order. Clear/resume the Core namespace epoch
// while the VM still owns the exclusive operation lock, including on failure.
struct WriteOperation<'a> {
    _upper_namespace: UpperNamespaceMutation<'a>,
    _operation: std::sync::RwLockWriteGuard<'a, ()>,
}

const MAX_LOWER_DATA_OBJECTS: usize = 4096;

/// No eviction: a guest may retain pages after the native FH is released.
/// The layer ordinal is a generation within this fixed-root owner; the remaining
/// fields are actual FD device/inode/mount/size evidence. No descriptor is pinned.
#[derive(Default)]
struct LowerDataObjects {
    ids: HashMap<(usize, u64, u64, u64, u64), u32>,
}

impl LowerDataObjects {
    fn register(&mut self, key: (usize, u64, u64, u64, u64)) -> Option<(LowerDataObject, bool)> {
        if let Some(id) = self.ids.get(&key) {
            return Some((
                LowerDataObject {
                    id: *id,
                    size: key.4,
                },
                false,
            ));
        }
        if self.ids.len() >= MAX_LOWER_DATA_OBJECTS {
            return None;
        }
        let id = self.ids.len() as u32 + 1;
        self.ids.insert(key, id);
        Some((LowerDataObject { id, size: key.4 }, true))
    }
}

pub struct OverlayFs {
    profile: pvisor_overlay_core::profile::Profile,
    // Namespace and metadata remain serialized. Descriptor I/O admission
    // uses the handle map and scoped fences without acquiring this guard.
    operation_lock: RwLock<()>,
    io_activity: Arc<IoActivity>,
    // Dropped before Core/layers. The worker owns only native object leases,
    // never guest Readers, descriptor chains, or RAM gate access.
    host_writeback: bool,
    host_writeback_options: WriteBufferOptions,
    host_writeback_qualification_lock: bool,
    host_writeback_synchronous: bool,
    write_buffer: RwLock<Option<Arc<WriteBuffer>>>,
    core: FilesystemService,
    // New INITs use the caller's current preference. Restoring an already
    // negotiated session must not silently change its saved writeback mode.
    new_session_writeback: bool,
    // Derived from INIT or restored native state, never a second snapshot field.
    writeback_active: AtomicBool,
    // Only the root device opts in, and each OPEN still requires an explicit
    // physical-lower stability promise. Guest-owned writeback separately keeps
    // upper cache pages. This preference is not a persisted handle/cache object.
    immutable_data_cache: bool,
    readonly_flush_elision: bool,
    readonly_flush_elision_configured: bool,
    readonly_flush_elision_active: AtomicBool,
    readonly_flush_elision_seen: AtomicBool,
    lower_data_cache: bool,
    lower_data_cache_configured: bool,
    lower_data_cache_active: AtomicBool,
    lower_data_cache_seen: AtomicBool,
    lower_data_objects: Mutex<LowerDataObjects>,
    shared_open_qualification: bool,
    negative_entry_cache: bool,
    negative_entry_cache_configured: bool,
    // Standard negative dentries can survive until their TTL after a reply.
    // Never clear this owner-lifetime snapshot fence during DESTROY/INIT.
    negative_entry_cache_seen: AtomicBool,
    negative_entry_cache_roots: Vec<PathBuf>,
    negative_entry_cache_protected_prefixes: Vec<PathBuf>,
    virtual_metadata: bool,
    snapshot_config: Config,
    roots: Vec<PathBuf>,
    layers: Vec<Arc<PassthroughFs>>,
    // Owns one lookup reference per entry, never attributes or policy results.
    // Checked against fresh Core parent identities before every use.
    directory_cache: Mutex<DirectoryCache>,
    // Runtime preference only: references are discarded before snapshots and
    // restored owners use their newly configured bound.
    directory_cache_capacity: usize,
    directory_cache_configured: bool,
    inode_alloc: Arc<InodeAllocator>,
    nodes: Mutex<Nodes>,
    detached_writeback: Mutex<HashMap<u64, Arc<DetachedWritebackInode>>>,
    handles: Mutex<HashMap<u64, Handle>>,
    next_handle: AtomicU64,
}

impl OverlayFs {
    pub fn new(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        Self::build(cfg, inode_alloc, false)
    }

    pub fn open_existing(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        Self::build(cfg, inode_alloc, true)
    }

    pub(super) fn configure_immutable_data_cache(&mut self, enabled: bool) {
        self.immutable_data_cache = enabled;
    }

    #[cfg(test)]
    pub(super) fn configure_host_writeback(&mut self, enabled: bool, dax: bool) -> io::Result<()> {
        self.configure_host_writeback_options(
            enabled,
            dax,
            WriteBufferOptions::default(),
            false,
            false,
        )
    }

    pub(super) fn configure_host_writeback_options(
        &mut self,
        enabled: bool,
        dax: bool,
        options: WriteBufferOptions,
        qualification_lock: bool,
        synchronous: bool,
    ) -> io::Result<()> {
        options.validate()?;
        if !enabled {
            return Ok(());
        }
        if !cfg!(target_os = "linux") || dax || self.new_session_writeback {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "host writeback requires Linux, DAX=0 and guest writeback=0",
            ));
        }
        let buffer = Arc::new(WriteBuffer::with_profile(options, self.profile.clone())?);
        *self.write_buffer.get_mut().unwrap() = Some(buffer);
        self.host_writeback = true;
        self.host_writeback_options = options;
        self.host_writeback_qualification_lock = qualification_lock;
        self.host_writeback_synchronous = synchronous;
        self.profile.add("host_writeback_enabled", 1);
        for name in [
            "host_writeback_accepted_requests",
            "host_writeback_accepted_bytes",
            "host_writeback_backing_batches",
            "host_writeback_backing_bytes",
            "host_writeback_backing_errors",
            "host_writeback_queue_sessions",
            "host_writeback_queue_peak_bytes_sum",
            "host_writeback_queue_peak_jobs_sum",
            "host_writeback_queue_batches",
            "host_writeback_queue_jobs",
            "host_writeback_reserve_wait_count",
            "host_writeback_reserve_wait_ns",
        ] {
            self.profile.add(name, 0);
        }
        for (name, value) in [
            ("host_writeback_capacity_bytes", options.budget_bytes as u64),
            ("host_writeback_max_jobs", options.max_jobs as u64),
            ("host_writeback_batch_bytes", options.batch_bytes as u64),
            ("host_writeback_max_chunks", options.max_chunks as u64),
            (
                "host_writeback_delay_us",
                options.max_delay.as_micros() as u64,
            ),
            (
                "host_writeback_qualification_lock",
                u64::from(qualification_lock),
            ),
            ("host_writeback_synchronous", u64::from(synchronous)),
        ] {
            self.profile.add(name, value);
        }
        Ok(())
    }

    fn buffered_writes(&self) -> Option<Arc<WriteBuffer>> {
        if !self.host_writeback {
            return None;
        }
        self.write_buffer.read().unwrap().clone()
    }

    /// Wait for the accepted prefix. Errors remain sticky in the queue until
    /// FLUSH/FSYNC/SYNCFS or capture observes them; namespace cleanup still runs.
    fn drain_buffered_writes(&self) -> io::Result<()> {
        match self.buffered_writes() {
            Some(buffer) => buffer.flush(),
            None => Ok(()),
        }
    }

    /// The explicit preference promises exclusive upper namespace ownership,
    /// including physical roots/ancestors/mounts and directory permissions.
    /// All mutations use this VM connection; no overlapping backend may be
    /// attached later. Runtime-only: successful negative replies fence snapshots.
    pub(super) fn configure_negative_entry_cache(
        &mut self,
        requested: bool,
        guest_cache: bool,
        dax: bool,
        synthetic_names: Vec<PathBuf>,
    ) -> io::Result<()> {
        if self.negative_entry_cache_configured {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "negative entry cache must be configured once before requests",
            ));
        }
        let mut enabled = requested
            && cfg!(target_os = "linux")
            && guest_cache
            && !dax
            && !self.new_session_writeback
            && self.snapshot_config.semantics == passthrough::PermissionSemantics::LinuxComplete
            && self.supports_directory_image()
            && self
                .snapshot_config
                .access_policy
                .same_rules(&Default::default());
        let mut roots = Vec::new();
        if enabled {
            self.core.validate_exclusive_upper_namespace()?;
            roots = self
                .roots
                .iter()
                .map(|root| root.canonicalize())
                .collect::<io::Result<_>>()?;
            enabled = negative_entry_cache_local_roots(&roots);
            for root in &roots {
                if backend::attachment_overlaps(root)? {
                    enabled = false;
                    break;
                }
            }
        }
        self.negative_entry_cache = enabled;
        self.negative_entry_cache_roots = roots;
        // Core already validates exclusions as nonempty relative paths. Their
        // ENOENT is a visibility decision, never a cacheable absence; protect
        // ancestors too so a missing prefix cannot hide that distinction.
        let mut protected_prefixes = synthetic_names;
        protected_prefixes.extend(
            self.snapshot_config
                .excluded_paths
                .iter()
                .map(PathBuf::from),
        );
        protected_prefixes.sort();
        protected_prefixes.dedup();
        self.negative_entry_cache_protected_prefixes = protected_prefixes;
        self.negative_entry_cache_configured = true;
        self.profile
            .add("negative_entry_cache_requested", u64::from(requested));
        self.profile
            .add("negative_entry_cache_enabled", u64::from(enabled));
        self.profile.add("negative_entry_cache_replies", 0);
        self.profile.add("negative_entry_cache_fallbacks", 0);
        Ok(())
    }

    fn negative_entry_cache_path_eligible(&self, path: &Path) -> bool {
        !self
            .negative_entry_cache_protected_prefixes
            .iter()
            .any(|name| path.starts_with(name) || name.starts_with(path))
            && self.negative_entry_cache_roots.iter().all(|root| {
                // A late attachment violates the owner's promise. Refuse further
                // promises anyway; this cannot invalidate prior guest dentries.
                matches!(backend::attachment_overlaps(root), Ok(false))
            })
    }

    /// Runtime-only preference, configured once before INIT. A negotiated
    /// session retains all guest close barriers and skips only a qualified
    /// native read-only lower descriptor's backend FLUSH. Snapshots of such
    /// sessions are rejected rather than persisting a new contract implicitly.
    pub(super) fn configure_readonly_flush_elision(
        &mut self,
        requested: bool,
        guest_cache: bool,
        dax: bool,
    ) -> io::Result<()> {
        if self.readonly_flush_elision_configured {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read-only flush elision must be configured once before INIT",
            ));
        }
        self.readonly_flush_elision = requested
            && cfg!(target_os = "linux")
            && guest_cache
            && !dax
            && self.snapshot_config.semantics == passthrough::PermissionSemantics::LinuxComplete
            && self
                .snapshot_config
                .lower_mutability
                .contains(&pvisor_overlay_core::LayerMutability::Immutable);
        self.readonly_flush_elision_configured = true;
        self.profile
            .add("readonly_flush_elision_requested", u64::from(requested));
        self.profile.add("readonly_flush_elision_enabled", 0);
        self.profile.add("readonly_flush_elision_open_flags", 0);
        Ok(())
    }

    /// Configure the private independent-lower-data protocol before INIT.
    /// The caller's immutable owner keeps physical contents, identity and mount
    /// stable through final device teardown; IDs never provide authorization.
    pub(super) fn configure_lower_data_cache(
        &mut self,
        requested: bool,
        guest_cache: bool,
        dax: bool,
    ) -> io::Result<()> {
        if self.lower_data_cache_configured {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lower data cache must be configured once before INIT",
            ));
        }
        self.lower_data_cache = requested
            && cfg!(target_os = "linux")
            && guest_cache
            && !dax
            && self.snapshot_config.semantics == passthrough::PermissionSemantics::LinuxComplete
            && self
                .snapshot_config
                .lower_mutability
                .contains(&pvisor_overlay_core::LayerMutability::Immutable);
        self.lower_data_cache_configured = true;
        self.profile
            .add("lower_data_cache_requested", u64::from(requested));
        for key in [
            "lower_data_cache_enabled",
            "lower_data_cache_open_flags",
            "lower_data_cache_object_registrations",
            "lower_data_cache_object_reuses",
            "lower_data_cache_object_rejections",
            "lower_data_cache_capacity_rejections",
        ] {
            self.profile.add(key, 0);
        }
        Ok(())
    }

    /// Opt in before serving requests or restoring state. The separate flag
    /// promises stable physical upper roots/ancestors/mounts, in addition to
    /// the guest-directory contract's exclusively owned upper namespace.
    pub(super) fn configure_exclusive_upper_absence_cache(
        &mut self,
        requested: bool,
        guest_cache: bool,
    ) -> io::Result<()> {
        let enabled = requested
            && guest_cache
            && self.snapshot_config.semantics == passthrough::PermissionSemantics::LinuxComplete
            && self.supports_directory_image();
        self.core.configure_exclusive_upper_absence_cache(enabled)?;
        self.profile.add(
            "exclusive_upper_absence_cache_requested",
            u64::from(requested),
        );
        Ok(())
    }

    /// Configure a fresh owner before serving requests or restoring handles.
    /// This changes only retained native directory references, not metadata,
    /// authority, invalidation, or the snapshot's filesystem contract.
    pub(super) fn configure_directory_cache_capacity(&mut self, capacity: usize) -> io::Result<()> {
        if !(1..=MAX_DIRECTORY_CACHE_CAPACITY).contains(&capacity) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory reference cache capacity must be between 1 and 4096",
            ));
        }
        if self.directory_cache_configured
            || !self.directory_cache.get_mut().unwrap().entries.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory reference cache capacity must be configured once before serving requests",
            ));
        }
        self.directory_cache_capacity = capacity;
        self.directory_cache_configured = true;
        // FsWorker calls this once per owner. The final per-instance units
        // value proves the chosen capacity in independent diagnostics.
        self.profile
            .add("directory_cache_capacity", capacity as u64);
        Ok(())
    }

    fn read_operation(&self) -> io::Result<std::sync::RwLockReadGuard<'_, ()>> {
        let _wait = self.profile.span("operation_read_lock_wait");
        let guard = self
            .operation_lock
            .read()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        // A completed WRITE reply must be reflected in subsequent metadata.
        // Preserve cleanup operations even after a recorded asynchronous error.
        let _ = self.drain_buffered_writes();
        Ok(guard)
    }

    fn write_operation(&self) -> io::Result<WriteOperation<'_>> {
        let _wait = self.profile.span("operation_write_lock_wait");
        let operation = self
            .operation_lock
            .write()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let _ = self.drain_buffered_writes();
        let upper_namespace = self.core.upper_namespace_mutation();
        Ok(WriteOperation {
            _upper_namespace: upper_namespace,
            _operation: operation,
        })
    }

    // Caller holds the exclusive operation guard, so admission and handle
    // mappings cannot change while selecting and draining affected objects.
    fn fence_native_inode(&self, layer: Layer, inode: u64) -> IoFence {
        let handles = self.handles.lock().unwrap().iter().filter_map(|(id, handle)| {
            matches!(handle, Handle::File(file) if file.layer.0 == layer.0 && file.inode == inode).then_some(*id)
        }).collect();
        let fence = self.io_activity.fence(handles, false);
        fence.drain();
        fence
    }

    fn fence_paths(&self, paths: &[&Path]) -> IoFence {
        let handles = self.handles.lock().unwrap();
        if handles.is_empty() {
            return self.io_activity.fence(Vec::new(), false);
        }
        let nodes = self.nodes.lock().unwrap();
        let mut overlay_objects = BTreeSet::new();
        let mut native_objects = BTreeSet::new();
        for handle in handles.values() {
            if let Handle::File(h) = handle {
                // The namespace path can have moved while an opened backing
                // descriptor continues to own its original physical generation.
                if paths.iter().any(|path| {
                    Path::new(OsStr::from_bytes(&h.relative_path)).starts_with(path)
                        || nodes
                            .by_inode
                            .get(&h.overlay_inode)
                            .is_some_and(|p| p.starts_with(path))
                }) {
                    overlay_objects.insert(h.overlay_inode);
                    native_objects.insert((h.layer.0, h.inode));
                }
            }
        }
        drop(nodes);
        let ids: Vec<_> = handles
            .iter()
            .filter_map(|(id, handle)| match handle {
                Handle::File(h)
                    if overlay_objects.contains(&h.overlay_inode)
                        || native_objects.contains(&(h.layer.0, h.inode)) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        // Mark blocked under the handle-map lock: an I/O admission cannot
        // slip between selecting the IDs and closing those IDs to new leases.
        let fence = self.io_activity.fence(ids, false);
        drop(handles);
        fence.drain();
        fence
    }

    fn build(cfg: Config, inode_alloc: Arc<InodeAllocator>, restoring: bool) -> io::Result<Self> {
        let writeback_setting = std::env::var("PVISOR_VM_FS_WRITEBACK").ok();
        Self::build_with_writeback(cfg, inode_alloc, restoring, writeback_setting.as_deref())
    }

    fn build_with_writeback(
        cfg: Config,
        inode_alloc: Arc<InodeAllocator>,
        restoring: bool,
        writeback_setting: Option<&str>,
    ) -> io::Result<Self> {
        let shared_open_qualification = shared_open_qualification_enabled(
            std::env::var_os("PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION").as_deref(),
        )? && cfg!(target_os = "linux");
        if cfg.lower_dirs.is_empty() || (cfg.baseline_lower.is_some() && cfg.apply_target.is_none())
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let lowers = cfg.lower_dirs.iter().map(PathBuf::from).collect::<Vec<_>>();
        let upper = PathBuf::from(&cfg.upper_dir);
        let work = cfg.work_dir.as_ref().map(PathBuf::from);
        let preimages = cfg.preimage_dir.as_ref().map(PathBuf::from);
        let excluded = cfg.excluded_paths.iter().map(PathBuf::from).collect();
        let target = cfg
            .apply_target
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| lowers.last().unwrap().clone());
        let layout = pvisor_overlay_core::OverlayLayout::with_baseline(
            lowers.clone(),
            target,
            cfg.baseline_lower.as_deref().map(Path::new),
        )?
        .with_lower_mutability(cfg.lower_mutability.clone())?;
        let open = if restoring {
            OverlayCore::open_existing_for_layout
        } else {
            OverlayCore::new_for_layout_with_compact_preimages
        };
        let core = open(layout, upper.clone(), work, excluded, preimages)?;
        let core = FilesystemService::new(core).with_access_policy(&cfg.access_policy);
        let core = if let Some(index) = &cfg.baseline_content_index {
            core.with_immutable_content_index(&index.root, index.file.clone(), &index.sha256)?
        } else {
            core
        };

        let mut roots = Vec::with_capacity(lowers.len() + 1);
        roots.push(upper);
        roots.extend(lowers);
        let new_session_writeback = writeback_setting.is_some_and(|value| value != "0");
        // Unset allows old enabled snapshots to restore, while explicit "0"
        // retains the existing refusal. Native restore still checks every
        // saved mode, backing identity and handle before installing the state.
        let restore_capability = writeback_setting.map_or(restoring, |value| value != "0");
        let layers = roots
            .iter()
            .map(|root| {
                PassthroughFs::new(
                    passthrough::Config {
                        root_dir: root.to_string_lossy().into_owned(),
                        semantics: cfg.semantics,
                        attr_timeout: TTL,
                        entry_timeout: TTL,
                        // The merged stage is guest-owned. Writeback READs on
                        // O_WRONLY and guest-managed append use native support.
                        writeback: restore_capability,
                        ..Default::default()
                    },
                    inode_alloc.clone(),
                )
                .map(Arc::new)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut nodes = Nodes::default();
        nodes.by_inode.insert(fuse::ROOT_ID, PathBuf::new());
        nodes.by_path.insert(PathBuf::new(), fuse::ROOT_ID);
        nodes.ordered_paths.insert(PathBuf::new());
        let virtual_metadata = roots.iter().try_fold(false, |found, root| {
            backend::attributes(root).map(|attr| {
                found
                    || attr.is_some()
                    || pvisor_overlay_core::sys::get_xattr(
                        root,
                        OsStr::new("user.containers.override_stat"),
                    )
                    .is_ok()
            })
        })?;
        let profile = pvisor_overlay_core::profile::Profile::from_env("virtio-fs-overlay");
        profile.add(
            "shared_open_qualification_enabled",
            u64::from(shared_open_qualification),
        );
        #[cfg(target_os = "linux")]
        for label in passthrough::OPEN_QUALIFICATION_COUNTERS {
            profile.add(label, 0);
        }
        for label in [
            "negative_entry_cache_requested",
            "negative_entry_cache_enabled",
            "negative_entry_cache_replies",
            "negative_entry_cache_fallbacks",
        ] {
            profile.add(label, 0);
        }
        Ok(Self {
            lower_data_cache: false,
            lower_data_cache_configured: false,
            lower_data_cache_active: AtomicBool::new(false),
            lower_data_cache_seen: AtomicBool::new(false),
            lower_data_objects: Mutex::new(LowerDataObjects::default()),
            shared_open_qualification,
            negative_entry_cache: false,
            negative_entry_cache_configured: false,
            negative_entry_cache_seen: AtomicBool::new(false),
            negative_entry_cache_roots: Vec::new(),
            negative_entry_cache_protected_prefixes: Vec::new(),
            virtual_metadata,
            profile,
            operation_lock: RwLock::new(()),
            io_activity: Arc::new(IoActivity::default()),
            host_writeback: false,
            host_writeback_options: WriteBufferOptions::default(),
            host_writeback_qualification_lock: false,
            host_writeback_synchronous: false,
            write_buffer: RwLock::new(None),
            core,
            new_session_writeback,
            writeback_active: AtomicBool::new(false),
            immutable_data_cache: false,
            readonly_flush_elision: false,
            readonly_flush_elision_configured: false,
            readonly_flush_elision_active: AtomicBool::new(false),
            readonly_flush_elision_seen: AtomicBool::new(false),
            snapshot_config: cfg,
            roots,
            layers,
            directory_cache: Mutex::new(DirectoryCache::default()),
            directory_cache_capacity: DEFAULT_DIRECTORY_CACHE_CAPACITY,
            directory_cache_configured: false,
            inode_alloc,
            nodes: Mutex::new(nodes),
            detached_writeback: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        })
    }

    fn path(&self, inode: u64) -> io::Result<PathBuf> {
        self.nodes
            .lock()
            .unwrap()
            .by_inode
            .get(&inode)
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn child(&self, parent: u64, name: &CStr) -> io::Result<PathBuf> {
        OverlayCore::child(&self.path(parent)?, OsStr::from_bytes(name.to_bytes()))
            .map_err(linux_error)
    }

    fn allocate_inode(&self, path: PathBuf) -> u64 {
        let mut nodes = self.nodes.lock().unwrap();
        if let Some(inode) = nodes.by_path.get(&path) {
            return *inode;
        }
        let inode = self.inode_alloc.next();
        nodes.ordered_paths.insert(path.clone());
        nodes.by_path.insert(path.clone(), inode);
        nodes.by_inode.insert(inode, path);
        inode
    }

    fn acquire_guest_lookup(&self, inode: u64) {
        if !self.writeback_active.load(Ordering::Relaxed) || inode == fuse::ROOT_ID {
            return;
        }
        let mut nodes = self.nodes.lock().unwrap();
        if !nodes.legacy_lookup_refs.contains(&inode) {
            let count = nodes.lookup_refs.entry(inode).or_default();
            *count = count
                .checked_add(1)
                .expect("guest lookup reference overflow");
        }
    }

    fn forget_guest_lookup(&self, inode: u64, count: u64) {
        if count == 0 {
            return;
        }
        let retired = {
            let mut nodes = self.nodes.lock().unwrap();
            if nodes.legacy_lookup_refs.contains(&inode) {
                return;
            }
            let Some(current) = nodes.lookup_refs.get_mut(&inode) else {
                return;
            };
            if count < *current {
                *current -= count;
                return;
            }
            nodes.lookup_refs.remove(&inode);
            self.detached_writeback.lock().unwrap().remove(&inode)
        };
        if retired.is_some() {
            self.profile.add("guest_writeback_detached_released", 1);
        }
        // A native FORGET may close an fd. Never run it under either registry.
        drop(retired);
    }

    fn prepare_detached_writeback(
        &self,
        ctx: Context,
        path: &Path,
    ) -> io::Result<Option<(u64, Arc<DetachedWritebackInode>)>> {
        if !self.writeback_active.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let inode = {
            let nodes = self.nodes.lock().unwrap();
            let Some(&inode) = nodes.by_path.get(path) else {
                return Ok(None);
            };
            if !nodes.legacy_lookup_refs.contains(&inode)
                && nodes.lookup_refs.get(&inode).copied().unwrap_or(0) == 0
            {
                return Ok(None);
            }
            inode
        };
        let layer = self.layer(path)?;
        let entry = self.inner_entry(layer, path, ctx)?;
        let reference = DirectoryReference {
            fs: self.layers[layer.0].clone(),
            inode: entry.inode,
        };
        if entry.attr.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Ok(None);
        }
        let mut projected = entry.attr;
        self.backing_attributes(&self.roots[layer.0].join(path), &mut projected)?;
        let object = if layer.0 == 0 {
            DetachedWritebackInode::Upper {
                reference,
                projected,
                virtual_mode: projected.st_mode != entry.attr.st_mode,
            }
        } else {
            let mut attr = projected;
            attr.st_ino = inode as _;
            attr.st_nlink = attr.st_nlink.saturating_sub(1);
            drop(reference);
            DetachedWritebackInode::Lower(Mutex::new(attr))
        };
        Ok(Some((inode, Arc::new(object))))
    }

    fn commit_detached_writeback(&self, candidate: Option<(u64, Arc<DetachedWritebackInode>)>) {
        let Some((inode, object)) = candidate else {
            return;
        };
        // FORGET can run while the namespace mutation is in progress. Check
        // its final count atomically with publishing the retained identity.
        let nodes = self.nodes.lock().unwrap();
        if nodes.legacy_lookup_refs.contains(&inode)
            || nodes.lookup_refs.get(&inode).copied().unwrap_or(0) != 0
        {
            self.detached_writeback
                .lock()
                .unwrap()
                .insert(inode, object);
            self.profile.add("guest_writeback_detached_retained", 1);
        }
    }

    fn detached_attributes(
        &self,
        ctx: Context,
        inode: u64,
    ) -> io::Result<Option<(bindings::stat64, Duration)>> {
        let object = self.detached_writeback.lock().unwrap().get(&inode).cloned();
        let Some(object) = object else {
            return Ok(None);
        };
        let (mut attr, timeout) = match &*object {
            DetachedWritebackInode::Upper {
                reference,
                projected,
                virtual_mode,
            } => {
                let (mut attr, timeout) = reference.fs.getattr(ctx, reference.inode, None)?;
                if *virtual_mode {
                    attr.st_mode = projected.st_mode;
                }
                attr.st_uid = projected.st_uid;
                attr.st_gid = projected.st_gid;
                attr.st_rdev = projected.st_rdev;
                (attr, timeout)
            }
            DetachedWritebackInode::Lower(attr) => (*attr.lock().unwrap(), TTL),
        };
        attr.st_ino = inode as _;
        Ok(Some((attr, timeout)))
    }

    fn remove_path(&self, prefix: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let paths = nodes
            .ordered_paths
            .range(prefix.to_path_buf()..)
            .take_while(|path| path.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        self.profile.add("nodes_removed", paths.len() as u64);
        for path in paths {
            nodes.ordered_paths.remove(&path);
            if let Some(inode) = nodes.by_path.remove(&path) {
                nodes.by_inode.remove(&inode);
            }
        }
    }

    fn remap_path(&self, old: &Path, new: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let changes = nodes
            .ordered_paths
            .range(old.to_path_buf()..)
            .take_while(|path| path.starts_with(old))
            .map(|path| {
                let inode = nodes.by_path[path];
                let suffix = path.strip_prefix(old).unwrap();
                let replacement = if suffix.as_os_str().is_empty() {
                    new.to_path_buf()
                } else {
                    new.join(suffix)
                };
                (path.clone(), replacement, inode)
            })
            .collect::<Vec<_>>();
        self.profile.add("nodes_remapped", changes.len() as u64);
        for (old_path, _, _) in &changes {
            nodes.ordered_paths.remove(old_path);
            nodes.by_path.remove(old_path);
        }
        for (_, new_path, inode) in changes {
            nodes.ordered_paths.insert(new_path.clone());
            nodes.by_path.insert(new_path.clone(), inode);
            nodes.by_inode.insert(inode, new_path);
        }
    }

    fn layer(&self, path: &Path) -> io::Result<Layer> {
        let resolved = self
            .core
            .resolve(path)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        if resolved.is_upper {
            return Ok(Layer(0));
        }
        self.roots
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, root)| resolved.path == **root || resolved.path.starts_with(root))
            .map(|(index, _)| Layer(index))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn inner_entry(&self, layer: Layer, path: &Path, ctx: Context) -> io::Result<Entry> {
        self.inner_entry_with_parents(layer, path, ctx, &[])
    }

    fn cached_directory(
        &self,
        key: &(usize, PathBuf),
        identity: BackingIdentity,
    ) -> Option<Arc<DirectoryReference>> {
        let (reference, stale) = {
            let mut cache = self.directory_cache.lock().unwrap();
            cache.clock = cache.clock.saturating_add(1);
            let used = cache.clock;
            match cache.entries.get_mut(key) {
                Some(directory) if directory.identity == identity => {
                    directory.used = used;
                    (Some(directory.reference.clone()), None)
                }
                Some(_) => (None, cache.entries.remove(key)),
                None => (None, None),
            }
        };
        if stale.is_some() {
            self.profile.add("directory_cache_invalidations", 1);
        }
        // Native forget and close may take other locks; never run under the
        // directory-cache lock, including replacement and capacity eviction.
        drop(stale);
        reference
    }

    fn cache_directory(
        &self,
        key: (usize, PathBuf),
        identity: BackingIdentity,
        reference: Arc<DirectoryReference>,
    ) {
        let (replaced, evicted) = {
            let mut cache = self.directory_cache.lock().unwrap();
            cache.clock = cache.clock.saturating_add(1);
            let used = cache.clock;
            let replaced = cache.entries.insert(
                key,
                CachedDirectory {
                    identity,
                    reference,
                    used,
                },
            );
            let evicted = if cache.entries.len() > self.directory_cache_capacity {
                let oldest = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, directory)| directory.used)
                    .map(|(key, _)| key.clone())
                    .expect("nonempty directory cache");
                cache.entries.remove(&oldest)
            } else {
                None
            };
            (replaced, evicted)
        };
        if evicted.is_some() {
            self.profile.add("directory_cache_evictions", 1);
        }
        drop((replaced, evicted));
    }

    fn inner_entry_with_parents(
        &self,
        layer: Layer,
        path: &Path,
        ctx: Context,
        parents: &[BackingIdentity],
    ) -> io::Result<Entry> {
        match self.inner_lookup_with_parents(layer, path, ctx, parents, false)? {
            BackingLookup::Native(entry) => Ok(entry),
            #[cfg(target_os = "linux")]
            BackingLookup::RegularAttributes { .. } => Err(io::Error::from_raw_os_error(libc::EIO)),
        }
    }

    fn inner_lookup_with_parents(
        &self,
        layer: Layer,
        path: &Path,
        ctx: Context,
        parents: &[BackingIdentity],
        reply_attributes: bool,
    ) -> io::Result<BackingLookup> {
        let _span = self.profile.span("inner_inode");
        let fs = &self.layers[layer.0];
        let mut inode = fuse::ROOT_ID;
        let mut temporary_reference = false;
        let mut held_directory = None;
        let mut result = None;
        let mut prefix = PathBuf::new();
        #[cfg(not(target_os = "linux"))]
        let _ = reply_attributes;
        #[cfg(target_os = "linux")]
        let components = path.components().count();
        let cacheable = !parents.is_empty()
            && parents.len() + 1 == path.components().count()
            && parents
                .iter()
                .all(|id| cfg!(target_os = "macos") || id.mount_id.is_some());
        // Core freshly validated every physical ancestor. Keep a strong native
        // inode reference while using the deepest matching directory, without
        // holding the cache map lock across lookup, getattr or forget.
        let start = if cacheable {
            let key = (
                layer.0,
                path.parent().expect("validated parent count").to_path_buf(),
            );
            if let Some(reference) = self.cached_directory(&key, *parents.last().unwrap()) {
                inode = reference.inode;
                held_directory = Some(reference);
                self.profile.add("directory_cache_deep_hits", 1);
                parents.len()
            } else {
                0
            }
        } else {
            0
        };
        for (index, component) in path.components().enumerate().skip(start) {
            let key = if cacheable && index < parents.len() {
                prefix.push(component.as_os_str());
                Some((layer.0, prefix.clone()))
            } else {
                None
            };
            if let (Some(identity), Some(key)) = (parents.get(index), key.as_ref()) {
                if let Some(reference) = self.cached_directory(key, *identity) {
                    if temporary_reference {
                        fs.forget(ctx, inode, 1);
                    }
                    inode = reference.inode;
                    held_directory = Some(reference);
                    temporary_reference = false;
                    self.profile.add("directory_cache_hits", 1);
                    continue;
                }
                self.profile.add("directory_cache_misses", 1);
            }
            let name = CString::new(component.as_os_str().as_bytes())?;
            #[cfg(target_os = "linux")]
            if reply_attributes && index + 1 == components {
                let attributes = {
                    let _span = self.profile.span("backing_regular_attributes");
                    fs.regular_lookup_attributes(inode, &name)
                };
                match attributes {
                    Ok(Some((attr, attr_timeout, entry_timeout))) => {
                        if temporary_reference {
                            fs.forget(ctx, inode, 1);
                        }
                        drop(held_directory);
                        return Ok(BackingLookup::RegularAttributes {
                            attr,
                            attr_timeout,
                            entry_timeout,
                        });
                    }
                    Ok(None) => {} // Directory/symlink/replacement needs native lookup.
                    Err(error) => {
                        if temporary_reference {
                            fs.forget(ctx, inode, 1);
                        }
                        return Err(error);
                    }
                }
            }
            let entry = {
                let _span = self.profile.span("backing_lookup");
                fs.lookup(ctx, inode, &name)
            };
            if temporary_reference {
                fs.forget(ctx, inode, 1);
            }
            drop(held_directory.take()); // backing lookup no longer uses its parent
            let entry = entry?;
            inode = entry.inode;
            temporary_reference = inode != fuse::ROOT_ID;
            let matching_parent = parents.get(index).filter(|identity| {
                cacheable
                    && **identity
                        == BackingIdentity {
                            device: entry.attr.st_dev as _,
                            inode: entry.attr.st_ino as _,
                            mount_id: fs.lookup_mount_id(inode),
                        }
            });
            if let (Some(identity), Some(key)) = (matching_parent, key) {
                let reference = Arc::new(DirectoryReference {
                    fs: fs.clone(),
                    inode,
                });
                self.cache_directory(key, *identity, reference.clone());
                held_directory = Some(reference);
                temporary_reference = false; // ownership transferred to the Arc
            }
            result = Some(entry);
        }
        drop(held_directory);
        if let Some(entry) = result {
            return Ok(BackingLookup::Native(entry));
        }
        let (attr, timeout) = fs.getattr(ctx, fuse::ROOT_ID, None)?;
        Ok(BackingLookup::Native(Entry {
            inode: fuse::ROOT_ID,
            generation: 0,
            attr,
            attr_flags: 0,
            attr_timeout: timeout,
            entry_timeout: TTL,
        }))
    }

    fn inner_inode(&self, layer: Layer, path: &Path, ctx: Context) -> io::Result<u64> {
        Ok(self.inner_entry(layer, path, ctx)?.inode)
    }

    fn observed_inner(&self, ctx: Context, path: &Path) -> io::Result<(Layer, u64)> {
        let _io = self.fence_paths(&[path]);
        let backing = self
            .core
            .observe_read_for_backing_lookup(path)
            .map_err(linux_error)?;
        let layer = Layer(backing.entry.layer);
        let entry = self.inner_entry_with_parents(layer, path, ctx, &backing.parents)?;
        Ok((layer, entry.inode))
    }

    fn entry_on_backing(
        &self,
        ctx: Context,
        path: &Path,
        inode: u64,
        backing: &BackingResolution,
    ) -> io::Result<Entry> {
        let _span = self.profile.span("entry");
        let layer = Layer(backing.entry.layer);
        // Core still performs every namespace, alias and policy check. Its
        // path metadata is only a type hint: attributes must come from the
        // native parent's pinned FD, including after a host root replacement.
        let mut entry = match self.inner_lookup_with_parents(
            layer,
            path,
            ctx,
            &backing.parents,
            backing.entry.metadata.is_file(),
        )? {
            BackingLookup::Native(entry) => {
                if entry.inode != fuse::ROOT_ID {
                    self.layers[layer.0].forget(ctx, entry.inode, 1);
                }
                entry
            }
            #[cfg(target_os = "linux")]
            BackingLookup::RegularAttributes {
                attr,
                attr_timeout,
                entry_timeout,
            } => Entry {
                inode,
                generation: 0,
                attr,
                attr_flags: 0,
                attr_timeout,
                entry_timeout,
            },
        };
        self.backing_attributes(&backing.entry.resolved.path, &mut entry.attr)?;
        entry.inode = inode;
        entry.attr.st_ino = inode as _;
        Ok(entry)
    }

    fn entry(&self, ctx: Context, path: &Path, inode: u64) -> io::Result<Entry> {
        let backing = self
            .core
            .metadata_for_backing_lookup(path)
            .map_err(linux_error)?;
        self.entry_on_backing(ctx, path, inode, &backing)
    }

    fn backing_attributes(&self, path: &Path, attr: &mut bindings::stat64) -> io::Result<()> {
        let remote = backend::attributes(path).map_err(linux_error)?;
        if let Some(remote) = &remote {
            attr.st_mode = (remote.kind.mode() | u32::from(remote.perm)) as _;
            attr.st_uid = remote.uid;
            attr.st_gid = remote.gid;
            attr.st_nlink = remote.nlink as _;
            attr.st_size = remote.size as _;
            attr.st_blocks = remote.blocks as _;
            attr.st_rdev = remote.rdev as _;
            let (sec, nsec) = pvisor_overlay_core::sys::unix_timestamp(remote.atime);
            attr.st_atime = sec as _;
            attr.st_atime_nsec = nsec as _;
            let (sec, nsec) = pvisor_overlay_core::sys::unix_timestamp(remote.mtime);
            attr.st_mtime = sec as _;
            attr.st_mtime_nsec = nsec as _;
            let (sec, nsec) = pvisor_overlay_core::sys::unix_timestamp(remote.ctime);
            attr.st_ctime = sec as _;
            attr.st_ctime_nsec = nsec as _;
        }
        #[cfg(target_os = "linux")]
        if self.virtual_metadata && remote.is_none() {
            // Copy-up retains the source's virtual ownership when an unprivileged
            // host cannot chown the native upper. Native mode/size/times stay live.
            if let Ok(bytes) = pvisor_overlay_core::sys::get_xattr(
                path,
                OsStr::new("user.containers.override_stat"),
            ) {
                let identity = backend::UnixIdentity::parse(&bytes).map_err(linux_error)?;
                attr.st_uid = identity.uid;
                attr.st_gid = identity.gid;
                #[allow(clippy::unnecessary_cast)] // mode_t differs by platform
                let mode = if identity.mode & 0o170000 == 0 {
                    (attr.st_mode as u32 & 0o170000) | identity.mode
                } else {
                    identity.mode
                };
                attr.st_mode = mode as _;
            }
        }
        Ok(())
    }

    fn clear_directory_cache(&self) {
        let entries = std::mem::take(&mut self.directory_cache.lock().unwrap().entries);
        drop(entries); // final directory references release outside the map lock
    }

    fn writable_inner(&self, ctx: Context, path: &Path) -> io::Result<u64> {
        let _io = self.fence_paths(&[path]);
        // OverlayCore reports host errno; passthrough already reports Linux
        // errno. Convert only at the core boundary, never the whole request.
        self.core.copy_up(path).map_err(linux_error)?;
        self.inner_inode(Layer(0), path, ctx)
    }

    fn upper_parent(&self, ctx: Context, path: &Path) -> io::Result<(u64, CString)> {
        self.core
            .prepare_create(path)
            .inspect_err(|error| {
                if std::env::var("PVISOR_FS_PROFILE").as_deref() == Ok("1") {
                    log::error!("overlay create preparation failed: {error}");
                }
            })
            .map_err(linux_error)?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        Ok((
            self.inner_inode(Layer(0), parent, ctx)?,
            CString::new(name.as_bytes())?,
        ))
    }

    fn allocate_handle(&self, handle: Handle) -> u64 {
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(id, handle);
        id
    }

    fn with_file_handle<T>(
        &self,
        id: u64,
        f: impl FnOnce(&PassthroughFs, &FileHandle) -> io::Result<T>,
    ) -> io::Result<T> {
        // RELEASE shares the namespace read guard with metadata queries.
        // Admit the whole callback so it cannot lose its native mapping while
        // GETATTR or LSEEK is still using the opened object.
        let lease = self.file_io_lease(id)?;
        f(&self.layers[lease.handle.layer.0], &lease.handle)
    }

    fn file_io_lease(&self, id: u64) -> io::Result<FileIoLease> {
        loop {
            let handles = self.handles.lock().unwrap();
            let handle = match handles.get(&id) {
                Some(Handle::File(handle)) => handle.clone(),
                _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
            };
            let mut state = self.io_activity.state.lock().unwrap();
            if state.global != 0 || state.blocked.contains_key(&id) {
                drop(handles);
                // Wait without the operation or handle-map lock; after the
                // fence opens, revalidate the handle and its native mapping.
                state = self.io_activity.ready.wait(state).unwrap();
                drop(state);
                continue;
            }
            let native = self.layers[handle.layer.0].lease_handle(handle.inode, handle.handle)?;
            *state.active.entry(id).or_default() += 1;
            return Ok(FileIoLease {
                id,
                native,
                handle,
                activity: self.io_activity.clone(),
            });
        }
    }

    fn dtype(mode: u32) -> u32 {
        // POSIX file type occupies bits 12..15; FUSE d_type uses bits 0..3.
        (mode >> 12) & 0xf
    }
    fn open_native_for_reply(
        &self,
        ctx: Context,
        inode: u64,
        kill_priv: bool,
        flags: u32,
        share_data: bool,
    ) -> io::Result<NativeOpenReply> {
        let _span = self.profile.span("open");
        let writing = FilesystemService::is_write_open(flags as i32);
        // Read-only opens observe a stable namespace and insert handles under
        // their own mutex; they need not exclude independent backing reads.
        // Privilege changes, copy-up and every writable open remain exclusive.
        let readonly = !writing && !kill_priv;
        let _read_operation = if readonly {
            Some(self.read_operation()?)
        } else {
            None
        };
        let _write_operation = if readonly {
            None
        } else {
            Some(self.write_operation()?)
        };
        let path = self.path(inode)?;
        let _io = (!readonly).then(|| self.fence_paths(&[&path]));
        let (layer, inner, immutable_regular) = match self
            .core
            .prepare_open_for_backing_lookup(&path, flags as i32)
            .map_err(linux_error)?
        {
            OpenBacking::Writable(_) => (Layer(0), self.inner_inode(Layer(0), &path, ctx)?, false),
            OpenBacking::ReadOnly(backing) => {
                let layer = Layer(backing.entry.layer);
                let immutable_regular = backing.entry.metadata.is_file()
                    && layer.0.checked_sub(1).is_some_and(|index| {
                        self.snapshot_config.lower_mutability.get(index)
                            == Some(&pvisor_overlay_core::LayerMutability::Immutable)
                    });
                let inner = self
                    .inner_entry_with_parents(layer, &path, ctx, &backing.parents)?
                    .inode;
                (layer, inner, immutable_regular)
            }
        };
        let opened = self.layers[layer.0].open(ctx, inner, kill_priv, flags);
        let (handle, mut options) = match opened {
            Ok((Some(handle), options)) => (handle, options),
            result => {
                self.layers[layer.0].forget(ctx, inner, 1);
                return match result {
                    Err(error) => Err(error),
                    _ => Err(io::Error::from_raw_os_error(libc::EIO)),
                };
            }
        };
        // Keep OPEN's policy, baseline observation and native descriptor
        // checks. A writable/copy-up open must invalidate the merged inode's
        // old lower pages rather than reusing this read-only optimization.
        if self.immutable_data_cache && readonly && immutable_regular {
            options |= OpenOptions::KEEP_CACHE;
        }
        // The guest owns upper writes; reopening must not launder its dirty
        // pages one at a time through cache invalidation.
        if layer.0 == 0 && self.writeback_active.load(Ordering::Relaxed) {
            options |= OpenOptions::KEEP_CACHE;
        }
        if layer.0 != 0 && readonly && self.writeback_active.load(Ordering::Relaxed) {
            options |= OpenOptions::PVISOR_LOWER;
        }
        #[cfg(target_os = "linux")]
        let want_flush = self.readonly_flush_elision_active.load(Ordering::Relaxed)
            && readonly
            && immutable_regular;
        #[cfg(target_os = "linux")]
        if !share_data
            && want_flush
            && self.layers[layer.0].readonly_local_flush_eligible_profiled(
                inner,
                handle,
                &self.profile,
            )
        {
            options |= OpenOptions::PVISOR_LOCAL_FLUSH;
            self.profile.add("readonly_flush_elision_open_flags", 1);
        }
        if self.lower_data_cache_active.load(Ordering::Relaxed)
            && readonly
            && immutable_regular
            && !options.contains(OpenOptions::DIRECT_IO)
            && matches!(
                backend::attributes(&self.roots[layer.0].join(&path)),
                Ok(None)
            )
        {
            options |= OpenOptions::PVISOR_DATA_OBJECT;
        }
        let id = self.allocate_handle(Handle::File(Arc::new(FileHandle {
            overlay_inode: inode,
            layer,
            inode: inner,
            handle,
            relative_path: path.as_os_str().as_bytes().to_vec(),
        })));
        let reply = OpenReply::new(Some(id), options);
        #[cfg(target_os = "linux")]
        let mut reply = reply;
        #[cfg(target_os = "linux")]
        let mut data_identity = None;
        #[cfg(target_os = "linux")]
        if share_data {
            let local_flush = if reply.allows_data_object(true) {
                let qualification = self.layers[layer.0].qualify_readonly_open(
                    inner,
                    handle,
                    want_flush,
                    &self.profile,
                );
                data_identity = Some(qualification.data_identity);
                qualification.local_flush
            } else {
                want_flush
                    && self.layers[layer.0].readonly_local_flush_eligible_profiled(
                        inner,
                        handle,
                        &self.profile,
                    )
            };
            if local_flush {
                reply.options |= OpenOptions::PVISOR_LOCAL_FLUSH;
                self.profile.add("readonly_flush_elision_open_flags", 1);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = share_data;
        Ok(NativeOpenReply {
            reply,
            #[cfg(target_os = "linux")]
            data_identity,
        })
    }

    #[cfg(target_os = "linux")]
    fn lower_data_object_with_identity(
        &self,
        inode: u64,
        handle: u64,
        qualification: Option<Option<passthrough::LowerDataIdentity>>,
    ) -> Option<LowerDataObject> {
        if !self.lower_data_cache_active.load(Ordering::Relaxed) {
            return None;
        }
        let receipt = (|| {
            let lease = self.file_io_lease(handle).ok()?;
            let h = &lease.handle;
            if h.overlay_inode != inode
                || h.layer
                    .0
                    .checked_sub(1)
                    .and_then(|i| self.snapshot_config.lower_mutability.get(i))
                    != Some(&pvisor_overlay_core::LayerMutability::Immutable)
            {
                return None;
            }
            // Remote-content projections are not identified by the native
            // placeholder's physical inode. Unrelated virtual files do not
            // affect this selected entry's qualification.
            let physical = self.roots[h.layer.0].join(OsStr::from_bytes(&h.relative_path));
            if !matches!(backend::attributes(&physical), Ok(None)) {
                return None;
            }
            let identity = match qualification {
                Some(identity) => {
                    self.profile.add("open_qualification_receipt_hits", 1);
                    identity?
                }
                None => {
                    self.profile.add("open_qualification_receipt_misses", 1);
                    self.layers[h.layer.0].lower_data_identity_profiled(
                        h.inode,
                        h.handle,
                        &self.profile,
                    )?
                }
            };
            let key = (
                h.layer.0,
                identity.dev,
                identity.ino,
                identity.mount_id,
                identity.size,
            );
            match self.lower_data_objects.lock().unwrap().register(key) {
                Some((receipt, registered)) => {
                    self.profile.add(
                        if registered {
                            "lower_data_cache_object_registrations"
                        } else {
                            "lower_data_cache_object_reuses"
                        },
                        1,
                    );
                    Some(receipt)
                }
                None => {
                    self.profile.add("lower_data_cache_capacity_rejections", 1);
                    None
                }
            }
        })();
        if receipt.is_none() {
            self.profile.add("lower_data_cache_object_rejections", 1);
        } else {
            self.profile.add("lower_data_cache_open_flags", 1);
        }
        receipt
    }
}

impl FileSystem for OverlayFs {
    fn diagnostic_profiles(&self) -> Vec<pvisor_overlay_core::profile::ProfileReport> {
        self.profile
            .report()
            .into_iter()
            .chain(self.core.profile_report())
            .collect()
    }

    fn supports_directory_image(&self) -> bool {
        !self.snapshot_config.lower_mutability.is_empty()
            && self
                .snapshot_config
                .lower_mutability
                .iter()
                .all(|mutability| *mutability == pvisor_overlay_core::LayerMutability::Immutable)
    }

    fn capture_state(&self) -> io::Result<super::snapshot::FsSnapshot> {
        if self.lower_data_cache_seen.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshots of owners that negotiated lower-data-object sessions are unsupported",
            ));
        }

        if self.readonly_flush_elision_active.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshots of negotiated read-only flush-elision sessions are unsupported",
            ));
        }
        let _operation = self.write_operation().unwrap();
        if self.negative_entry_cache_seen.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshots after cacheable negative-entry replies are unsupported",
            ));
        }
        let _io = self.io_activity.fence(Vec::new(), true);
        _io.drain();
        self.drain_buffered_writes()?;
        if !self.detached_writeback.lock().unwrap().is_empty() {
            return Err(super::snapshot::unsupported(
                "detached guest-writeback identities must reach FORGET before snapshot",
            ));
        }
        self.core.sync_preimages()?;
        for root in self.roots.iter().skip(1) {
            backend::materialize_tree(root)?;
        }
        self.clear_directory_cache();
        self.profile.emit_checkpoint();
        self.core.emit_profile_checkpoint();
        let hard_links = self.core.capture_hard_links()?;
        let sources = self
            .core
            .capture_hard_link_sources()?
            .into_iter()
            .map(|(dev, ino, path)| ((dev, ino), path))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut hard_link_origins = Vec::new();
        let mut missing = std::collections::BTreeSet::new();
        for (dev, ino, _) in &hard_links {
            let Some(path) = sources.get(&(*dev, *ino)) else {
                missing.insert((*dev, *ino));
                continue;
            };
            let (layer, root) = self
                .roots
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, root)| path.starts_with(root))
                .ok_or_else(|| super::snapshot::invalid("hard-link source escaped lower"))?;
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_file()
                || metadata.nlink() < 2
                || (metadata.dev(), metadata.ino()) != (*dev, *ino)
            {
                return Err(super::snapshot::invalid("hard-link source changed"));
            }
            hard_link_origins.push((
                *dev,
                *ino,
                layer,
                path.strip_prefix(root)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
            ));
        }
        // Legacy snapshots lack origin hints. Preserve their existing full-copy
        // capture semantics; new captures/restores use copy-up-time hints above.
        for (layer, root) in self.roots.iter().enumerate().skip(1) {
            use std::os::unix::fs::MetadataExt;
            let mut directories = vec![root.clone()];
            while !missing.is_empty() && !directories.is_empty() {
                for entry in std::fs::read_dir(directories.pop().unwrap())? {
                    let path = entry?.path();
                    let metadata = std::fs::symlink_metadata(&path)?;
                    if missing.is_empty() {
                        break;
                    }
                    if metadata.is_dir() {
                        directories.push(path);
                    } else if metadata.is_file()
                        && missing.remove(&(metadata.dev(), metadata.ino()))
                    {
                        hard_link_origins.push((
                            metadata.dev(),
                            metadata.ino(),
                            layer,
                            path.strip_prefix(root)
                                .map_err(|_| {
                                    super::snapshot::invalid("hard-link origin escaped lower")
                                })?
                                .as_os_str()
                                .as_bytes()
                                .to_vec(),
                        ));
                    }
                }
            }
        }
        if !missing.is_empty() {
            return Err(super::snapshot::unsupported(
                "overlay hard-link source is no longer owned",
            ));
        }
        let (saved_nodes, lookup_refs, legacy_lookup_refs) = {
            let nodes = self.nodes.lock().unwrap();
            if nodes
                .lookup_refs
                .keys()
                .chain(nodes.legacy_lookup_refs.iter())
                .any(|inode| !nodes.by_inode.contains_key(inode))
            {
                return Err(super::snapshot::unsupported(
                    "unlinked guest lookup identities must reach FORGET before snapshot",
                ));
            }
            (
                nodes
                    .by_inode
                    .iter()
                    .map(|(inode, path)| (*inode, path.as_os_str().as_bytes().to_vec()))
                    .collect(),
                self.writeback_active.load(Ordering::Relaxed).then(|| {
                    nodes
                        .lookup_refs
                        .iter()
                        .map(|(inode, count)| (*inode, *count))
                        .collect()
                }),
                nodes.legacy_lookup_refs.iter().copied().collect(),
            )
        };
        Ok(super::snapshot::FsSnapshot::Overlay(Box::new(
            OverlaySnapshot {
                config: self.snapshot_config.clone(),
                hard_links,
                hard_link_origins,
                layers: self
                    .layers
                    .iter()
                    .map(|layer| layer.capture_state())
                    .collect::<io::Result<_>>()?,
                nodes: saved_nodes,
                lookup_refs,
                legacy_lookup_refs,
                handles: self
                    .handles
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, handle)| (*id, handle.clone()))
                    .collect(),
                next_handle: self.next_handle.load(Ordering::Relaxed),
            },
        )))
    }
    fn restore_state(&self, state: &super::snapshot::FsSnapshot) -> io::Result<()> {
        if !self.detached_writeback.lock().unwrap().is_empty() {
            return Err(super::snapshot::unsupported(
                "cannot replace a session with retained detached writeback identities",
            ));
        }
        if self.lower_data_cache_seen.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshots of owners that negotiated lower-data-object sessions are unsupported",
            ));
        }

        if self.readonly_flush_elision_active.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot replace a negotiated read-only flush-elision session by restoring state",
            ));
        }
        use super::snapshot::{invalid, FsSnapshot};
        let FsSnapshot::Overlay(state) = state else {
            return Err(invalid("overlay filesystem type mismatch"));
        };
        let _operation = self.write_operation().unwrap();
        if self.negative_entry_cache_seen.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot restore an owner after cacheable negative-entry replies",
            ));
        }
        if self.negative_entry_cache
            && state
                .layers
                .iter()
                .any(|saved| matches!(saved, FsSnapshot::Passthrough(layer) if layer.writeback))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "negative entry cache cannot restore a writeback-enabled session",
            ));
        }
        if self.host_writeback
            && state
                .layers
                .iter()
                .any(|saved| matches!(saved, FsSnapshot::Passthrough(layer) if layer.writeback))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "host writeback cannot restore a guest-writeback session",
            ));
        }
        let _io = self.io_activity.fence(Vec::new(), true);
        _io.drain();
        self.drain_buffered_writes()?;
        if state.config != self.snapshot_config
            || state.layers.len() != self.layers.len()
            || state.next_handle == 0
            || state.next_handle == u64::MAX
        {
            return Err(invalid("overlay topology or handle allocator mismatch"));
        }
        let mut nodes = Nodes::default();
        for (inode, path) in &state.nodes {
            let path = PathBuf::from(OsStr::from_bytes(path));
            nodes.ordered_paths.insert(path.clone());
            if *inode == 0
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                || nodes.by_inode.insert(*inode, path.clone()).is_some()
                || nodes.by_path.insert(path, *inode).is_some()
            {
                return Err(invalid("invalid overlay inode paths"));
            }
        }
        if let Some(refs) = &state.lookup_refs {
            for &(inode, count) in refs {
                if count == 0
                    || !nodes.by_inode.contains_key(&inode)
                    || nodes.lookup_refs.insert(inode, count).is_some()
                {
                    return Err(invalid("invalid guest lookup references"));
                }
            }
            for &inode in &state.legacy_lookup_refs {
                if !nodes.by_inode.contains_key(&inode)
                    || nodes.lookup_refs.contains_key(&inode)
                    || !nodes.legacy_lookup_refs.insert(inode)
                {
                    return Err(invalid("invalid legacy guest lookup references"));
                }
            }
        } else if matches!(state.layers.first(), Some(FsSnapshot::Passthrough(layer)) if layer.writeback)
        {
            // Legacy snapshots did not save guest nlookup. Their finite inode
            // set remains conservative; newly created nodes are counted exactly.
            nodes
                .legacy_lookup_refs
                .extend(nodes.by_inode.keys().copied());
        }
        let mut handles = HashMap::new();
        for (id, handle) in &state.handles {
            if *id == 0 || *id >= state.next_handle || handles.insert(*id, handle.clone()).is_some()
            {
                return Err(invalid("invalid overlay handle"));
            }
            match handle {
                Handle::File(file) => {
                    OverlayCore::validate_rel(Path::new(OsStr::from_bytes(&file.relative_path)))
                        .map_err(|_| invalid("invalid file handle relative path"))?;
                    let Some(FsSnapshot::Passthrough(layer)) = state.layers.get(file.layer.0)
                    else {
                        return Err(invalid("invalid file handle layer"));
                    };
                    if !nodes.by_inode.contains_key(&file.overlay_inode)
                        || !layer
                            .handles
                            .iter()
                            .any(|h| h.handle == file.handle && h.inode == file.inode)
                    {
                        return Err(invalid("overlay handle without backing inode"));
                    }
                }
                Handle::DirectoryNames {
                    parent,
                    names,
                    types,
                } => {
                    if !nodes.by_inode.contains_key(parent)
                        || (!types.is_empty() && types.len() != names.len())
                        || types
                            .iter()
                            .any(|type_| !matches!(*type_, 0 | 1 | 2 | 4 | 6 | 8 | 10 | 12))
                        || names.iter().any(|name| {
                            name.is_empty()
                                || name.contains(&0)
                                || name.contains(&b'/')
                                || name == b"."
                                || name == b".."
                        })
                    {
                        return Err(invalid("invalid lazy directory cookie"));
                    }
                }
                Handle::Directory(entries) => {
                    if entries
                        .iter()
                        .any(|e| e.name.is_empty() || e.name.contains(&0) || e.name.contains(&b'/'))
                    {
                        return Err(invalid("invalid overlay directory cookie"));
                    }
                }
            }
        }
        self.clear_directory_cache();
        for (layer, saved) in self.layers.iter().zip(&state.layers) {
            layer.restore_state(saved)?;
        }
        self.writeback_active.store(
            matches!(state.layers.first(), Some(FsSnapshot::Passthrough(layer)) if layer.writeback),
            Ordering::Relaxed,
        );
        self.core.restore_hard_links(&state.hard_links)?;
        let sources = state
            .hard_link_origins
            .iter()
            .map(|(dev, ino, layer, relative)| {
                let root = self
                    .roots
                    .get(*layer)
                    .filter(|_| *layer > 0)
                    .ok_or_else(|| invalid("invalid lower hard-link source"))?;
                Ok((*dev, *ino, super::snapshot::relative_path(root, relative)?))
            })
            .collect::<io::Result<Vec<_>>>()?;
        self.core.restore_hard_link_sources(&sources)?;
        *self.nodes.lock().unwrap() = nodes;
        *self.handles.lock().unwrap() = handles;
        self.next_handle.store(state.next_handle, Ordering::Relaxed);
        Ok(())
    }

    type Inode = u64;
    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        let _operation = self.write_operation()?;
        if self.host_writeback {
            let mut buffer = self.write_buffer.write().unwrap();
            if buffer.is_none() {
                *buffer = Some(Arc::new(WriteBuffer::with_profile(
                    self.host_writeback_options,
                    self.profile.clone(),
                )?));
            }
        }
        let mut capable = capable;
        if !self.new_session_writeback {
            capable.remove(FsOptions::WRITEBACK_CACHE);
        }
        let mut options = None;
        for layer in &self.layers {
            let layer_options = layer.init(capable)?;
            options = Some(options.map_or(layer_options, |current| current & layer_options));
        }
        let mut options = options.unwrap_or_else(FsOptions::empty);
        // Directory handles and enumeration belong to OverlayFs, not native
        // layers. macOS passthrough does not advertise these capabilities.
        let directory_options = FsOptions::DO_READDIRPLUS | FsOptions::READDIRPLUS_AUTO;
        options.remove(directory_options);
        if capable.contains(FsOptions::DO_READDIRPLUS) {
            options |= capable & directory_options;
        }
        if options.contains(FsOptions::WRITEBACK_CACHE) {
            options |= capable & FsOptions::PVISOR_WRITEBACK;
        }
        let local_flush = self.readonly_flush_elision
            && capable.contains(FsOptions::PVISOR_LOCAL_FLUSH)
            && !options.intersects(
                FsOptions::WRITEBACK_CACHE
                    | FsOptions::POSIX_LOCKS
                    | FsOptions::FLOCK_LOCKS
                    | FsOptions::HAS_INODE_DAX
                    | FsOptions::MAP_ALIGNMENT,
            );
        if self.readonly_flush_elision_active.load(Ordering::Relaxed) && !local_flush {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot remove a negotiated read-only flush contract from a live session",
            ));
        }
        if local_flush {
            options |= FsOptions::PVISOR_LOCAL_FLUSH;
            self.readonly_flush_elision_active
                .store(true, Ordering::Relaxed);
            if !self
                .readonly_flush_elision_seen
                .swap(true, Ordering::Relaxed)
            {
                self.profile.add("readonly_flush_elision_enabled", 1);
            }
        }
        let data_objects = self.lower_data_cache
            && capable.contains(FsOptions::PVISOR_DATA_OBJECT)
            && !options.intersects(
                FsOptions::WRITEBACK_CACHE
                    | FsOptions::POSIX_LOCKS
                    | FsOptions::FLOCK_LOCKS
                    | FsOptions::HAS_INODE_DAX
                    | FsOptions::MAP_ALIGNMENT,
            );
        if self.lower_data_cache_active.load(Ordering::Relaxed) && !data_objects {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot remove a negotiated lower-data-object contract from a live session",
            ));
        }
        if data_objects {
            options |= FsOptions::PVISOR_DATA_OBJECT;
            self.lower_data_cache_active.store(true, Ordering::Relaxed);
            if !self.lower_data_cache_seen.swap(true, Ordering::Relaxed) {
                self.profile.add("lower_data_cache_enabled", 1);
            }
        }
        self.profile.add(
            "guest_writeback_enabled",
            u64::from(options.contains(FsOptions::WRITEBACK_CACHE)),
        );
        self.profile.add(
            "guest_batched_writeback_enabled",
            u64::from(options.contains(FsOptions::PVISOR_WRITEBACK)),
        );
        self.writeback_active.store(
            options.contains(FsOptions::WRITEBACK_CACHE),
            Ordering::Relaxed,
        );
        Ok(options)
    }

    fn destroy(&self) {
        let _operation = self.write_operation().unwrap();
        let _io = self.io_activity.fence(Vec::new(), true);
        _io.drain();
        if let Some(buffer) = self.write_buffer.write().unwrap().take() {
            if let Err(error) = buffer.stop() {
                log::error!("host writeback teardown failed: {error}");
            }
        }
        self.clear_directory_cache();
        self.handles.lock().unwrap().clear();
        self.detached_writeback.lock().unwrap().clear();
        {
            let mut nodes = self.nodes.lock().unwrap();
            nodes.lookup_refs.clear();
            nodes.legacy_lookup_refs.clear();
        }
        for layer in &self.layers {
            layer.destroy();
        }
        self.readonly_flush_elision_active
            .store(false, Ordering::Relaxed);
        self.lower_data_cache_active.store(false, Ordering::Relaxed);
    }

    fn lookup(&self, ctx: Context, parent: u64, name: &CStr) -> io::Result<Entry> {
        let _span = self.profile.span("lookup");
        let _operation = self.read_operation()?;
        let path = self.child(parent, name)?;
        let backing = if self.negative_entry_cache && self.negative_entry_cache_path_eligible(&path)
        {
            match self
                .core
                .metadata_for_backing_lookup_optional(&path)
                .map_err(linux_error)?
            {
                Some(backing) => backing,
                None => {
                    self.negative_entry_cache_seen
                        .store(true, Ordering::Relaxed);
                    self.profile.add("negative_entry_cache_replies", 1);
                    return Ok(Entry {
                        inode: 0,
                        generation: 0,
                        // No inode or lookup reference belongs to this name.
                        // SAFETY: stat64 is scalar POD; negative replies do
                        // not instantiate or consume these zero attributes.
                        attr: unsafe { std::mem::zeroed() },
                        attr_flags: 0,
                        attr_timeout: Duration::ZERO,
                        entry_timeout: TTL,
                    });
                }
            }
        } else {
            if self.negative_entry_cache {
                self.profile.add("negative_entry_cache_fallbacks", 1);
            }
            self.core
                .metadata_for_backing_lookup(&path)
                .map_err(linux_error)?
        };
        let inode = self.allocate_inode(path.clone());
        let entry = self.entry_on_backing(ctx, &path, inode, &backing)?;
        self.acquire_guest_lookup(inode);
        Ok(entry)
    }

    fn forget(&self, _ctx: Context, inode: u64, count: u64) {
        self.forget_guest_lookup(inode, count);
    }

    fn getattr(
        &self,
        ctx: Context,
        inode: u64,
        handle: Option<u64>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let _span = self.profile.span("getattr");
        let _operation = self.read_operation()?;
        // GETATTR may omit FH (e.g. stat through /proc/self/fd). An unlinked
        // inode still belongs to its open file, even after its path is gone.
        let (handle, path) = match handle {
            Some(handle) => (Some(handle), None),
            None => match self.path(inode) {
                Ok(path) => (None, Some(path)),
                Err(error) => {
                    if let Some(attributes) = self.detached_attributes(ctx, inode)? {
                        return Ok(attributes);
                    }
                    // Only detached inodes require a linear scan of open handles.
                    let handle = self.handles.lock().unwrap().iter().find_map(|(id, h)| {
                        matches!(h, Handle::File(h) if h.overlay_inode == inode).then_some(*id)
                    });
                    match handle {
                        Some(handle) => (Some(handle), None),
                        None => return Err(error),
                    }
                }
            },
        };
        if let Some(handle) = handle {
            return self.with_file_handle(handle, |fs, h| {
                if h.overlay_inode != inode {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                let (mut attr, timeout) = fs.getattr(ctx, h.inode, Some(h.handle))?;
                let path = self.roots[h.layer.0].join(OsStr::from_bytes(&h.relative_path));
                #[allow(clippy::unnecessary_cast)] // native dev_t differs by platform
                let same_object = h.layer.0 != 0
                    || !self.virtual_metadata
                    || std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
                        metadata.ino() == attr.st_ino as u64 && metadata.dev() == attr.st_dev as u64
                    });
                if same_object {
                    self.backing_attributes(&path, &mut attr)?;
                }
                attr.st_ino = inode as _;
                Ok((attr, timeout))
            });
        }
        let entry = self.entry(ctx, &path.expect("GETATTR resolved the inode path"), inode)?;
        Ok((entry.attr, entry.attr_timeout))
    }

    fn setattr(
        &self,
        ctx: Context,
        inode: u64,
        attr: bindings::stat64,
        handle: Option<u64>,
        valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let _operation = self.write_operation()?;
        // ftruncate operates on the opened object even after unlink/rename.
        // Writable opens have already copied the object into the upper layer.
        // Never resolve a replacement at the old name or mutate a lower FD.
        if let Some(handle) = handle.filter(|_| {
            valid.contains(SetattrValid::SIZE)
                && !valid.intersects(SetattrValid::MODE | SetattrValid::UID | SetattrValid::GID)
        }) {
            let h = match self.handles.lock().unwrap().get(&handle) {
                Some(Handle::File(h)) if h.overlay_inode == inode && h.layer.0 == 0 => h.clone(),
                _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
            };
            // Drain every open alias of this native object before changing size.
            // The exclusive operation guard keeps its mapping alive through setattr.
            let _io = self.fence_native_inode(h.layer, h.inode);
            let fs = &self.layers[h.layer.0];
            let (mut attr, timeout) = fs.setattr(ctx, h.inode, attr, Some(h.handle), valid)?;
            let path = self.roots[0].join(OsStr::from_bytes(&h.relative_path));
            #[allow(clippy::unnecessary_cast)] // native dev_t differs by platform
            let same_object = std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
                metadata.ino() == attr.st_ino as u64 && metadata.dev() == attr.st_dev as u64
            });
            if same_object {
                self.backing_attributes(&path, &mut attr)?;
            }
            attr.st_ino = inode as _;
            return Ok((attr, timeout));
        }
        let path = match self.path(inode) {
            Ok(path) => path,
            Err(error) => {
                // Writeback ctime/mtime may follow unlink or replacement. Never
                // resolve its former name, which could now name a different file.
                if valid.is_empty() || !(SetattrValid::MTIME | SetattrValid::CTIME).contains(valid)
                {
                    return Err(error);
                }
                let object = self.detached_writeback.lock().unwrap().get(&inode).cloned();
                if handle.is_none() {
                    if let Some(object) = object {
                        let (mut updated, timeout) = match &*object {
                            DetachedWritebackInode::Upper {
                                reference,
                                projected,
                                virtual_mode,
                            } => {
                                let _io = self.fence_native_inode(Layer(0), reference.inode);
                                let (mut updated, timeout) = reference.fs.setattr(
                                    ctx,
                                    reference.inode,
                                    attr,
                                    None,
                                    valid,
                                )?;
                                if *virtual_mode {
                                    updated.st_mode = projected.st_mode;
                                }
                                updated.st_uid = projected.st_uid;
                                updated.st_gid = projected.st_gid;
                                updated.st_rdev = projected.st_rdev;
                                (updated, timeout)
                            }
                            DetachedWritebackInode::Lower(view) => {
                                let mut view = view.lock().unwrap();
                                if valid.contains(SetattrValid::MTIME) {
                                    view.st_mtime = attr.st_mtime;
                                    view.st_mtime_nsec = attr.st_mtime_nsec;
                                }
                                if valid.contains(SetattrValid::CTIME) {
                                    view.st_ctime = attr.st_ctime;
                                    view.st_ctime_nsec = attr.st_ctime_nsec;
                                }
                                (*view, TTL)
                            }
                        };
                        updated.st_ino = inode as _;
                        return Ok((updated, timeout));
                    }
                }
                let handles = self.handles.lock().unwrap();
                let id = handle.or_else(|| handles.iter().find_map(|(id, h)| {
                    matches!(h, Handle::File(h) if h.overlay_inode == inode && h.layer.0 == 0).then_some(*id)
                })).ok_or(error)?;
                let h = match handles.get(&id) {
                    Some(Handle::File(h)) if h.overlay_inode == inode && h.layer.0 == 0 => {
                        h.clone()
                    }
                    _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
                };
                drop(handles);
                let _io = self.io_activity.fence(vec![id], false);
                _io.drain();
                let (mut attr, timeout) =
                    self.layers[0].setattr(ctx, h.inode, attr, Some(h.handle), valid)?;
                attr.st_ino = inode as _;
                return Ok((attr, timeout));
            }
        };
        let _io = self.fence_paths(&[&path]);
        self.core
            .prepare_metadata_change(&path)
            .map_err(linux_error)?;
        let inner = self.writable_inner(ctx, &path)?;
        #[cfg(target_os = "linux")]
        let previous = if self.virtual_metadata {
            Some(self.entry(ctx, &path, inode)?.attr)
        } else {
            None
        };
        let requested = attr;
        let result = self.layers[0].setattr(ctx, inner, attr, None, valid);
        self.layers[0].forget(ctx, inner, 1);
        let (mut attr, timeout) = result?;
        #[cfg(target_os = "linux")]
        if let Some(previous) = previous {
            if pvisor_overlay_core::sys::get_xattr(
                &self.core.upper_path(&path),
                OsStr::new("user.containers.override_stat"),
            )
            .is_ok()
            {
                if !valid.contains(SetattrValid::MODE) {
                    attr.st_mode = previous.st_mode;
                }
                attr.st_uid = if valid.contains(SetattrValid::UID) {
                    requested.st_uid
                } else {
                    previous.st_uid
                };
                attr.st_gid = if valid.contains(SetattrValid::GID) {
                    requested.st_gid
                } else {
                    previous.st_gid
                };
                let value = format!("{}:{}:0{:o}", attr.st_uid, attr.st_gid, attr.st_mode);
                pvisor_overlay_core::sys::set_xattr(
                    &self.core.upper_path(&path),
                    OsStr::new("user.containers.override_stat"),
                    value.as_bytes(),
                    0,
                )
                .map_err(linux_error)?;
            }
        }
        #[cfg(target_os = "macos")]
        let _ = requested;
        attr.st_ino = inode as _;
        Ok((attr, timeout))
    }

    fn readlink(&self, ctx: Context, inode: u64) -> io::Result<Vec<u8>> {
        let _operation = self.write_operation()?;
        let path = self.path(inode)?;
        let (layer, inner) = self.observed_inner(ctx, &path)?;
        let result = self.layers[layer.0].readlink(ctx, inner);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn symlink(
        &self,
        ctx: Context,
        linkname: &CStr,
        parent: u64,
        name: &CStr,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        let created = self.layers[0].symlink(ctx, linkname, upper_parent, &upper_name, extensions);
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let created = created?;
        self.layers[0].forget(ctx, created.inode, 1);
        let inode = self.allocate_inode(path.clone());
        let entry = self.entry(ctx, &path, inode)?;
        self.acquire_guest_lookup(inode);
        Ok(entry)
    }

    fn mknod(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        rdev: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        let created = self.layers[0].mknod(
            ctx,
            upper_parent,
            &upper_name,
            mode,
            rdev,
            umask,
            extensions,
        );
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let created = created?;
        self.layers[0].forget(ctx, created.inode, 1);
        let inode = self.allocate_inode(path.clone());
        let entry = self.entry(ctx, &path, inode)?;
        self.acquire_guest_lookup(inode);
        Ok(entry)
    }

    fn mkdir(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        #[cfg(target_os = "linux")]
        let virtual_parent = if self.virtual_metadata {
            Some(self.entry(ctx, &self.path(parent)?, parent)?.attr)
        } else {
            None
        };
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        let created = self.layers[0]
            .mkdir(ctx, upper_parent, &upper_name, mode, umask, extensions)
            .inspect_err(|error| log::error!("overlay native mkdir failed: {error}"));
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let created = created?;
        self.layers[0].forget(ctx, created.inode, 1);
        #[cfg(target_os = "linux")]
        if let Some(parent_attr) = virtual_parent {
            // Rootless OCI ownership is virtual: native mkdir may use the host
            // UID. Record the guest creator and virtual setgid inheritance.
            let inherit_group = parent_attr.st_mode & libc::S_ISGID != 0;
            let gid = if inherit_group {
                parent_attr.st_gid
            } else {
                ctx.gid
            };
            let mode = created.attr.st_mode | if inherit_group { libc::S_ISGID } else { 0 };
            let identity = format!("{}:{}:0{:o}", ctx.uid, gid, mode);
            pvisor_overlay_core::sys::set_xattr(
                &self.core.upper_path(&path),
                OsStr::new("user.containers.override_stat"),
                identity.as_bytes(),
                0,
            )
            .map_err(linux_error)?;
        }
        let inode = self.allocate_inode(path.clone());
        let entry = self.entry(ctx, &path, inode)?;
        self.acquire_guest_lookup(inode);
        Ok(entry)
    }

    fn unlink(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        let _io = self.fence_paths(&[&path]);
        let retired = self.prepare_detached_writeback(_ctx, &path)?;
        self.core.remove(&path, false).map_err(linux_error)?;
        self.commit_detached_writeback(retired);
        self.remove_path(&path);
        Ok(())
    }

    fn rmdir(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        let _io = self.fence_paths(&[&path]);
        self.core.remove(&path, true).map_err(linux_error)?;
        self.remove_path(&path);
        Ok(())
    }

    fn rename(
        &self,
        _ctx: Context,
        olddir: u64,
        oldname: &CStr,
        newdir: u64,
        newname: &CStr,
        flags: u32,
    ) -> io::Result<()> {
        let _operation = self.write_operation()?;
        let old = self.child(olddir, oldname)?;
        let new = self.child(newdir, newname)?;
        let _io = self.fence_paths(&[&old, &new]);
        let retired = if flags == 0 && old != new && self.core.resolve(&new).is_some() {
            self.prepare_detached_writeback(_ctx, &new)?
        } else {
            None
        };
        match flags {
            0 => self.core.rename(&old, &new, false).map_err(linux_error)?,
            RENAME_NOREPLACE => self.core.rename(&old, &new, true).map_err(linux_error)?,
            RENAME_EXCHANGE => self.core.exchange(&old, &new).map_err(linux_error)?,
            _ => return Err(linux_error(io::Error::from_raw_os_error(libc::ENOTSUP))),
        }
        self.commit_detached_writeback(retired);
        if flags == RENAME_EXCHANGE {
            let marker = PathBuf::from(format!(".pvisor-exchange-{}", self.inode_alloc.next()));
            self.remap_path(&old, &marker);
            self.remap_path(&new, &old);
            self.remap_path(&marker, &new);
        } else {
            self.remove_path(&new);
            self.remap_path(&old, &new);
        }
        Ok(())
    }

    fn link(&self, ctx: Context, inode: u64, newparent: u64, newname: &CStr) -> io::Result<Entry> {
        let _operation = self.write_operation()?;
        let source = self.path(inode)?;
        let destination = self.child(newparent, newname)?;
        let _io = self.fence_paths(&[&source, &destination]);
        self.core
            .hard_link(&source, &destination)
            .map_err(linux_error)?;
        let new_inode = self.allocate_inode(destination.clone());
        let entry = self.entry(ctx, &destination, new_inode)?;
        self.acquire_guest_lookup(new_inode);
        Ok(entry)
    }

    fn open(
        &self,
        ctx: Context,
        inode: u64,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let opened = self.open_native_for_reply(ctx, inode, kill_priv, flags, false)?;
        Ok((opened.reply.handle, opened.reply.options))
    }

    fn open_for_reply(
        &self,
        ctx: Context,
        inode: u64,
        kill_priv: bool,
        flags: u32,
        request_eligible: bool,
    ) -> io::Result<OpenReply> {
        let opened = self.open_native_for_reply(
            ctx,
            inode,
            kill_priv,
            flags,
            self.shared_open_qualification && request_eligible,
        )?;
        let mut reply = opened.reply;
        #[cfg(target_os = "linux")]
        if let Some(identity) = opened.data_identity {
            if reply.allows_data_object(request_eligible) {
                reply.data_object = self.lower_data_object_with_identity(
                    inode,
                    reply.handle.unwrap(),
                    Some(identity),
                );
            }
            return Ok(reply);
        }
        reply.complete_legacy(self, inode, request_eligible);
        Ok(reply)
    }

    fn lower_data_object(&self, inode: u64, handle: u64) -> Option<LowerDataObject> {
        #[cfg(target_os = "linux")]
        {
            self.lower_data_object_with_identity(inode, handle, None)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (inode, handle);
            None
        }
    }

    fn create(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        kill_priv: bool,
        flags: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<(Entry, Option<u64>, OpenOptions)> {
        let _operation = self.write_operation()?;
        let path = self.child(parent, name)?;
        let _io = self.fence_paths(&[&path]);
        let inode = self.allocate_inode(path.clone());
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        let (mut entry, handle, options) = self.layers[0].create(
            ctx,
            upper_parent,
            &upper_name,
            mode,
            kill_priv,
            flags,
            umask,
            extensions,
        )?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inner_inode = entry.inode;
        entry.inode = inode;
        entry.attr.st_ino = inode as _;
        let handle = handle.ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        let id = self.allocate_handle(Handle::File(Arc::new(FileHandle {
            overlay_inode: inode,
            layer: Layer(0),
            inode: inner_inode,
            handle,
            relative_path: path.as_os_str().as_bytes().to_vec(),
        })));
        self.acquire_guest_lookup(inode);
        Ok((entry, Some(id), options))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        _inode: u64,
        handle: u64,
        mut w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        let _span = self.profile.span("read");
        let lease = self.file_io_lease(handle)?;
        self.drain_buffered_writes()?;
        let h = &lease.handle;
        let backing = self.roots[h.layer.0].join(OsStr::from_bytes(&h.relative_path));
        if let Some(bytes) = self
            .core
            .read_remote(&backing, offset, size)
            .map_err(linux_error)?
        {
            w.write_all(&bytes)?;
            Ok(bytes.len())
        } else {
            self.layers[h.layer.0].read_leased(&lease.native, w, size, offset)
        }
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        mut r: R,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<usize> {
        let _span = self.profile.span("write");
        let lease = self.file_io_lease(handle)?;
        if let Some(buffer) = self.buffered_writes() {
            if lease.handle.layer.0 == 0
                && buffered_request_flags(flags)
                && if self.host_writeback_qualification_lock {
                    self.layers[0].buffered_write_eligible(&lease.native)?
                } else {
                    self.layers[0].buffered_write_eligible_unlocked(&lease.native)?
                }
                && size != 0
            {
                if offset
                    .checked_add(u64::from(size))
                    .is_none_or(|end| end > i64::MAX as u64)
                {
                    return Err(io::Error::from_raw_os_error(libc::EFBIG));
                }
                // Reserve before allocation or copying. The permit also covers
                // the in-flight backend write, not just the pending FIFO.
                let permit = buffer.reserve(size as usize)?;
                let mut bytes = vec![0; size as usize];
                r.read_exact(&mut bytes)?;
                let target = BufferedWrite {
                    fs: self.layers[0].clone(),
                    lease,
                    kill_priv,
                    profile: self.profile.clone(),
                };
                permit.submit(Box::new(target), bytes, offset)?;
                self.profile.add("host_writeback_accepted_requests", 1);
                self.profile
                    .add("host_writeback_accepted_bytes", u64::from(size));
                if self.host_writeback_synchronous {
                    // Engineering control: identical owned-copy and worker path,
                    // but wait for its accepted prefix before replying to WRITE.
                    buffer.flush()?;
                }
                return Ok(size as usize);
            }
            // Synchronous/direct/append paths cannot overtake accepted writes.
            buffer.flush()?;
        }
        self.layers[lease.handle.layer.0].write_leased(
            ctx,
            &lease.native,
            r,
            size,
            offset,
            kill_priv,
        )
    }

    fn flush(&self, _ctx: Context, _inode: u64, handle: u64, _lock_owner: u64) -> io::Result<()> {
        let lease = self.file_io_lease(handle)?;
        self.drain_buffered_writes()?;
        self.layers[lease.handle.layer.0].flush_leased(&lease.native)
    }

    fn fsync(&self, _ctx: Context, _inode: u64, datasync: bool, handle: u64) -> io::Result<()> {
        let lease = self.file_io_lease(handle)?;
        self.drain_buffered_writes()?;
        self.core.sync_preimages()?;
        self.layers[lease.handle.layer.0].sync_leased(&lease.native, datasync)
    }

    fn syncfs(&self, _ctx: Context) -> io::Result<()> {
        // Legacy sessions retain their existing ENOSYS behavior. For the new
        // cache this is also the supervisor's final asynchronous-error fence.
        if !self.host_writeback {
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        }
        let _operation = self.write_operation()?;
        let _io = self.io_activity.fence(Vec::new(), true);
        _io.drain();
        self.drain_buffered_writes()?;
        self.core.sync_preimages().map_err(linux_error)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let upper = std::fs::File::open(&self.roots[0]).map_err(linux_error)?;
            // SAFETY: the opened upper root pins the target filesystem until
            // syncfs completes, including files closed before this request.
            if unsafe { libc::syncfs(upper.as_raw_fd()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fsyncdir(&self, _ctx: Context, inode: u64, _datasync: bool, _handle: u64) -> io::Result<()> {
        let _operation = self.write_operation()?;
        self.drain_buffered_writes()?;
        self.core.sync_preimages().map_err(linux_error)?;
        let entry = self
            .core
            .metadata_resolved(&self.path(inode)?)
            .map_err(linux_error)?;
        if !entry.metadata.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        std::fs::File::open(entry.resolved.path)
            .and_then(|file| file.sync_all())
            .map_err(linux_error)
    }

    fn release(
        &self,
        ctx: Context,
        _inode: u64,
        flags: u32,
        handle: u64,
        flush: bool,
        flock_release: bool,
        lock_owner: Option<u64>,
    ) -> io::Result<()> {
        // Closing an open description does not mutate the namespace. A shared
        // guard still orders it against capture/destroy and namespace changes;
        // only this handle's admitted operations need to finish first.
        let _operation = self.read_operation()?;
        let _io = self.io_activity.fence(vec![handle], false);
        _io.drain();
        let handle = self.handles.lock().unwrap().remove(&handle);
        match handle {
            Some(Handle::File(h)) => {
                let result = self.layers[h.layer.0].release(
                    ctx,
                    h.inode,
                    flags,
                    h.handle,
                    flush,
                    flock_release,
                    lock_owner,
                );
                self.layers[h.layer.0].forget(ctx, h.inode, 1);
                result.and_then(|()| self.drain_buffered_writes())
            }
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn statfs(&self, ctx: Context, inode: u64) -> io::Result<bindings::statvfs64> {
        let _operation = self.read_operation()?;
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].statfs(ctx, inner);
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        result
    }

    fn setxattr(
        &self,
        ctx: Context,
        inode: u64,
        name: &CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        let _operation = self.write_operation()?;
        pvisor_overlay_core::validate_guest_xattr(OsStr::from_bytes(name.to_bytes()))
            .map_err(linux_error)?;
        let path = self.path(inode)?;
        let _io = self.fence_paths(&[&path]);
        self.core
            .prepare_metadata_change(&path)
            .map_err(linux_error)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].setxattr(ctx, inner, name, value, flags);
        self.layers[0].forget(ctx, inner, 1);
        result
    }

    fn getxattr(
        &self,
        ctx: Context,
        inode: u64,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        let _operation = self.write_operation()?;
        let path = self.path(inode)?;
        let (layer, inner) = self.observed_inner(ctx, &path)?;
        let result = self.layers[layer.0].getxattr(ctx, inner, name, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn listxattr(&self, ctx: Context, inode: u64, size: u32) -> io::Result<ListxattrReply> {
        let _operation = self.write_operation()?;
        let path = self.path(inode)?;
        let (layer, inner) = self.observed_inner(ctx, &path)?;
        let result = self.layers[layer.0].listxattr(ctx, inner, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn removexattr(&self, ctx: Context, inode: u64, name: &CStr) -> io::Result<()> {
        let _operation = self.write_operation()?;
        pvisor_overlay_core::validate_guest_xattr(OsStr::from_bytes(name.to_bytes()))
            .map_err(linux_error)?;
        let path = self.path(inode)?;
        let _io = self.fence_paths(&[&path]);
        self.core
            .prepare_metadata_change(&path)
            .map_err(linux_error)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].removexattr(ctx, inner, name);
        self.layers[0].forget(ctx, inner, 1);
        result
    }

    fn opendir(
        &self,
        _ctx: Context,
        inode: u64,
        _flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let _span = self.profile.span("opendir");
        let _operation = self.read_operation()?;
        let path = self.path(inode)?;
        let (names, types): (Vec<_>, Vec<_>) = self
            .core
            .directory_candidates(&path)
            .map_err(linux_error)?
            .into_iter()
            .map(|(name, type_)| (name.as_bytes().to_vec(), type_))
            .unzip();
        let handle = self.allocate_handle(Handle::DirectoryNames {
            parent: inode,
            names: Arc::new(names),
            types: Arc::new(types),
        });
        Ok((Some(handle), OpenOptions::empty()))
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        _inode: u64,
        handle: u64,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        let _operation = self.read_operation()?;
        let directory = self.handles.lock().unwrap().get(&handle).cloned();
        if let Some(Handle::DirectoryNames {
            parent,
            names,
            types,
        }) = directory
        {
            let directory = self.path(parent)?;
            for (index, name) in names.iter().enumerate().skip(offset as usize) {
                let path =
                    OverlayCore::child(&directory, OsStr::from_bytes(name)).map_err(linux_error)?;
                let type_ = if let Some(type_) = types.get(index) {
                    *type_
                } else {
                    // Older name-only handles retain their checked fallback.
                    let Some(entry) = self
                        .core
                        .directory_entry(&directory, OsStr::from_bytes(name))
                        .map_err(linux_error)?
                    else {
                        continue;
                    };
                    Self::dtype(entry.backing.metadata.mode())
                };
                let ino = self.allocate_inode(path);
                if add_entry(DirEntry {
                    ino,
                    offset: (index + 1) as u64,
                    type_,
                    name,
                })? == 0
                {
                    break;
                }
            }
            return Ok(());
        }
        let items = match self.handles.lock().unwrap().get(&handle) {
            Some(Handle::Directory(items)) => items.clone(),
            _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
        };
        for (index, item) in items.iter().enumerate().skip(offset as usize) {
            if add_entry(DirEntry {
                ino: item.ino as _,
                offset: (index + 1) as u64,
                type_: item.type_,
                name: &item.name,
            })? == 0
            {
                break;
            }
        }
        Ok(())
    }

    fn readdirplus<F>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry, Entry) -> io::Result<usize>,
    {
        let _span = self.profile.span("readdirplus");
        let _operation = self.read_operation()?;
        let directory = self.handles.lock().unwrap().get(&handle).cloned();
        if let Some(Handle::DirectoryNames { parent, names, .. }) = directory {
            let directory = self.path(parent)?;
            for (index, name) in names.iter().enumerate().skip(offset as usize) {
                let path =
                    OverlayCore::child(&directory, OsStr::from_bytes(name)).map_err(linux_error)?;
                let Some(backing) = self
                    .core
                    .directory_entry_for_backing_lookup(&directory, OsStr::from_bytes(name))
                    .map_err(linux_error)?
                else {
                    continue;
                };
                let inode = self.allocate_inode(path.clone());
                let entry = self.entry_on_backing(ctx, &path, inode, &backing)?;
                self.acquire_guest_lookup(inode);
                let added = add_entry(
                    DirEntry {
                        ino: inode,
                        offset: (index + 1) as u64,
                        type_: Self::dtype(backing.entry.metadata.mode()),
                        name,
                    },
                    entry,
                );
                match added {
                    Ok(0) => {
                        self.forget_guest_lookup(inode, 1);
                        break;
                    }
                    Err(error) => {
                        self.forget_guest_lookup(inode, 1);
                        return Err(error);
                    }
                    Ok(_) => {}
                }
            }
            return Ok(());
        }
        let items = match self.handles.lock().unwrap().get(&handle) {
            Some(Handle::Directory(items)) => items.clone(),
            _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
        };
        for (index, item) in items.iter().enumerate().skip(offset as usize) {
            let path = self.path(item.ino)?;
            let entry = self.entry(ctx, &path, item.ino)?;
            self.acquire_guest_lookup(item.ino);
            let added = add_entry(
                DirEntry {
                    ino: item.ino as _,
                    offset: (index + 1) as u64,
                    type_: item.type_,
                    name: &item.name,
                },
                entry,
            );
            match added {
                Ok(0) => {
                    self.forget_guest_lookup(item.ino, 1);
                    break;
                }
                Err(error) => {
                    self.forget_guest_lookup(item.ino, 1);
                    return Err(error);
                }
                Ok(_) => {}
            }
        }
        Ok(())
    }

    fn releasedir(&self, _ctx: Context, _inode: u64, _flags: u32, handle: u64) -> io::Result<()> {
        // An enumeration already owns an Arc snapshot. Removing its registry
        // entry cannot invalidate that snapshot or change namespace contents.
        let _operation = self.read_operation()?;
        match self.handles.lock().unwrap().remove(&handle) {
            Some(Handle::Directory(_) | Handle::DirectoryNames { .. }) => Ok(()),
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn access(&self, ctx: Context, inode: u64, mask: u32) -> io::Result<()> {
        let _operation = self.read_operation()?;
        let path = self.path(inode)?;
        if self.virtual_metadata {
            let attr = self.entry(ctx, &path, inode)?.attr;
            // mode_t is u16 on macOS and u32 on Linux.
            #[allow(clippy::unnecessary_cast)]
            let mode = attr.st_mode as u32;
            let requested = mask & 7;
            let allowed = if ctx.uid == 0 {
                if mode & 0o111 != 0 || mode & 0o170000 == 0o040000 {
                    7
                } else {
                    6
                }
            } else if ctx.uid == attr.st_uid {
                (mode >> 6) & 7
            } else if ctx.gid == attr.st_gid {
                (mode >> 3) & 7
            } else {
                mode & 7
            };
            return if requested & !allowed == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(libc::EACCES))
            };
        }
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].access(ctx, inner, mask);
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        result
    }

    fn lseek(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        let _operation = self.write_operation()?;
        self.with_file_handle(handle, |fs, h| {
            backend::materialize_file(
                &self.roots[h.layer.0].join(OsStr::from_bytes(&h.relative_path)),
            )
            .map_err(linux_error)?;
            fs.lseek(ctx, h.inode, h.handle, offset, whence)
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    mod mutation_regressions {
        use super::*;

        fn fixture(virtual_parent: Option<&[u8]>) -> (tempfile::TempDir, OverlayFs, Context) {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            let upper = temp.path().join("upper");
            std::fs::create_dir(&lower).unwrap();
            std::fs::create_dir(&upper).unwrap();
            if let Some(identity) = virtual_parent {
                pvisor_overlay_core::sys::set_xattr(
                    &upper,
                    OsStr::new("user.containers.override_stat"),
                    identity,
                    0,
                )
                .unwrap();
            }
            let fs = parent_cache_fixture(temp.path());
            (
                temp,
                fs,
                Context {
                    uid: 0,
                    gid: 0,
                    pid: 1,
                },
            )
        }

        fn truncate_detached(replace: bool) {
            let (temp, fs, ctx) = fixture(None);
            let upper = temp.path().join("upper");
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"file",
                    libc::S_IFREG | 0o600,
                    false,
                    libc::O_RDWR as u32,
                    0,
                    Extensions::default(),
                )
                .unwrap();
            std::fs::write(upper.join("file"), b"original").unwrap();
            if replace {
                std::fs::write(upper.join("replacement"), b"replacement").unwrap();
                fs.rename(
                    ctx,
                    fuse::ROOT_ID,
                    c"replacement",
                    fuse::ROOT_ID,
                    c"file",
                    0,
                )
                .unwrap();
            } else {
                fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
            }
            let mut attr = fs.getattr(ctx, entry.inode, handle).unwrap().0;
            assert_eq!(attr.st_nlink, 0);
            attr.st_size = 2;
            let result = fs.setattr(ctx, entry.inode, attr, handle, SetattrValid::SIZE);
            assert!(
                result.is_ok(),
                "ftruncate on a valid detached writable handle: {result:?}"
            );
            assert_eq!(fs.getattr(ctx, entry.inode, handle).unwrap().0.st_size, 2);
            if replace {
                assert_eq!(std::fs::read(upper.join("file")).unwrap(), b"replacement");
            } else {
                assert!(!upper.join("file").exists());
            }
            fs.release(
                ctx,
                entry.inode,
                libc::O_RDWR as u32,
                handle.unwrap(),
                false,
                false,
                None,
            )
            .unwrap();
        }
        #[test]
        fn truncate_unlink() {
            truncate_detached(false);
        }
        #[test]
        fn truncate_replacement() {
            truncate_detached(true);
        }

        #[test]
        fn truncate_detached_waits_for_io_on_another_open_handle() {
            use std::sync::mpsc::channel;
            use std::time::Instant;

            let (temp, fs, ctx) = fixture(None);
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"file",
                    libc::S_IFREG | 0o600,
                    false,
                    libc::O_RDWR as u32,
                    0,
                    Extensions::default(),
                )
                .unwrap();
            let handle = handle.unwrap();
            std::fs::write(temp.path().join("upper/file"), b"original").unwrap();
            let alias = fs
                .open(ctx, entry.inode, false, libc::O_RDWR as u32)
                .unwrap()
                .0
                .unwrap();
            fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
            let mut attr = fs.getattr(ctx, entry.inode, Some(handle)).unwrap().0;
            attr.st_size = 2;
            let lease = fs.file_io_lease(alias).unwrap();
            let (sent, received) = channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    sent.send(fs.setattr(ctx, entry.inode, attr, Some(handle), SetattrValid::SIZE))
                        .unwrap();
                });
                let deadline = Instant::now() + Duration::from_secs(2);
                let fenced = loop {
                    if fs
                        .io_activity
                        .state
                        .lock()
                        .unwrap()
                        .blocked
                        .contains_key(&alias)
                    {
                        break true;
                    }
                    if Instant::now() >= deadline {
                        break false;
                    }
                    std::thread::yield_now();
                };
                let premature = received.try_recv();
                drop(lease);
                let result = received.recv_timeout(Duration::from_secs(2));
                assert!(fenced, "truncate must fence I/O through every open alias");
                assert!(
                    premature.is_err(),
                    "truncate completed before admitted I/O drained"
                );
                assert_eq!(
                    result.expect("truncate did not resume").unwrap().0.st_size,
                    2
                );
            });
            assert_eq!(
                fs.getattr(ctx, entry.inode, Some(alias)).unwrap().0.st_size,
                2
            );
            assert!(!temp.path().join("upper/file").exists());
            for id in [handle, alias] {
                fs.release(
                    ctx,
                    entry.inode,
                    libc::O_RDWR as u32,
                    id,
                    false,
                    false,
                    None,
                )
                .unwrap();
            }
        }

        #[test]
        fn truncate_live_and_path_default() {
            let (temp, fs, ctx) = fixture(None);
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"file",
                    libc::S_IFREG | 0o600,
                    false,
                    libc::O_RDWR as u32,
                    0,
                    Extensions::default(),
                )
                .unwrap();
            std::fs::write(temp.path().join("upper/file"), b"original").unwrap();
            for (fh, size) in [(None, 5), (handle, 2)] {
                let mut attr = fs.getattr(ctx, entry.inode, handle).unwrap().0;
                attr.st_size = size;
                fs.setattr(ctx, entry.inode, attr, fh, SetattrValid::SIZE)
                    .unwrap();
                assert_eq!(
                    std::fs::metadata(temp.path().join("upper/file"))
                        .unwrap()
                        .len(),
                    size as u64
                );
            }
            fs.release(
                ctx,
                entry.inode,
                libc::O_RDWR as u32,
                handle.unwrap(),
                false,
                false,
                None,
            )
            .unwrap();
        }

        #[test]
        fn truncate_rejects_bad_handles() {
            let (temp, fs, ctx) = fixture(None);
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"file",
                    libc::S_IFREG | 0o600,
                    false,
                    libc::O_RDWR as u32,
                    0,
                    Extensions::default(),
                )
                .unwrap();
            std::fs::write(temp.path().join("upper/file"), b"original").unwrap();
            std::fs::write(temp.path().join("upper/other"), b"other").unwrap();
            let other = fs.lookup(ctx, fuse::ROOT_ID, c"other").unwrap();
            let mut attr = other.attr;
            attr.st_size = 1;
            for (inode, fh) in [(other.inode, handle), (entry.inode, Some(u64::MAX))] {
                assert_eq!(
                    fs.setattr(ctx, inode, attr, fh, SetattrValid::SIZE)
                        .unwrap_err()
                        .raw_os_error(),
                    Some(libc::EBADF)
                );
            }
            assert_eq!(
                std::fs::read(temp.path().join("upper/file")).unwrap(),
                b"original"
            );
            assert_eq!(
                std::fs::read(temp.path().join("upper/other")).unwrap(),
                b"other"
            );
            fs.release(
                ctx,
                entry.inode,
                libc::O_RDWR as u32,
                handle.unwrap(),
                false,
                false,
                None,
            )
            .unwrap();
        }

        #[test]
        fn truncate_readonly_upper_and_lower() {
            let (temp, fs, ctx) = fixture(None);
            for (directory, name) in [("upper", c"upper-file"), ("lower", c"lower-file")] {
                let path = temp
                    .path()
                    .join(directory)
                    .join(OsStr::from_bytes(name.to_bytes()));
                std::fs::write(&path, b"original").unwrap();
                let entry = fs.lookup(ctx, fuse::ROOT_ID, name).unwrap();
                let (handle, _) = fs
                    .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
                    .unwrap();
                let mut attr = entry.attr;
                attr.st_size = 1;
                assert!(
                    fs.setattr(ctx, entry.inode, attr, handle, SetattrValid::SIZE)
                        .is_err(),
                    "readonly {directory} handle allowed truncate"
                );
                assert_eq!(std::fs::read(path).unwrap(), b"original");
                fs.release(
                    ctx,
                    entry.inode,
                    libc::O_RDONLY as u32,
                    handle.unwrap(),
                    false,
                    false,
                    None,
                )
                .unwrap();
            }
        }

        fn mkdir_virtual(setgid: bool) {
            let marker = if setgid {
                b"1234:2345:42777".as_slice()
            } else {
                b"1234:2345:40777".as_slice()
            };
            let (temp, fs, ctx) = fixture(Some(marker));
            let entry = fs
                .mkdir(
                    ctx,
                    fuse::ROOT_ID,
                    c"created",
                    0o755,
                    0o022,
                    Extensions::default(),
                )
                .unwrap();
            let expected_gid = if setgid { 2345 } else { 0 };
            assert_eq!(
                (entry.attr.st_uid, entry.attr.st_gid),
                (0, expected_gid),
                "guest creator and virtual parent group"
            );
            assert_eq!(
                entry.attr.st_mode & 0o7777,
                if setgid { 0o2755 } else { 0o755 }
            );
            let again = fs.lookup(ctx, fuse::ROOT_ID, c"created").unwrap();
            assert_eq!((again.attr.st_uid, again.attr.st_gid), (0, expected_gid));
            let identity = pvisor_overlay_core::sys::get_xattr(
                &temp.path().join("upper/created"),
                OsStr::new("user.containers.override_stat"),
            )
            .unwrap();
            let identity = backend::UnixIdentity::parse(&identity).unwrap();
            assert_eq!((identity.uid, identity.gid), (0, expected_gid));
        }
        #[test]
        fn mkdir_virtual_owner() {
            mkdir_virtual(false);
        }
        #[test]
        fn mkdir_virtual_setgid() {
            mkdir_virtual(true);
        }

        #[test]
        fn mkdir_default_and_excluded_path() {
            let (temp, fs, ctx) = fixture(None);
            let entry = fs
                .mkdir(
                    ctx,
                    fuse::ROOT_ID,
                    c"created",
                    0o755,
                    0o022,
                    Extensions::default(),
                )
                .unwrap();
            let host = std::fs::metadata(temp.path().join("upper/created")).unwrap();
            assert_eq!(
                (entry.attr.st_uid, entry.attr.st_gid),
                (host.uid(), host.gid())
            );
            assert_eq!(entry.attr.st_mode & 0o7777, 0o755);
            assert!(
                fs.mkdir(
                    ctx,
                    fuse::ROOT_ID,
                    c"private",
                    0o755,
                    0,
                    Extensions::default()
                )
                .is_err()
            );
            assert!(!temp.path().join("upper/private").exists());
        }
    }

    use super::*;

    #[cfg(target_os = "linux")]
    mod host_writeback {
        use super::*;
        include!("overlay_host_writeback_tests.rs");
    }

    #[cfg(target_os = "linux")]
    fn negative_entry_fixture(root: &Path) -> OverlayFs {
        negative_entry_fixture_with_exclusions(root, Vec::new())
    }

    #[cfg(target_os = "linux")]
    fn negative_entry_fixture_with_exclusions(
        root: &Path,
        excluded_paths: Vec<String>,
    ) -> OverlayFs {
        std::fs::create_dir_all(root.join("lower/dir")).unwrap();
        std::fs::write(root.join("lower/existing"), b"lower").unwrap();
        let mut fs = OverlayFs::build_with_writeback(
            Config {
                lower_dirs: vec![root.join("lower").to_string_lossy().into_owned()],
                lower_mutability: vec![pvisor_overlay_core::LayerMutability::Immutable],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: root.join("upper").to_string_lossy().into_owned(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths,
                access_policy: Default::default(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            Arc::new(InodeAllocator::new()),
            false,
            Some("0"),
        )
        .unwrap();
        fs.profile = pvisor_overlay_core::profile::Profile::enabled("negative-entry-test");
        fs.init(FsOptions::empty()).unwrap();
        fs
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entries_require_owned_default_local_views_and_keep_explicit_profiles() {
        for case in 0..8 {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            let mut fs = negative_entry_fixture(temp.path());
            let requested = case != 0;
            let guest_cache = case != 1;
            let dax = case == 2;
            match case {
                3 => fs.new_session_writeback = true,
                4 => fs.snapshot_config.lower_mutability.clear(),
                5 => {
                    fs.snapshot_config.lower_mutability[0] =
                        pvisor_overlay_core::LayerMutability::Mutable
                }
                6 => {
                    fs.snapshot_config.semantics = passthrough::PermissionSemantics::LinuxSimplified
                }
                7 => {
                    fs.snapshot_config.access_policy =
                        pvisor_overlay_core::FileAccessPolicy::new(vec!["denied".into()], vec![])
                            .unwrap()
                }
                _ => {}
            }
            fs.configure_negative_entry_cache(requested, guest_cache, dax, vec![])
                .unwrap();
            let profile = fs.profile.report().unwrap();
            assert_eq!(
                profile.measurements["negative_entry_cache_requested"].units,
                u64::from(requested)
            );
            assert_eq!(
                profile.measurements["negative_entry_cache_enabled"].units,
                0
            );
            assert_eq!(
                profile.measurements["negative_entry_cache_replies"].units,
                0
            );
            assert_eq!(
                fs.lookup(
                    Context {
                        uid: 0,
                        gid: 0,
                        pid: 1
                    },
                    fuse::ROOT_ID,
                    c"missing"
                )
                .err()
                .expect("operation must fail")
                .raw_os_error(),
                Some(libc::ENOENT)
            );
            assert_eq!(
                fs.configure_negative_entry_cache(true, true, false, vec![])
                    .expect_err("operation must fail")
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entry_exclusions_protect_ancestors_and_subtrees_without_disabling_siblings() {
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for enabled in [false, true] {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            std::fs::create_dir_all(temp.path().join("lower/tmp/pvisor-host-0")).unwrap();
            std::fs::write(
                temp.path().join("lower/tmp/pvisor-host-0/secret"),
                b"host authority",
            )
            .unwrap();
            let mut fs = negative_entry_fixture_with_exclusions(
                temp.path(),
                vec!["tmp/pvisor-host-0".into(), "absent/protected".into()],
            );
            fs.configure_negative_entry_cache(enabled, true, false, vec![PathBuf::from("once")])
                .unwrap();
            let initial_nodes = fs.nodes.lock().unwrap().by_inode.len();
            // This missing ancestor must use ordinary ENOENT, just like the
            // existing excluded object and the worker's synthetic name.
            for name in [c"absent", c"once"] {
                assert_eq!(
                    fs.lookup(ctx, fuse::ROOT_ID, name)
                        .err()
                        .expect("protected names retain ordinary lookup")
                        .raw_os_error(),
                    Some(libc::ENOENT)
                );
            }
            assert_eq!(fs.nodes.lock().unwrap().by_inode.len(), initial_nodes);
            let tmp = fs.lookup(ctx, fuse::ROOT_ID, c"tmp").unwrap();
            assert_ne!(tmp.inode, 0);
            assert_eq!((tmp.attr_timeout, tmp.entry_timeout), (TTL, TTL));
            assert_eq!(
                fs.lookup(ctx, tmp.inode, c"pvisor-host-0")
                    .err()
                    .expect("physically present host authority stays excluded")
                    .raw_os_error(),
                Some(libc::ENOENT)
            );
            // Even a retained protocol inode must not turn an excluded
            // descendant into a successful nodeid=0 response.
            let protected = fs.allocate_inode(PathBuf::from("tmp/pvisor-host-0"));
            assert_eq!(
                fs.lookup(ctx, protected, c"secret")
                    .err()
                    .expect("excluded descendants retain Core's visibility error")
                    .raw_os_error(),
                Some(libc::ENOENT)
            );
            let before = fs.profile.report().unwrap();
            assert_eq!(
                before.measurements["negative_entry_cache_enabled"].units,
                u64::from(enabled)
            );
            assert_eq!(before.measurements["negative_entry_cache_replies"].units, 0);
            assert_eq!(
                before.measurements["negative_entry_cache_fallbacks"].units,
                if enabled { 5 } else { 0 }
            );
            for (parent, name) in [
                (fuse::ROOT_ID, c"unrelated"),
                (fuse::ROOT_ID, c"tmp-other"),
                (tmp.inode, c"pvisor-host-01"),
                (tmp.inode, c"unrelated"),
            ] {
                let result = fs.lookup(ctx, parent, name);
                if enabled {
                    let negative = result.unwrap();
                    assert_eq!(
                        (
                            negative.inode,
                            negative.attr_timeout,
                            negative.entry_timeout
                        ),
                        (0, Duration::ZERO, TTL)
                    );
                } else {
                    assert_eq!(
                        result
                            .err()
                            .expect("feature-off absence remains an error")
                            .raw_os_error(),
                        Some(libc::ENOENT)
                    );
                }
            }
            let after = fs.profile.report().unwrap();
            assert_eq!(
                after.measurements["negative_entry_cache_replies"].units,
                if enabled { 4 } else { 0 }
            );
            assert_eq!(
                after.measurements["negative_entry_cache_fallbacks"].units,
                before.measurements["negative_entry_cache_fallbacks"].units
            );
            // Neither a forbidden object nor cacheable absence materializes
            // a host-authority inode or changes the native file.
            assert_eq!(
                std::fs::read(temp.path().join("lower/tmp/pvisor-host-0/secret")).unwrap(),
                b"host authority"
            );
            assert!(!fs
                .nodes
                .lock()
                .unwrap()
                .by_path
                .contains_key(Path::new("tmp/pvisor-host-0/secret")));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entries_allocate_no_inode_preserve_errors_and_never_extend_positive_ttls() {
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut fs = negative_entry_fixture(temp.path());
        fs.snapshot_config
            .access_policy
            .bind_session("run", "attempt", "workspace");
        fs.snapshot_config.access_policy = fs.snapshot_config.access_policy.for_view("rootfs");
        fs.configure_negative_entry_cache(true, true, false, vec![PathBuf::from("once")])
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let count = fs.nodes.lock().unwrap().by_inode.len();
        let missing = fs.lookup(ctx, fuse::ROOT_ID, c"missing").unwrap();
        assert_eq!(
            (
                missing.inode,
                missing.generation,
                missing.attr_timeout,
                missing.entry_timeout
            ),
            (0, 0, Duration::ZERO, TTL)
        );
        assert_eq!(fs.nodes.lock().unwrap().by_inode.len(), count);
        assert!(!fs
            .nodes
            .lock()
            .unwrap()
            .by_path
            .contains_key(Path::new("missing")));
        let wire = super::super::fuse::EntryOut::from(missing);
        assert_eq!((wire.nodeid, wire.entry_valid, wire.attr_valid), (0, 1, 0));
        let existing = fs.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap();
        assert_ne!(existing.inode, 0);
        assert_eq!((existing.attr_timeout, existing.entry_timeout), (TTL, TTL));
        assert_eq!(
            fs.lookup(ctx, existing.inode, c"child")
                .err()
                .expect("operation must fail")
                .raw_os_error(),
            Some(libc::ENOTDIR)
        );
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"once")
                .err()
                .expect("operation must fail")
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        let profile = fs.profile.report().unwrap();
        assert_eq!(
            profile.measurements["negative_entry_cache_enabled"].units,
            1
        );
        assert_eq!(
            profile.measurements["negative_entry_cache_replies"].units,
            1
        );
        assert_eq!(
            profile.measurements["negative_entry_cache_fallbacks"].units,
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entry_replies_do_not_hide_native_create_rename_link_or_unlink_results() {
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut fs = negative_entry_fixture(temp.path());
        fs.configure_negative_entry_cache(true, true, false, vec![])
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for name in [c"created", c"renamed", c"linked", c"directory", c"symlink"] {
            assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, name).unwrap().inode, 0);
        }
        let (created, handle, _) = fs
            .create(
                ctx,
                fuse::ROOT_ID,
                c"created",
                libc::S_IFREG | 0o640,
                false,
                libc::O_RDWR as u32,
                0,
                Extensions::default(),
            )
            .unwrap();
        fs.release(
            ctx,
            created.inode,
            libc::O_RDWR as u32,
            handle.unwrap(),
            false,
            false,
            None,
        )
        .unwrap();
        assert_ne!(fs.lookup(ctx, fuse::ROOT_ID, c"created").unwrap().inode, 0);
        fs.rename(ctx, fuse::ROOT_ID, c"created", fuse::ROOT_ID, c"renamed", 0)
            .unwrap();
        assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, c"created").unwrap().inode, 0);
        let renamed = fs.lookup(ctx, fuse::ROOT_ID, c"renamed").unwrap();
        assert_ne!(renamed.inode, 0);
        fs.link(ctx, renamed.inode, fuse::ROOT_ID, c"linked")
            .unwrap();
        assert_ne!(fs.lookup(ctx, fuse::ROOT_ID, c"linked").unwrap().inode, 0);
        fs.unlink(ctx, fuse::ROOT_ID, c"linked").unwrap();
        assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, c"linked").unwrap().inode, 0);
        fs.mkdir(
            ctx,
            fuse::ROOT_ID,
            c"directory",
            0o755,
            0,
            Extensions::default(),
        )
        .unwrap();
        assert_ne!(
            fs.lookup(ctx, fuse::ROOT_ID, c"directory").unwrap().inode,
            0
        );
        fs.symlink(
            ctx,
            c"renamed",
            fuse::ROOT_ID,
            c"symlink",
            Extensions::default(),
        )
        .unwrap();
        assert_ne!(fs.lookup(ctx, fuse::ROOT_ID, c"symlink").unwrap().inode, 0);
        assert_eq!(
            fs.rename(
                ctx,
                fuse::ROOT_ID,
                c"absent-source",
                fuse::ROOT_ID,
                c"failed",
                0
            )
            .expect_err("operation must fail")
            .raw_os_error(),
            Some(libc::ENOENT)
        );
        assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, c"failed").unwrap().inode, 0);
        assert_eq!(
            std::fs::read(temp.path().join("lower/existing")).unwrap(),
            b"lower"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entry_snapshot_fence_is_not_serialized_and_survives_destroy() {
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut fs = negative_entry_fixture(temp.path());
        fs.configure_negative_entry_cache(true, true, false, vec![])
            .unwrap();
        let before = fs.capture_state().unwrap();
        assert!(!serde_json::to_string(&before)
            .unwrap()
            .contains("negative_entry_cache"));
        fs.restore_state(&before).unwrap();
        fs.lookup(
            Context {
                uid: 0,
                gid: 0,
                pid: 1,
            },
            fuse::ROOT_ID,
            c"missing",
        )
        .unwrap();
        assert_eq!(
            fs.capture_state()
                .expect_err("operation must fail")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            fs.restore_state(&before)
                .expect_err("operation must fail")
                .kind(),
            io::ErrorKind::Unsupported
        );
        fs.destroy();
        fs.init(FsOptions::empty()).unwrap();
        assert_eq!(
            fs.capture_state()
                .expect_err("operation must fail")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            fs.restore_state(&before)
                .expect_err("operation must fail")
                .kind(),
            io::ErrorKind::Unsupported
        );
        let mut restored =
            OverlayFs::open_existing(fs.snapshot_config.clone(), Arc::new(InodeAllocator::new()))
                .unwrap();
        assert!(!restored.negative_entry_cache);
        assert!(!restored.negative_entry_cache_seen.load(Ordering::Relaxed));
        restored
            .configure_negative_entry_cache(false, true, false, vec![])
            .unwrap();
        restored.restore_state(&before).unwrap();
        assert_eq!(
            restored
                .lookup(
                    Context {
                        uid: 0,
                        gid: 0,
                        pid: 1
                    },
                    fuse::ROOT_ID,
                    c"missing"
                )
                .err()
                .expect("operation must fail")
                .raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entries_reject_existing_projections_and_fall_back_after_late_attachment() {
        use pvisor_overlay_core::backend::{BackendAttachment, FileAttr, ReadOnlyBackend};
        struct Projection;
        impl ReadOnlyBackend for Projection {
            fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
                Err(io::Error::from_raw_os_error(libc::ENOENT))
            }
            fn prepare_directory(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
            fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
                Err(io::Error::from_raw_os_error(libc::ENOENT))
            }
            fn read_at(&self, _: &Path, _: u64, _: u32) -> io::Result<Vec<u8>> {
                Err(io::Error::from_raw_os_error(libc::ENOENT))
            }
            fn materialize_file(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
            fn materialize_tree(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
        }
        for late in [false, true] {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            let mut fs = negative_entry_fixture(temp.path());
            if late {
                fs.configure_negative_entry_cache(true, true, false, vec![])
                    .unwrap();
            }
            let _attachment =
                BackendAttachment::new(&temp.path().join("lower"), Arc::new(Projection)).unwrap();
            if !late {
                fs.configure_negative_entry_cache(true, true, false, vec![])
                    .unwrap();
            }
            assert_eq!(
                fs.lookup(
                    Context {
                        uid: 0,
                        gid: 0,
                        pid: 1
                    },
                    fuse::ROOT_ID,
                    c"missing"
                )
                .err()
                .expect("operation must fail")
                .raw_os_error(),
                Some(libc::ENOENT)
            );
            let profile = fs.profile.report().unwrap();
            assert_eq!(
                profile.measurements["negative_entry_cache_enabled"].units,
                u64::from(late)
            );
            assert_eq!(
                profile.measurements["negative_entry_cache_replies"].units,
                0
            );
            assert!(!fs.negative_entry_cache_seen.load(Ordering::Relaxed));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entry_configuration_rejects_journal_overlap_without_enabling_core_caches() {
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut config = negative_entry_fixture(temp.path()).snapshot_config.clone();
        config.preimage_dir = Some(
            temp.path()
                .join("upper/journal")
                .to_string_lossy()
                .into_owned(),
        );
        // Legacy layouts permit this journal position. Initialize one through
        // its real constructor so open_existing reaches preference validation.
        let legacy = OverlayCore::new_with_exclusions_and_preimages(
            vec![PathBuf::from(&config.lower_dirs[0])],
            PathBuf::from(&config.upper_dir),
            None,
            vec![],
            config.preimage_dir.as_ref().map(PathBuf::from),
        )
        .unwrap();
        drop(legacy);
        let mut fs = OverlayFs::build_with_writeback(
            config,
            Arc::new(InodeAllocator::new()),
            true,
            Some("0"),
        )
        .unwrap();
        assert_eq!(
            fs.configure_negative_entry_cache(true, true, false, vec![])
                .expect_err("operation must fail")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!fs.negative_entry_cache_configured);
        assert!(!fs.core.exclusive_upper_absence_cache_enabled());
        fs.configure_negative_entry_cache(false, true, false, vec![])
            .unwrap();
        assert!(!fs.negative_entry_cache);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn negative_entry_restore_preserves_unset_writeback_compatibility_only_when_disabled() {
        for (saved_writeback, negative, accepted) in [
            (true, true, false),
            (true, false, true),
            (false, true, true),
        ] {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            let config = negative_entry_fixture(temp.path()).snapshot_config.clone();
            let source = OverlayFs::build_with_writeback(
                config.clone(),
                Arc::new(InodeAllocator::new()),
                false,
                Some(if saved_writeback { "1" } else { "0" }),
            )
            .unwrap();
            let options = source.init(FsOptions::WRITEBACK_CACHE).unwrap();
            assert_eq!(
                options.contains(FsOptions::WRITEBACK_CACHE),
                saved_writeback
            );
            let ctx = Context {
                uid: 0,
                gid: 0,
                pid: 1,
            };
            let existing = source.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap();
            let saved = source.capture_state().unwrap();
            drop(source);

            // Unset preserves native restoration support for historical WB1,
            // while the preference for a future INIT remains writeback-off.
            let mut restored = OverlayFs::build_with_writeback(
                config,
                Arc::new(InodeAllocator::new()),
                true,
                None,
            )
            .unwrap();
            assert!(!restored.new_session_writeback);
            assert!(!restored.writeback_active.load(Ordering::Relaxed));
            restored.profile =
                pvisor_overlay_core::profile::Profile::enabled("negative-restore-test");
            restored
                .configure_negative_entry_cache(negative, true, false, vec![])
                .unwrap();
            assert_eq!(
                restored.profile.report().unwrap().measurements["negative_entry_cache_enabled"]
                    .units,
                u64::from(negative)
            );
            if accepted {
                restored.restore_state(&saved).unwrap();
                assert_eq!(
                    restored.writeback_active.load(Ordering::Relaxed),
                    saved_writeback
                );
                assert_eq!(
                    restored
                        .getattr(ctx, existing.inode, None)
                        .unwrap()
                        .0
                        .st_size,
                    5
                );
                if negative {
                    assert_eq!(
                        restored
                            .lookup(ctx, fuse::ROOT_ID, c"missing")
                            .unwrap()
                            .inode,
                        0
                    );
                    assert_eq!(
                        restored.profile.report().unwrap().measurements
                            ["negative_entry_cache_replies"]
                            .units,
                        1
                    );
                }
            } else {
                assert_eq!(
                    restored
                        .restore_state(&saved)
                        .expect_err("WB1 must be refused")
                        .kind(),
                    io::ErrorKind::Unsupported
                );
                assert!(!restored.writeback_active.load(Ordering::Relaxed));
                assert_eq!(restored.nodes.lock().unwrap().by_inode.len(), 1);
                assert!(!restored.negative_entry_cache_seen.load(Ordering::Relaxed));
            }
        }
    }

    #[test]
    fn directory_images_require_every_physical_lower_to_be_immutable() {
        use pvisor_overlay_core::LayerMutability::{Immutable, Mutable};
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let other = temp.path().join("other");
        std::fs::create_dir(&lower).unwrap();
        std::fs::create_dir(&other).unwrap();
        for (i, (mutability, allowed)) in [
            (vec![], false),
            (vec![Mutable, Mutable], false),
            (vec![Immutable, Mutable], false),
            (vec![Immutable, Immutable], true),
        ]
        .into_iter()
        .enumerate()
        {
            let fs = OverlayFs::new(
                Config {
                    lower_dirs: vec![
                        lower.to_string_lossy().into(),
                        other.to_string_lossy().into(),
                    ],
                    lower_mutability: mutability,
                    upper_dir: temp
                        .path()
                        .join(format!("upper-{i}"))
                        .to_string_lossy()
                        .into(),
                    work_dir: None,
                    preimage_dir: None,
                    apply_target: None,
                    baseline_lower: None,
                    baseline_content_index: None,
                    excluded_paths: vec![],
                    access_policy: Default::default(),
                    semantics: passthrough::PermissionSemantics::LinuxComplete,
                },
                Arc::new(InodeAllocator::new()),
            )
            .unwrap();
            assert_eq!(fs.supports_directory_image(), allowed);
        }
    }

    struct ReadWriter {
        gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
        bytes: Vec<u8>,
    }
    impl io::Write for ReadWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl ZeroCopyWriter for ReadWriter {
        fn write_from(
            &mut self,
            file: &std::fs::File,
            count: usize,
            off: u64,
        ) -> io::Result<usize> {
            use std::os::unix::fs::FileExt;
            if let Some((started, resume)) = self.gate.take() {
                started.send(()).unwrap();
                resume.recv().unwrap();
            }
            let mut bytes = vec![0; count];
            let len = file.read_at(&mut bytes, off)?;
            self.bytes.extend_from_slice(&bytes[..len]);
            Ok(len)
        }
    }

    struct WriteReader(io::Cursor<Vec<u8>>);

    #[cfg(target_os = "linux")]
    struct WritebackTestFs(Arc<OverlayFs>);

    #[cfg(target_os = "linux")]
    impl FileSystem for WritebackTestFs {
        type Inode = u64;
        type Handle = u64;

        fn capture_state(&self) -> io::Result<super::super::snapshot::FsSnapshot> {
            self.0.capture_state()
        }

        fn restore_state(&self, state: &super::super::snapshot::FsSnapshot) -> io::Result<()> {
            self.0.restore_state(state)
        }

        fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
            self.0.init(capable)
        }

        fn destroy(&self) {
            self.0.destroy();
        }
    }

    #[cfg(target_os = "linux")]
    fn writeback_test_overlay(
        root: &Path,
        restoring: bool,
        setting: Option<&str>,
    ) -> Arc<OverlayFs> {
        Arc::new(
            OverlayFs::build_with_writeback(
                Config {
                    lower_mutability: Vec::new(),
                    lower_dirs: vec![root.join("lower").to_string_lossy().into_owned()],
                    apply_target: None,
                    baseline_lower: None,
                    baseline_content_index: None,
                    upper_dir: root.join("upper").to_string_lossy().into_owned(),
                    work_dir: None,
                    preimage_dir: None,
                    excluded_paths: Vec::new(),
                    access_policy: Default::default(),
                    semantics: passthrough::PermissionSemantics::LinuxComplete,
                },
                Arc::new(InodeAllocator::new()),
                restoring,
                setting,
            )
            .unwrap(),
        )
    }

    #[cfg(target_os = "linux")]
    fn writeback_test_control_request(
        server: &super::super::server::Server<WritebackTestFs>,
        opcode: fuse::Opcode,
        body: &[u8],
    ) -> (vm_memory::GuestMemoryMmap, vm_memory::GuestAddress, usize) {
        use super::super::descriptor_utils::{
            create_descriptor_chain, DescriptorType, OwnedDescriptorChain,
        };
        use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (DescriptorType::Readable, body.len() as u32),
                (DescriptorType::Writable, 256),
            ],
            0,
        )
        .unwrap();
        mem.write_slice(body, GuestAddress(0x100)).unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        let (reader, writer) = plan.reader_writer(&mem).unwrap();
        let len = server
            .handle_message(
                (
                    fuse::InHeader {
                        len: (std::mem::size_of::<fuse::InHeader>() + body.len()) as u32,
                        opcode: opcode as u32,
                        unique: 1,
                        ..Default::default()
                    },
                    reader,
                ),
                writer,
                false,
                &None,
                &Arc::new(std::sync::atomic::AtomicI32::new(0)),
            )
            .unwrap();
        (mem, GuestAddress(0x100 + body.len() as u64), len)
    }

    #[cfg(target_os = "linux")]
    fn negotiate_writeback_test_session(
        server: &super::super::server::Server<WritebackTestFs>,
        capable: FsOptions,
    ) -> FsOptions {
        use vm_memory::{Address, ByteValued, Bytes};
        let body = fuse::InitInCompat {
            major: fuse::KERNEL_VERSION,
            minor: fuse::KERNEL_MINOR_VERSION,
            flags: capable.bits() as u32,
            ..Default::default()
        };
        let (mem, address, len) =
            writeback_test_control_request(server, fuse::Opcode::Init, body.as_slice());
        let reply: fuse::OutHeader = mem.read_obj(address).unwrap();
        assert_eq!(reply.error, 0);
        assert!(
            len >= std::mem::size_of::<fuse::OutHeader>() + std::mem::size_of::<fuse::InitOut>()
        );
        let out: fuse::InitOut = mem
            .read_obj(
                address
                    .checked_add(std::mem::size_of::<fuse::OutHeader>() as u64)
                    .unwrap(),
            )
            .unwrap();
        FsOptions::from_bits((u64::from(out.flags2) << 32) | u64::from(out.flags)).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn new_overlay_sessions_require_explicit_writeback_and_guest_capability() {
        for (setting, capable, expected) in [
            (None, FsOptions::WRITEBACK_CACHE, false),
            (Some("0"), FsOptions::WRITEBACK_CACHE, false),
            (Some("1"), FsOptions::WRITEBACK_CACHE, true),
            (Some("1"), FsOptions::empty(), false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let fs = writeback_test_overlay(temp.path(), false, setting);
            let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
            let options = negotiate_writeback_test_session(&server, capable);
            assert_eq!(options.contains(FsOptions::WRITEBACK_CACHE), expected);
            let state = server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap();
            assert_eq!(state.options, options.bits());
            state.fs.validate_writeback(expected).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn default_restore_preserves_enabled_session_and_write_only_append_handles() {
        use super::super::snapshot::{FsSnapshot, ServerSnapshot};
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"abcd").unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
        let options = negotiate_writeback_test_session(&server, FsOptions::WRITEBACK_CACHE);
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let flags = (libc::O_WRONLY | libc::O_APPEND) as u32;
        let handle = fs.open(ctx, inode, false, flags).unwrap().0.unwrap();
        let saved = server
            .capture_state(fs.inode_alloc.snapshot_next())
            .unwrap();
        let saved: ServerSnapshot =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        drop((server, fs));

        let fs = writeback_test_overlay(temp.path(), true, None);
        let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
        server.restore_state(&saved).unwrap();
        fs.inode_alloc.restore_next(saved.next_inode).unwrap();
        let recaptured = server
            .capture_state(fs.inode_alloc.snapshot_next())
            .unwrap();
        assert_eq!(recaptured.options, options.bits());
        let (FsSnapshot::Overlay(old), FsSnapshot::Overlay(new)) = (&saved.fs, &recaptured.fs)
        else {
            panic!("overlay snapshots expected")
        };
        assert_eq!(
            serde_json::to_value(&new.handles).unwrap(),
            serde_json::to_value(&old.handles).unwrap()
        );
        for (old, new) in old.layers.iter().zip(&new.layers) {
            let (FsSnapshot::Passthrough(old), FsSnapshot::Passthrough(new)) = (old, new) else {
                panic!("native snapshots expected")
            };
            assert!(new.writeback);
            // Existing restore reopens the verified path with O_NOFOLLOW,
            // while normal OPEN follows the retained /proc/self/fd symlink.
            // All other flags and the saved handle state must be preserved.
            let mut expected_handles = old.handles.clone();
            for handle in &mut expected_handles {
                handle.flags |= libc::O_NOFOLLOW;
            }
            assert_eq!(
                serde_json::to_value(&new.handles).unwrap(),
                serde_json::to_value(&expected_handles).unwrap()
            );
        }
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        assert_eq!(
            fs.read(ctx, inode, handle, &mut output, 4, 0, None, 0)
                .unwrap(),
            4
        );
        assert_eq!(output.bytes, b"abcd");
        // WB allows partial-page READ on O_WRONLY; guest append offsets must
        // still reach the restored native FD without host O_APPEND rewriting.
        fs.write(
            ctx,
            inode,
            handle,
            WriteReader(io::Cursor::new(b"X".to_vec())),
            1,
            0,
            None,
            true,
            false,
            flags,
        )
        .unwrap();
        fs.fsync(ctx, inode, false, handle).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            b"Xbcd"
        );
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
        assert_eq!(
            fs.release(ctx, inode, 0, handle, false, false, None)
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restored_pre_init_snapshot_uses_the_new_session_preference() {
        for (setting, expected) in [(None, false), (Some("1"), true)] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let fs = writeback_test_overlay(temp.path(), false, None);
            let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
            let saved = server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap();
            assert_eq!(saved.options, 0);
            saved.fs.validate_writeback(false).unwrap();
            drop((server, fs));

            let fs = writeback_test_overlay(temp.path(), true, setting);
            let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
            server.restore_state(&saved).unwrap();
            fs.inode_alloc.restore_next(saved.next_inode).unwrap();
            assert_eq!(
                server
                    .capture_state(fs.inode_alloc.snapshot_next())
                    .unwrap()
                    .options,
                0
            );
            let options = negotiate_writeback_test_session(&server, FsOptions::WRITEBACK_CACHE);
            assert_eq!(options.contains(FsOptions::WRITEBACK_CACHE), expected);
            let recaptured = server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap();
            assert_eq!(recaptured.options, options.bits());
            recaptured.fs.validate_writeback(expected).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restored_enabled_session_reinitializes_using_the_new_session_preference() {
        for (setting, expected) in [(None, false), (Some("1"), true)] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let fs = writeback_test_overlay(temp.path(), false, Some("1"));
            let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
            negotiate_writeback_test_session(&server, FsOptions::WRITEBACK_CACHE);
            let saved = server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap();
            drop((server, fs));

            let fs = writeback_test_overlay(temp.path(), true, setting);
            let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
            server.restore_state(&saved).unwrap();
            fs.inode_alloc.restore_next(saved.next_inode).unwrap();
            server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap()
                .fs
                .validate_writeback(true)
                .unwrap();
            assert_eq!(
                writeback_test_control_request(&server, fuse::Opcode::Destroy, &[]).2,
                0
            );
            let options = negotiate_writeback_test_session(&server, FsOptions::WRITEBACK_CACHE);
            assert_eq!(options.contains(FsOptions::WRITEBACK_CACHE), expected);
            let recaptured = server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap();
            assert_eq!(recaptured.options, options.bits());
            recaptured.fs.validate_writeback(expected).unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enabled_snapshot_restore_keeps_explicit_off_and_session_mismatch_rejections() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
        negotiate_writeback_test_session(&server, FsOptions::WRITEBACK_CACHE);
        let saved = server
            .capture_state(fs.inode_alloc.snapshot_next())
            .unwrap();
        drop((server, fs));

        let fs = writeback_test_overlay(temp.path(), true, Some("0"));
        let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
        assert!(server.restore_state(&saved).is_err());
        assert_eq!(
            server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap()
                .options,
            0
        );
        drop((server, fs));

        let fs = writeback_test_overlay(temp.path(), true, None);
        let server = super::super::server::Server::new(WritebackTestFs(fs.clone()), true);
        let mut mismatched = saved.clone();
        mismatched.options &= !FsOptions::WRITEBACK_CACHE.bits();
        assert!(server.restore_state(&mismatched).is_err());
        assert_eq!(
            server
                .capture_state(fs.inode_alloc.snapshot_next())
                .unwrap()
                .options,
            0
        );
    }

    #[test]
    fn writeback_mode_and_write_only_append_handles_survive_snapshot_restore() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), b"abcd").unwrap();
        let config = passthrough::Config {
            root_dir: temp.path().to_string_lossy().into_owned(),
            writeback: true,
            ..Default::default()
        };
        let fs = PassthroughFs::new(config.clone(), Arc::new(InodeAllocator::new())).unwrap();
        assert!(fs
            .init(FsOptions::WRITEBACK_CACHE)
            .unwrap()
            .contains(FsOptions::WRITEBACK_CACHE));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let flags = (libc::O_WRONLY | libc::O_APPEND) as u32;
        let handle = fs.open(ctx, inode, false, flags).unwrap().0.unwrap();
        let state = fs.capture_state().unwrap();
        state.validate_writeback(true).unwrap();
        assert!(state.validate_writeback(false).is_err());
        let state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        drop(fs);
        let restored = PassthroughFs::new(config, Arc::new(InodeAllocator::new())).unwrap();
        restored.restore_state(&state).unwrap();
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        assert_eq!(
            restored
                .read(ctx, inode, handle, &mut output, 4, 0, None, 0)
                .unwrap(),
            4
        );
        assert_eq!(output.bytes, b"abcd");
        // Guest supplies append offsets after merging; the native fd must not
        // retain O_APPEND, which would override this explicit offset.
        restored
            .write(
                ctx,
                inode,
                handle,
                WriteReader(io::Cursor::new(b"X".to_vec())),
                1,
                0,
                None,
                true,
                false,
                flags,
            )
            .unwrap();
        restored.fsync(ctx, inode, false, handle).unwrap();
        assert_eq!(std::fs::read(temp.path().join("file")).unwrap(), b"Xbcd");
    }

    impl io::Read for WriteReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            io::Read::read(&mut self.0, bytes)
        }
    }

    impl ZeroCopyReader for WriteReader {
        fn read_to(
            &mut self,
            file: &std::fs::File,
            count: usize,
            offset: u64,
        ) -> io::Result<usize> {
            use std::os::unix::fs::FileExt;
            let mut bytes = vec![0; count];
            let count = io::Read::read(&mut self.0, &mut bytes)?;
            file.write_at(&bytes[..count], offset)
        }
    }

    #[test]
    fn detached_writeback_times_use_the_open_object_after_name_reuse() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"original").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        let mut attrs = fs.getattr(ctx, inode, Some(handle)).unwrap().0;
        fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        let (replacement, replacement_handle, _) = fs
            .create(
                ctx,
                fuse::ROOT_ID,
                c"file",
                0o600,
                false,
                (libc::O_RDWR | libc::O_CREAT) as u32,
                0,
                Extensions::default(),
            )
            .unwrap();
        attrs.st_mtime = 12345;
        attrs.st_mtime_nsec = 0;
        let (updated, _) = fs
            .setattr(
                ctx,
                inode,
                attrs,
                Some(handle),
                SetattrValid::MTIME | SetattrValid::CTIME,
            )
            .unwrap();
        assert_eq!(updated.st_mtime, 12345);
        assert_ne!(
            fs.getattr(ctx, replacement.inode, replacement_handle)
                .unwrap()
                .0
                .st_mtime,
            12345
        );
        attrs.st_size = 2;
        let (truncated, _) = fs
            .setattr(ctx, inode, attrs, Some(handle), SetattrValid::SIZE)
            .unwrap();
        assert_eq!(truncated.st_size, 2);
        assert_eq!(fs.getattr(ctx, inode, Some(handle)).unwrap().0.st_size, 2);
        assert_eq!(
            fs.getattr(ctx, replacement.inode, replacement_handle)
                .unwrap()
                .0
                .st_size,
            0
        );
        assert_eq!(
            std::fs::read(temp.path().join("lower/file")).unwrap(),
            b"original"
        );
        assert!(
            fs.setattr(ctx, inode, attrs, replacement_handle, SetattrValid::MTIME)
                .is_err()
        );
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
        fs.release(
            ctx,
            replacement.inode,
            0,
            replacement_handle.unwrap(),
            false,
            false,
            None,
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_writeback_after_release_keeps_identity_until_last_forget() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        fs.init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        std::fs::write(temp.path().join("upper/file"), b"old").unwrap();
        std::fs::hard_link(
            temp.path().join("upper/file"),
            temp.path().join("upper/alias"),
        )
        .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        assert_eq!(fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode, inode);
        let mut attrs = fs.getattr(ctx, inode, None).unwrap().0;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
        fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        std::fs::write(temp.path().join("upper/file"), b"replacement").unwrap();
        attrs.st_mtime = 12345;
        attrs.st_mtime_nsec = 0;
        fs.setattr(
            ctx,
            inode,
            attrs,
            None,
            SetattrValid::MTIME | SetattrValid::CTIME,
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(temp.path().join("upper/alias"))
                .unwrap()
                .mtime(),
            12345
        );
        assert_ne!(
            std::fs::metadata(temp.path().join("upper/file"))
                .unwrap()
                .mtime(),
            12345
        );
        assert_eq!(
            fs.capture_state().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        fs.forget(ctx, inode, 1);
        assert_eq!(fs.getattr(ctx, inode, None).unwrap().0.st_mtime, 12345);
        fs.forget(ctx, inode, 1);
        assert_eq!(
            fs.setattr(ctx, inode, attrs, None, SetattrValid::MTIME)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        assert!(fs.capture_state().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_writeback_lower_times_are_logical_and_never_touch_backing() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let lower = temp.path().join("lower/file");
        std::fs::write(&lower, b"immutable").unwrap();
        pvisor_overlay_core::sys::set_xattr(
            &lower,
            OsStr::new("user.containers.override_stat"),
            b"77:88:0100640",
            0,
        )
        .unwrap();
        pvisor_overlay_core::sys::set_xattr(
            &temp.path().join("lower"),
            OsStr::new("user.containers.override_stat"),
            b"0:0:040755",
            0,
        )
        .unwrap();
        let before = std::fs::metadata(&lower).unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        fs.init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let mut attrs = fs.getattr(ctx, inode, None).unwrap().0;
        fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        attrs.st_mtime = 12345;
        attrs.st_mtime_nsec = 0;
        attrs.st_ctime = 12346;
        let updated = fs
            .setattr(
                ctx,
                inode,
                attrs,
                None,
                SetattrValid::MTIME | SetattrValid::CTIME,
            )
            .unwrap()
            .0;
        assert_eq!(
            (updated.st_uid, updated.st_gid, updated.st_mtime),
            (77, 88, 12345)
        );
        let after = std::fs::metadata(&lower).unwrap();
        assert_eq!(
            (
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            ),
            (
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            )
        );
        assert_eq!(std::fs::read(&lower).unwrap(), b"immutable");
        assert!(fs
            .setattr(ctx, inode, attrs, None, SetattrValid::SIZE)
            .is_err());
        assert!(fs
            .setattr(ctx, inode + 9999, attrs, None, SetattrValid::MTIME)
            .is_err());
        fs.forget(ctx, inode, 1);
        assert!(fs.getattr(ctx, inode, None).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_writeback_rename_victim_survives_closed_handle_and_failed_mutation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        fs.init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        std::fs::write(temp.path().join("upper/victim"), b"victim").unwrap();
        std::fs::write(temp.path().join("upper/source"), b"source").unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"victim").unwrap().inode;
        let mut attrs = fs.getattr(ctx, inode, None).unwrap().0;
        assert!(fs
            .rename(ctx, fuse::ROOT_ID, c"missing", fuse::ROOT_ID, c"victim", 0)
            .is_err());
        assert!(fs.detached_writeback.lock().unwrap().is_empty());
        assert!(fs.capture_state().is_ok());
        fs.rename(ctx, fuse::ROOT_ID, c"source", fuse::ROOT_ID, c"victim", 0)
            .unwrap();
        attrs.st_mtime = 12345;
        attrs.st_mtime_nsec = 0;
        fs.setattr(ctx, inode, attrs, None, SetattrValid::MTIME)
            .unwrap();
        assert_eq!(fs.getattr(ctx, inode, None).unwrap().0.st_mtime, 12345);
        assert_ne!(
            std::fs::metadata(temp.path().join("upper/victim"))
                .unwrap()
                .mtime(),
            12345
        );
        assert_eq!(
            std::fs::read(temp.path().join("upper/victim")).unwrap(),
            b"source"
        );
        fs.forget(ctx, inode, 1);
        assert!(fs.capture_state().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_writeback_orphaned_directory_lookup_cannot_emit_invalid_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        fs.init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs
            .mkdir(ctx, fuse::ROOT_ID, c"dir", 0o755, 0, Extensions::default())
            .unwrap()
            .inode;
        fs.rmdir(ctx, fuse::ROOT_ID, c"dir").unwrap();
        assert_eq!(
            fs.capture_state().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        fs.forget(ctx, inode, 1);
        assert!(fs.capture_state().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detached_writeback_lookup_counts_roundtrip_and_legacy_retirement_is_conservative() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"original").unwrap();
        let fs = writeback_test_overlay(temp.path(), false, Some("1"));
        fs.init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        let snapshot = fs.capture_state().unwrap();
        let restored = writeback_test_overlay(temp.path(), true, None);
        restored.init(FsOptions::empty()).unwrap();
        restored.restore_state(&snapshot).unwrap();
        restored.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        fs.forget(ctx, inode, 1); // independent original owner cannot retire restored refs
        restored.forget(ctx, inode, 1);
        assert!(restored.getattr(ctx, inode, None).is_ok());
        restored.forget(ctx, inode, 1);
        assert!(restored.getattr(ctx, inode, None).is_err());
        // Independent unchanged backing for a legacy snapshot without nlookup.
        let legacy_temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(legacy_temp.path().join("lower")).unwrap();
        std::fs::write(legacy_temp.path().join("lower/file"), b"original").unwrap();
        let original = writeback_test_overlay(legacy_temp.path(), false, Some("1"));
        original
            .init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap();
        let legacy_inode = original.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let super::super::snapshot::FsSnapshot::Overlay(mut state) =
            original.capture_state().unwrap()
        else {
            unreachable!()
        };
        state.lookup_refs = None;
        state.legacy_lookup_refs.clear();
        let legacy = writeback_test_overlay(legacy_temp.path(), true, None);
        legacy.init(FsOptions::empty()).unwrap();
        legacy
            .restore_state(&super::super::snapshot::FsSnapshot::Overlay(state))
            .unwrap();
        legacy.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        legacy.forget(ctx, legacy_inode, u64::MAX);
        assert!(legacy.getattr(ctx, legacy_inode, None).is_ok());
        legacy.destroy();
        assert!(legacy.detached_writeback.lock().unwrap().is_empty());
    }

    #[test]
    fn writeback_keeps_owned_upper_pages_after_restore() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"original").unwrap();
        let config = parent_cache_fixture(temp.path()).snapshot_config.clone();
        let fs = OverlayFs::build_with_writeback(
            config.clone(),
            Arc::new(InodeAllocator::new()),
            false,
            Some("1"),
        )
        .unwrap();
        assert!(fs
            .init(FsOptions::WRITEBACK_CACHE | FsOptions::PVISOR_WRITEBACK)
            .unwrap()
            .contains(FsOptions::PVISOR_WRITEBACK));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let (handle, flags) = fs.open(ctx, inode, false, libc::O_RDWR as u32).unwrap();
        assert!(flags.contains(OpenOptions::KEEP_CACHE));
        fs.release(ctx, inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
        let snapshot = fs.capture_state().unwrap();
        drop(fs);
        let restored =
            OverlayFs::build_with_writeback(config, Arc::new(InodeAllocator::new()), true, None)
                .unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored.restore_state(&snapshot).unwrap();
        let (handle, flags) = restored
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(flags.contains(OpenOptions::KEEP_CACHE));
        restored
            .release(ctx, inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
    }

    #[test]
    fn restored_open_file_handles_support_leased_io_and_release() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"before").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        let config = fs.snapshot_config.clone();
        let wire = serde_json::to_vec(&fs.capture_state().unwrap()).unwrap();
        drop(fs);

        let restored = OverlayFs::open_existing(config, Arc::new(InodeAllocator::new())).unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored
            .restore_state(&serde_json::from_slice(&wire).unwrap())
            .unwrap();
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        assert_eq!(
            restored
                .read(ctx, inode, handle, &mut output, 6, 0, None, 0)
                .unwrap(),
            6
        );
        assert_eq!(output.bytes, b"before");
        assert_eq!(
            restored
                .write(
                    ctx,
                    inode,
                    handle,
                    WriteReader(io::Cursor::new(b"after!".to_vec())),
                    6,
                    0,
                    None,
                    false,
                    false,
                    0,
                )
                .unwrap(),
            6
        );
        restored.fsync(ctx, inode, false, handle).unwrap();
        restored
            .release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
        assert_eq!(
            restored.file_io_lease(handle).err().unwrap().raw_os_error(),
            Some(libc::EBADF)
        );
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            b"after!"
        );
        assert_eq!(
            std::fs::read(temp.path().join("lower/file")).unwrap(),
            b"before"
        );
    }

    #[test]
    fn backing_read_releases_the_operation_lock_and_allows_independent_write_and_fsync() {
        use std::sync::mpsc::channel;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/a"), b"first").unwrap();
        std::fs::write(temp.path().join("lower/b"), b"second").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let a = fs.lookup(ctx, fuse::ROOT_ID, c"a").unwrap().inode;
        let b = fs.lookup(ctx, fuse::ROOT_ID, c"b").unwrap().inode;
        let ah = fs
            .open(ctx, a, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let bh = fs
            .open(ctx, b, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        let (started, entered) = channel();
        let (resume, waiting) = channel();
        let (sent, completed) = channel();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                let mut writer = ReadWriter {
                    gate: Some((started, waiting)),
                    bytes: Vec::new(),
                };
                let result = fs.read(ctx, a, ah, &mut writer, 16, 0, None, 0);
                (result, writer.bytes)
            });
            if let Err(error) = entered.recv_timeout(Duration::from_secs(2)) {
                let _ = resume.send(());
                panic!("READ did not reach backing I/O: {error}");
            }
            let lock_is_free = fs.operation_lock.try_write().is_ok();
            scope.spawn(|| {
                let result = fs
                    .write(
                        ctx,
                        b,
                        bh,
                        WriteReader(io::Cursor::new(b"updated".to_vec())),
                        7,
                        0,
                        None,
                        false,
                        false,
                        0,
                    )
                    .and_then(|count| fs.fsync(ctx, b, false, bh).map(|()| count));
                sent.send(result).unwrap();
            });
            let result = completed.recv_timeout(Duration::from_secs(2));
            resume.send(()).unwrap();
            let (read, bytes) = first.join().unwrap();
            assert!(lock_is_free, "READ retained the global operation lock");
            assert_eq!(
                result
                    .expect("unrelated WRITE/FSYNC blocked on READ")
                    .unwrap(),
                7
            );
            assert_eq!(read.unwrap(), 5);
            assert_eq!(bytes, b"first");
        });
        fs.release(ctx, a, 0, ah, false, false, None).unwrap();
        fs.release(ctx, b, 0, bh, false, false, None).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("upper/b")).unwrap(),
            b"updated"
        );
        assert_eq!(
            std::fs::read(temp.path().join("lower/b")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn snapshot_global_drain_is_woken_by_the_last_file_io_lease() {
        use std::sync::mpsc::channel;
        use std::time::{Duration, Instant};
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/a"), b"first").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"a").unwrap().inode;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let lease = fs.file_io_lease(handle).unwrap();
        let (sent, received) = channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                sent.send(fs.capture_state()).unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(2);
            let installed = loop {
                if fs.io_activity.state.lock().unwrap().global != 0 {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::yield_now();
            };
            let premature = received.try_recv();
            drop(lease);
            let completed = received.recv_timeout(Duration::from_secs(2));
            assert!(installed, "snapshot did not install its global drain");
            assert!(premature.is_err(), "snapshot captured admitted I/O");
            completed
                .expect("last lease did not wake the global drain")
                .unwrap();
        });
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
    }

    #[test]
    fn native_descriptor_lease_survives_native_handle_removal() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/a"), b"first").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"a").unwrap().inode;
        let handle = fs
            .open(ctx, inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let lease = fs.file_io_lease(handle).unwrap();
        let native = &fs.layers[lease.handle.layer.0];
        native
            .release(
                ctx,
                lease.handle.inode,
                0,
                lease.handle.handle,
                false,
                false,
                None,
            )
            .unwrap();
        assert!(native
            .lease_handle(lease.handle.inode, lease.handle.handle)
            .is_err());
        let mut writer = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        assert_eq!(
            native
                .read_leased(&lease.native, &mut writer, 16, 0)
                .unwrap(),
            5
        );
        assert_eq!(writer.bytes, b"first");
        drop(lease);
        assert!(fs.io_activity.state.lock().unwrap().active.is_empty());
    }
    #[test]
    fn cached_directory_borrow_survives_eviction_and_releases_the_final_reference() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("lower/parent")).unwrap();
        std::fs::write(temp.path().join("lower/parent/file"), b"content").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let backing = fs
            .core
            .metadata_for_backing_lookup(Path::new("parent/file"))
            .unwrap();
        let entry = fs
            .inner_entry_with_parents(Layer(1), Path::new("parent/file"), ctx, &backing.parents)
            .unwrap();
        fs.layers[1].forget(ctx, entry.inode, 1);
        let reference = fs
            .directory_cache
            .lock()
            .unwrap()
            .entries
            .values()
            .next()
            .unwrap()
            .reference
            .clone();
        let inode = reference.inode;
        fs.clear_directory_cache();
        assert!(fs.directory_cache.lock().unwrap().entries.is_empty());
        assert!(fs.layers[1].getattr(ctx, inode, None).is_ok());
        let child = fs.layers[1].lookup(ctx, inode, c"file").unwrap();
        fs.layers[1].forget(ctx, child.inode, 1);
        drop(reference);
        assert!(fs.layers[1].getattr(ctx, inode, None).is_err());
    }

    #[test]
    fn concurrent_native_lookup_has_one_identity_and_keeps_each_returned_inode_alive() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        for index in 0..32 {
            std::fs::create_dir(temp.path().join(format!("lower/d{index}"))).unwrap();
        }
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let barrier = std::sync::Barrier::new(8);
        let results = Mutex::new(vec![Vec::new(); 32]);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let native = &fs.layers[1];
                let barrier = &barrier;
                let results = &results;
                scope.spawn(move || {
                    let ctx = Context {
                        uid: 0,
                        gid: 0,
                        pid: 1,
                    };
                    for index in 0..32 {
                        let name = CString::new(format!("d{index}")).unwrap();
                        barrier.wait();
                        let entry = native.lookup(ctx, fuse::ROOT_ID, &name);
                        barrier.wait(); // every lookup still owns a reference
                        let outcome = match entry {
                            Ok(entry) => {
                                let live = native.getattr(ctx, entry.inode, None).is_ok();
                                let inode = entry.inode;
                                native.forget(ctx, inode, 1);
                                (inode, live)
                            }
                            Err(_) => (0, false),
                        };
                        results.lock().unwrap()[index].push(outcome);
                    }
                });
            }
        });
        for round in results.into_inner().unwrap() {
            assert_eq!(round.len(), 8);
            assert!(
                round
                    .iter()
                    .all(|&(inode, live)| live && inode == round[0].0),
                "duplicate or prematurely forgotten native inode: {round:?}"
            );
        }
    }

    #[test]
    fn reads_overlap_but_release_waits_for_backing_io() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/a"), b"first").unwrap();
        std::fs::write(temp.path().join("lower/b"), b"second").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let a = fs.lookup(ctx, fuse::ROOT_ID, c"a").unwrap().inode;
        let b = fs.lookup(ctx, fuse::ROOT_ID, c"b").unwrap().inode;
        let ah = fs
            .open(ctx, a, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let bh = fs
            .open(ctx, b, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let (started, entered) = channel();
        let (resume, waiting) = channel();
        let (finished, completed) = channel();
        let fs = &fs;
        std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                let mut writer = ReadWriter {
                    gate: Some((started, waiting)),
                    bytes: Vec::new(),
                };
                let result = fs.read(ctx, a, ah, &mut writer, 16, 0, None, 0);
                (result, writer.bytes)
            });
            if let Err(error) = entered.recv_timeout(Duration::from_secs(2)) {
                let _ = resume.send(());
                panic!("backing READ did not reach its blocking point: {error}");
            }
            scope.spawn(move || {
                let mut writer = ReadWriter {
                    gate: None,
                    bytes: Vec::new(),
                };
                let result = fs.read(ctx, b, bh, &mut writer, 16, 0, None, 0);
                finished.send((result, writer.bytes)).unwrap();
            });
            let second = completed.recv_timeout(Duration::from_secs(2));
            if second.is_err() {
                resume.send(()).unwrap();
                panic!("independent READ serialized behind backing I/O");
            }
            let (result, bytes) = second.unwrap();
            let (released, done) = channel();
            scope.spawn(move || {
                let result = fs.release(ctx, a, libc::O_RDONLY as u32, ah, false, false, None);
                released.send(result).unwrap();
            });
            let premature = done.recv_timeout(Duration::from_millis(20));
            let (queried, query_done) = channel();
            scope.spawn(move || {
                queried
                    .send(
                        fs.lookup(ctx, fuse::ROOT_ID, c"b")
                            .map(|entry| entry.attr.st_size),
                    )
                    .unwrap();
            });
            // A blocked close must not exclude metadata on another object.
            let unrelated_metadata = query_done.recv_timeout(Duration::from_secs(2));
            resume.send(()).unwrap();
            assert_eq!(result.unwrap(), 6);
            assert_eq!(bytes, b"second");
            assert!(premature.is_err(), "release raced retained backing handle");
            assert_eq!(
                unrelated_metadata
                    .expect("RELEASE blocked unrelated LOOKUP")
                    .unwrap(),
                6
            );
            done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
            let (result, bytes) = first.join().unwrap();
            assert_eq!(result.unwrap(), 5);
            assert_eq!(bytes, b"first");
        });
        fs.release(ctx, b, libc::O_RDONLY as u32, bh, false, false, None)
            .unwrap();
    }

    #[test]
    fn readonly_open_overlaps_backing_read_but_writable_open_waits() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/a"), b"first").unwrap();
        std::fs::write(temp.path().join("lower/b"), b"second").unwrap();
        std::fs::write(temp.path().join("lower/c"), b"third").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let a = fs.lookup(ctx, fuse::ROOT_ID, c"a").unwrap().inode;
        let b = fs.lookup(ctx, fuse::ROOT_ID, c"b").unwrap().inode;
        let ah = fs
            .open(ctx, a, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let c = fs.lookup(ctx, fuse::ROOT_ID, c"c").unwrap().inode;
        let ch = fs
            .open(ctx, c, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let (started, entered) = channel();
        let (resume, waiting) = channel();
        let (opened, ready) = channel();
        let (write_started, write_entered) = channel();
        let (write_opened, write_ready) = channel();
        let fs = &fs;
        std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                let mut writer = ReadWriter {
                    gate: Some((started, waiting)),
                    bytes: Vec::new(),
                };
                fs.read(ctx, a, ah, &mut writer, 16, 0, None, 0).unwrap();
                assert_eq!(writer.bytes, b"first");
            });
            if let Err(error) = entered.recv_timeout(Duration::from_secs(2)) {
                let _ = resume.send(());
                panic!("backing READ did not reach its blocking point: {error}");
            }
            scope.spawn(move || {
                opened
                    .send(fs.open(ctx, b, false, libc::O_RDONLY as u32))
                    .unwrap();
            });
            let readonly = ready.recv_timeout(Duration::from_secs(2));
            if readonly.is_err() {
                resume.send(()).unwrap();
                panic!("independent read-only OPEN serialized behind backing READ");
            }
            let bh = match readonly.unwrap() {
                Ok((Some(handle), _)) => handle,
                result => {
                    resume.send(()).unwrap();
                    panic!("read-only OPEN failed: {result:?}");
                }
            };
            let (changed, change_ready) = channel();
            scope.spawn(move || {
                let result = (|| {
                    let wh = fs.open(ctx, b, false, libc::O_RDWR as u32)?.0.unwrap();
                    fs.release(ctx, b, libc::O_RDWR as u32, wh, false, false, None)?;
                    fs.rename(ctx, fuse::ROOT_ID, c"b", fuse::ROOT_ID, c"renamed", 0)?;
                    fs.unlink(ctx, fuse::ROOT_ID, c"renamed")?;
                    fs.release(ctx, b, libc::O_RDONLY as u32, bh, false, false, None)
                })();
                changed.send(result).unwrap();
            });
            let independent = change_ready.recv_timeout(Duration::from_secs(2));
            if independent.is_err() {
                resume.send(()).unwrap();
                panic!("independent namespace mutation waited for unrelated READ");
            }
            independent.unwrap().unwrap();
            scope.spawn(move || {
                write_started.send(()).unwrap();
                write_opened
                    .send(fs.open(ctx, a, false, libc::O_RDWR as u32))
                    .unwrap();
            });
            if let Err(error) = write_entered.recv_timeout(Duration::from_secs(2)) {
                resume.send(()).unwrap();
                panic!("writable OPEN did not start: {error}");
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !fs
                .io_activity
                .state
                .lock()
                .unwrap()
                .blocked
                .contains_key(&ah)
            {
                if std::time::Instant::now() >= deadline {
                    resume.send(()).unwrap();
                    panic!("same-object writer did not install its fence");
                }
                std::thread::yield_now();
            }
            // The namespace writer is now waiting for a, holding the metadata
            // write guard. New I/O admission on c must still proceed.
            let (read_done, read_ready) = channel();
            scope.spawn(move || {
                let mut writer = ReadWriter {
                    gate: None,
                    bytes: Vec::new(),
                };
                let result = fs.read(ctx, c, ch, &mut writer, 16, 0, None, 0);
                read_done.send((result, writer.bytes)).unwrap();
            });
            let other = read_ready.recv_timeout(Duration::from_secs(2));
            if other.is_err() {
                resume.send(()).unwrap();
                panic!("new unrelated I/O admission waited for the namespace writer");
            }
            let (result, bytes) = other.unwrap();
            assert_eq!(result.unwrap(), 5);
            assert_eq!(bytes, b"third");
            let premature = write_ready.recv_timeout(Duration::from_millis(20));
            resume.send(()).unwrap();
            assert!(
                premature.is_err(),
                "writable OPEN bypassed the mutation barrier"
            );
            let wh = write_ready
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .0
                .unwrap();
            first.join().unwrap();
            fs.release(ctx, a, libc::O_RDWR as u32, wh, false, false, None)
                .unwrap();
        });
        fs.release(ctx, a, libc::O_RDONLY as u32, ah, false, false, None)
            .unwrap();
        fs.release(ctx, c, libc::O_RDONLY as u32, ch, false, false, None)
            .unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("lower/b")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn new_stages_use_compact_journals_and_reopening_preserves_both_formats() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("file"), b"original").unwrap();
        for legacy in [false, true] {
            let stage = temp.path().join(format!("stage-{legacy}"));
            let journal = stage.join("preimages");
            let cfg = Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec![target.to_str().unwrap().into()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: stage.join("upper").to_str().unwrap().into(),
                work_dir: None,
                preimage_dir: Some(journal.to_str().unwrap().into()),
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            };
            if legacy {
                let core = OverlayCore::new_with_exclusions_and_preimages(
                    vec![target.clone()],
                    stage.join("upper"),
                    None,
                    vec![],
                    Some(journal.clone()),
                )
                .unwrap();
                core.observe_read(Path::new("file")).unwrap();
            }
            let fs = OverlayFs::new(cfg.clone(), Arc::new(InodeAllocator::new())).unwrap();
            fs.core.observe_read(Path::new("file")).unwrap();
            let original = pvisor_overlay_core::load_preimages(&journal).unwrap();
            assert_eq!(original.len(), 1);
            assert_eq!(journal.join("log-v2").exists(), !legacy);
            assert_eq!(journal.join("entries/format-v2.json").exists(), !legacy);
            drop(fs);
            let restored = OverlayFs::open_existing(cfg, Arc::new(InodeAllocator::new())).unwrap();
            std::fs::write(target.join("file"), b"later host edit").unwrap();
            restored.core.observe_read(Path::new("file")).unwrap();
            restored.core.copy_up(Path::new("file")).unwrap();
            assert_eq!(
                pvisor_overlay_core::load_preimages(&journal).unwrap()[0].state,
                original[0].state
            );
            std::fs::write(target.join("file"), b"original").unwrap();
        }
    }

    fn parent_cache_fixture(root: &Path) -> OverlayFs {
        let fs = OverlayFs::new(
            Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec![root.join("lower").to_string_lossy().into_owned()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: root.join("upper").to_string_lossy().into_owned(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths: vec!["private".into()],
                access_policy: pvisor_overlay_core::FileAccessPolicy::new(
                    vec!["private/**".into()],
                    vec![],
                )
                .unwrap(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        fs
    }

    #[cfg(target_os = "linux")]
    fn local_flush_fixture(root: &Path, immutable: bool, writeback: bool) -> OverlayFs {
        let mut config = parent_cache_fixture(root).snapshot_config.clone();
        config.lower_mutability = vec![if immutable {
            pvisor_overlay_core::LayerMutability::Immutable
        } else {
            pvisor_overlay_core::LayerMutability::Mutable
        }];
        let mut fs = OverlayFs::build_with_writeback(
            config,
            Arc::new(InodeAllocator::new()),
            false,
            Some(if writeback { "1" } else { "0" }),
        )
        .unwrap();
        fs.profile = pvisor_overlay_core::profile::Profile::enabled("local-flush-test");
        fs
    }

    #[cfg(target_os = "linux")]
    fn shared_qualification_fixture(
        root: &Path,
        shared: bool,
        data: bool,
        flush: bool,
    ) -> OverlayFs {
        let mut fs = negative_entry_fixture(root);
        fs.shared_open_qualification = shared;
        fs.profile
            .add("shared_open_qualification_enabled", u64::from(shared));
        for label in passthrough::OPEN_QUALIFICATION_COUNTERS {
            fs.profile.add(label, 0);
        }
        fs.configure_lower_data_cache(true, true, false).unwrap();
        fs.configure_readonly_flush_elision(true, true, false)
            .unwrap();
        let caps = if data {
            FsOptions::PVISOR_DATA_OBJECT
        } else {
            FsOptions::empty()
        } | if flush {
            FsOptions::PVISOR_LOCAL_FLUSH
        } else {
            FsOptions::empty()
        };
        let negotiated = fs.init(caps).unwrap();
        assert_eq!(negotiated.contains(FsOptions::PVISOR_DATA_OBJECT), data);
        assert_eq!(negotiated.contains(FsOptions::PVISOR_LOCAL_FLUSH), flush);
        fs
    }

    #[test]
    fn shared_open_qualification_setting_is_strict_and_defaults_off() {
        assert!(!shared_open_qualification_enabled(None).unwrap());
        assert!(!shared_open_qualification_enabled(Some(OsStr::new("0"))).unwrap());
        assert!(shared_open_qualification_enabled(Some(OsStr::new("1"))).unwrap());
        for value in [b"".as_slice(), b"true", b"2", b" 1", b"\xff"] {
            assert_eq!(
                shared_open_qualification_enabled(Some(OsStr::from_bytes(value)))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_open_qualification_preserves_ids_fixed_eof_copyup_and_fresh_fd_checks() {
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for shared in [false, true] {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            let fs = shared_qualification_fixture(temp.path(), shared, true, true);
            let inode = fs.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap().inode;
            let first = fs
                .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, true)
                .unwrap();
            let second = fs
                .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, true)
                .unwrap();
            assert!(first.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
            assert!(second.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
            let receipt = first.data_object.unwrap();
            assert_eq!(receipt.size, 5);
            assert_eq!(second.data_object, Some(receipt));
            assert_ne!(first.handle, second.handle);
            let profile = fs.profile.report().unwrap();
            for (label, legacy, combined) in [
                ("open_qualification_fgetfl_attempts", 4, 2),
                ("open_qualification_fstat_attempts", 4, 0),
                ("open_qualification_statx_attempts", 2, 2),
                ("open_qualification_fstatfs_attempts", 4, 2),
                ("open_qualification_shared_attempts", 0, 2),
                ("open_qualification_receipt_hits", 0, 2),
            ] {
                assert_eq!(
                    profile.measurements[label].units,
                    if shared { combined } else { legacy }
                );
            }
            assert_eq!(
                profile.measurements["lower_data_cache_object_registrations"].units,
                1
            );
            assert_eq!(
                profile.measurements["lower_data_cache_object_reuses"].units,
                1
            );
            // The old receipt API has no saved evidence: a later call must
            // perform fresh native qualification even when sharing is enabled.
            assert_eq!(
                fs.lower_data_object(inode, first.handle.unwrap()),
                Some(receipt)
            );
            let after = fs.profile.report().unwrap();
            for label in [
                "open_qualification_fgetfl_attempts",
                "open_qualification_fstat_attempts",
                "open_qualification_statx_attempts",
                "open_qualification_fstatfs_attempts",
            ] {
                assert_eq!(
                    after.measurements[label].units,
                    profile.measurements[label].units + 1
                );
            }
            let writer = fs
                .open_for_reply(ctx, inode, false, libc::O_RDWR as u32, false)
                .unwrap();
            assert!(writer.data_object.is_none());
            assert!(!writer.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
            std::fs::write(temp.path().join("upper/existing"), b"longer upper bytes").unwrap();
            let upper = fs
                .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, true)
                .unwrap();
            assert!(upper.data_object.is_none());
            assert!(!upper.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
            let mut bytes = ReadWriter {
                gate: None,
                bytes: vec![],
            };
            fs.read(
                ctx,
                inode,
                first.handle.unwrap(),
                &mut bytes,
                64,
                0,
                None,
                0,
            )
            .unwrap();
            assert_eq!(bytes.bytes, b"lower");
            for opened in [first, second, writer, upper] {
                let handle = opened.handle.unwrap();
                fs.release(ctx, inode, 0, handle, false, false, None)
                    .unwrap();
                assert!(fs.lower_data_object(inode, handle).is_none());
            }
            assert!(fs.handles.lock().unwrap().is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_open_qualification_preserves_feature_independence_and_snapshot_schema() {
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for (data, flush) in [(false, false), (false, true), (true, false), (true, true)] {
            let temp = tempfile::tempdir_in("/dev/shm").unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            std::fs::write(temp.path().join("lower/linked"), b"aliased").unwrap();
            std::fs::hard_link(
                temp.path().join("lower/linked"),
                temp.path().join("lower/alias"),
            )
            .unwrap();
            let fs = shared_qualification_fixture(temp.path(), true, data, flush);
            let inode = fs.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap().inode;
            let reply = fs
                .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, data)
                .unwrap();
            assert_eq!(reply.data_object.is_some(), data);
            assert_eq!(
                reply.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH),
                flush
            );
            let profile = fs.profile.report().unwrap();
            assert_eq!(
                profile.measurements["open_qualification_fgetfl_attempts"].units,
                u64::from(data || flush)
            );
            assert_eq!(
                profile.measurements["open_qualification_fstat_attempts"].units,
                u64::from(!data && flush)
            );
            assert_eq!(
                profile.measurements["open_qualification_statx_attempts"].units,
                u64::from(data)
            );
            assert_eq!(
                profile.measurements["open_qualification_fstatfs_attempts"].units,
                u64::from(data || flush)
            );
            let linked = fs.lookup(ctx, fuse::ROOT_ID, c"linked").unwrap().inode;
            let linked_reply = fs
                .open_for_reply(ctx, linked, false, libc::O_RDONLY as u32, data)
                .unwrap();
            assert!(linked_reply.data_object.is_none());
            assert_eq!(
                linked_reply
                    .options
                    .contains(OpenOptions::PVISOR_LOCAL_FLUSH),
                flush
            );
            let mut bytes = ReadWriter {
                gate: None,
                bytes: vec![],
            };
            fs.read(
                ctx,
                linked,
                linked_reply.handle.unwrap(),
                &mut bytes,
                32,
                0,
                None,
                0,
            )
            .unwrap();
            assert_eq!(bytes.bytes, b"aliased");
            fs.release(
                ctx,
                linked,
                0,
                linked_reply.handle.unwrap(),
                false,
                false,
                None,
            )
            .unwrap();
            if !data && !flush {
                let state = fs.capture_state().unwrap();
                let serialized = serde_json::to_value(&state).unwrap();
                assert!(!serialized.to_string().contains("qualification"));
                let mut restored = OverlayFs::open_existing(
                    fs.snapshot_config.clone(),
                    Arc::new(InodeAllocator::new()),
                )
                .unwrap();
                restored.shared_open_qualification = false;
                restored.restore_state(&state).unwrap();
                assert!(!restored.shared_open_qualification);
                let restored_reply = restored
                    .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, false)
                    .unwrap();
                assert!(restored_reply.data_object.is_none());
                restored
                    .release(
                        ctx,
                        inode,
                        0,
                        restored_reply.handle.unwrap(),
                        false,
                        false,
                        None,
                    )
                    .unwrap();
            } else {
                assert_eq!(
                    fs.capture_state().unwrap_err().kind(),
                    io::ErrorKind::Unsupported
                );
            }
            fs.release(ctx, inode, 0, reply.handle.unwrap(), false, false, None)
                .unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_open_qualification_keeps_core_denials_and_live_mutable_reopens() {
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("file"), b"before").unwrap();
        std::fs::write(lower.join("private/key"), b"denied").unwrap();
        let mut fs = parent_cache_fixture(temp.path());
        fs.shared_open_qualification = true;
        fs.configure_lower_data_cache(true, true, false).unwrap();
        fs.configure_readonly_flush_elision(true, true, false)
            .unwrap();
        let negotiated = fs
            .init(FsOptions::PVISOR_DATA_OBJECT | FsOptions::PVISOR_LOCAL_FLUSH)
            .unwrap();
        assert!(
            !negotiated.intersects(FsOptions::PVISOR_DATA_OBJECT | FsOptions::PVISOR_LOCAL_FLUSH)
        );
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let denied = fs.allocate_inode(PathBuf::from("private/key"));
        assert_eq!(
            fs.open_for_reply(ctx, denied, false, libc::O_RDONLY as u32, true)
                .err()
                .expect("Core denial must precede native qualification")
                .raw_os_error(),
            Some(libc::EACCES)
        );
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        for expected in [b"before".as_slice(), b"after external change".as_slice()] {
            std::fs::write(lower.join("file"), expected).unwrap();
            let reply = fs
                .open_for_reply(ctx, inode, false, libc::O_RDONLY as u32, true)
                .unwrap();
            assert!(reply.data_object.is_none());
            assert!(!reply.options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
            let mut bytes = ReadWriter {
                gate: None,
                bytes: vec![],
            };
            fs.read(
                ctx,
                inode,
                reply.handle.unwrap(),
                &mut bytes,
                64,
                0,
                None,
                0,
            )
            .unwrap();
            assert_eq!(bytes.bytes, expected);
            fs.release(ctx, inode, 0, reply.handle.unwrap(), false, false, None)
                .unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_open_qualification_keeps_second_backend_check_and_no_reusable_evidence() {
        use pvisor_overlay_core::backend::{BackendAttachment, FileAttr, ReadOnlyBackend};
        struct UnavailableProjection;
        impl ReadOnlyBackend for UnavailableProjection {
            fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
            fn prepare_directory(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
            fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
            fn read_at(&self, _: &Path, _: u64, _: u32) -> io::Result<Vec<u8>> {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
            fn materialize_file(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
            fn materialize_tree(&self, path: &Path) -> io::Result<()> {
                self.prepare_metadata(path)
            }
        }
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let fs = shared_qualification_fixture(temp.path(), true, true, true);
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap().inode;
        let opened = fs
            .open_native_for_reply(ctx, inode, false, libc::O_RDONLY as u32, true)
            .unwrap();
        assert!(opened.data_identity.as_ref().unwrap().is_some());
        let handle = opened.reply.handle.unwrap();
        let attachment =
            BackendAttachment::new(&temp.path().join("lower"), Arc::new(UnavailableProjection))
                .unwrap();
        assert!(fs
            .lower_data_object_with_identity(inode, handle, opened.data_identity)
            .is_none());
        assert!(fs.lower_data_objects.lock().unwrap().ids.is_empty());
        let before = fs.profile.report().unwrap();
        assert_eq!(
            before.measurements["open_qualification_receipt_hits"].units,
            0
        );
        drop(attachment);
        assert!(fs.lower_data_object(inode, handle).is_some());
        let after = fs.profile.report().unwrap();
        assert_eq!(
            after.measurements["open_qualification_legacy_data_attempts"].units,
            1
        );
        fs.release(ctx, inode, 0, handle, false, false, None)
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_open_qualification_flows_through_augmentation_without_marking_virtual_files() {
        use super::super::augment_fs::AugmentFs;
        use super::super::virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent};
        let temp = tempfile::tempdir_in("/dev/shm").unwrap();
        let inner = shared_qualification_fixture(temp.path(), true, true, true);
        let allocator = inner.inode_alloc.clone();
        let fs = AugmentFs::new(
            inner,
            &allocator,
            vec![VirtualDirEntry {
                name: CString::new("synthetic").unwrap(),
                entry: VirtualEntry {
                    mode: 0o444,
                    one_shot: false,
                    content: VirtualEntryContent::File {
                        data: Arc::from(b"virtual".as_slice()),
                    },
                },
            }],
        );
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let ordinary = fs.lookup(ctx, fuse::ROOT_ID, c"existing").unwrap().inode;
        let ordinary_reply = fs
            .open_for_reply(ctx, ordinary, false, libc::O_RDONLY as u32, true)
            .unwrap();
        assert!(ordinary_reply.data_object.is_some());
        assert!(ordinary_reply
            .options
            .contains(OpenOptions::PVISOR_LOCAL_FLUSH));
        let virtual_inode = fs.lookup(ctx, fuse::ROOT_ID, c"synthetic").unwrap().inode;
        let virtual_reply = fs
            .open_for_reply(ctx, virtual_inode, false, libc::O_RDONLY as u32, true)
            .unwrap();
        assert!(virtual_reply.data_object.is_none());
        assert_eq!(virtual_reply.handle, Some(0));
        assert!(!virtual_reply
            .options
            .contains(OpenOptions::PVISOR_LOCAL_FLUSH));
        let profile = fs
            .diagnostic_profiles()
            .into_iter()
            .find(|p| {
                p.measurements
                    .contains_key("open_qualification_shared_attempts")
            })
            .unwrap();
        assert_eq!(
            profile.measurements["open_qualification_shared_attempts"].units,
            1
        );
        for (inode, reply, expected) in [
            (ordinary, ordinary_reply, b"lower".as_slice()),
            (virtual_inode, virtual_reply, b"virtual".as_slice()),
        ] {
            let mut bytes = ReadWriter {
                gate: None,
                bytes: vec![],
            };
            fs.read(
                ctx,
                inode,
                reply.handle.unwrap(),
                &mut bytes,
                32,
                0,
                None,
                0,
            )
            .unwrap();
            assert_eq!(bytes.bytes, expected);
            fs.release(ctx, inode, 0, reply.handle.unwrap(), false, false, None)
                .unwrap();
        }
    }

    #[test]
    fn lower_data_object_ids_are_bounded_never_recycled_and_layer_specific() {
        let mut table = LowerDataObjects::default();
        let first = (1, 2, 3, 4, 5);
        let (a, fresh) = table.register(first).unwrap();
        assert!(fresh);
        assert_eq!(a, LowerDataObject { id: 1, size: 5 });
        assert_eq!(table.register(first), Some((a, false)));
        for index in 1..MAX_LOWER_DATA_OBJECTS {
            let key = (1, 2, index as u64 + 3, 4, 5);
            assert_eq!(table.register(key).unwrap().0.id, index as u32 + 1);
        }
        assert!(table.register((2, 2, 3, 4, 5)).is_none());
        assert_eq!(table.register(first), Some((a, false))); // hit still works at limit
        assert_eq!(table.ids.len(), MAX_LOWER_DATA_OBJECTS);
        let mut layers = LowerDataObjects::default();
        assert_ne!(
            layers.register(first).unwrap().0.id,
            layers.register((2, 2, 3, 4, 5)).unwrap().0.id
        );
        assert_ne!(
            layers.register(first).unwrap().0.id,
            layers.register((1, 2, 3, 4, 6)).unwrap().0.id
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lower_data_objects_preserve_open_checks_fixed_eof_and_old_lower_binding() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("file"), b"lower").unwrap();
        std::fs::write(lower.join("private/key"), b"denied").unwrap();
        std::fs::write(lower.join("linked"), b"linked").unwrap();
        std::fs::hard_link(lower.join("linked"), lower.join("alias")).unwrap();
        let mut fs = local_flush_fixture(temp.path(), true, false);
        fs.configure_lower_data_cache(true, true, false).unwrap();
        assert!(fs
            .init(FsOptions::PVISOR_DATA_OBJECT)
            .unwrap()
            .contains(FsOptions::PVISOR_DATA_OBJECT));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let inode = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap().inode;
        let (h1, opts) = fs.open(ctx, inode, false, libc::O_RDONLY as u32).unwrap();
        assert!(opts.contains(OpenOptions::PVISOR_DATA_OBJECT));
        let h1 = h1.unwrap();
        let receipt = fs.lower_data_object(inode, h1);
        if let Some(receipt) = receipt {
            assert_ne!(receipt.id, 0);
            assert_eq!(receipt.size, 5);
        }
        assert!(fs.lower_data_object(inode + 100, h1).is_none());
        let (h2, _) = fs.open(ctx, inode, false, libc::O_RDONLY as u32).unwrap();
        let h2 = h2.unwrap();
        assert_eq!(fs.lower_data_object(inode, h2), receipt);
        fs.release(ctx, inode, 0, h2, false, false, None).unwrap();
        let (writer, opts) = fs.open(ctx, inode, false, libc::O_RDWR as u32).unwrap();
        assert!(!opts.contains(OpenOptions::PVISOR_DATA_OBJECT));
        assert!(fs.lower_data_object(inode, writer.unwrap()).is_none());
        std::fs::write(temp.path().join("upper/file"), b"upper grew substantially").unwrap();
        assert_eq!(fs.lower_data_object(inode, h1), receipt); // fixed lower EOF
        let mut bytes = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        fs.read(ctx, inode, h1, &mut bytes, 64, 0, None, 0).unwrap();
        assert_eq!(bytes.bytes, b"lower");
        let (upper, opts) = fs.open(ctx, inode, false, libc::O_RDONLY as u32).unwrap();
        assert!(!opts.contains(OpenOptions::PVISOR_DATA_OBJECT));
        assert!(fs.lower_data_object(inode, upper.unwrap()).is_none());
        // This fixture denies private/**. Core conservatively rejects every
        // multi-link regular file when any denial exists, before native OPEN.
        // The native descriptor test independently covers nlink qualification.
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"linked")
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        let denied = fs.allocate_inode(PathBuf::from("private/key"));
        assert!(fs.open(ctx, denied, false, libc::O_RDONLY as u32).is_err());
        for (node, handle) in [
            (inode, h1),
            (inode, writer.unwrap()),
            (inode, upper.unwrap()),
        ] {
            fs.release(ctx, node, 0, handle, false, false, None)
                .unwrap();
        }
        assert!(fs.handles.lock().unwrap().is_empty());
        if receipt.is_some() {
            assert_eq!(fs.lower_data_objects.lock().unwrap().ids.len(), 1);
        }
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"lower");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lower_data_configuration_fallback_snapshot_and_destroy_lifetime() {
        for (requested, guest_cache, dax, immutable, wb, guest, expected) in [
            (false, true, false, true, false, true, false),
            (true, false, false, true, false, true, false),
            (true, true, true, true, false, true, false),
            (true, true, false, false, false, true, false),
            (true, true, false, true, true, true, false),
            (true, true, false, true, false, false, false),
            (true, true, false, true, false, true, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let mut fs = local_flush_fixture(temp.path(), immutable, wb);
            fs.configure_lower_data_cache(requested, guest_cache, dax)
                .unwrap();
            assert_eq!(
                fs.configure_lower_data_cache(requested, guest_cache, dax)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            let old_state = fs.capture_state().unwrap();
            let caps = FsOptions::WRITEBACK_CACHE
                | if guest {
                    FsOptions::PVISOR_DATA_OBJECT
                } else {
                    FsOptions::empty()
                };
            assert_eq!(
                fs.init(caps)
                    .unwrap()
                    .contains(FsOptions::PVISOR_DATA_OBJECT),
                expected
            );
            assert_eq!(
                fs.profile.report().unwrap().measurements["lower_data_cache_enabled"].units,
                u64::from(expected)
            );
            if expected {
                assert_eq!(
                    fs.capture_state().unwrap_err().kind(),
                    io::ErrorKind::Unsupported
                );
                assert_eq!(
                    fs.restore_state(&old_state).unwrap_err().kind(),
                    io::ErrorKind::Unsupported
                );
                fs.lower_data_objects
                    .lock()
                    .unwrap()
                    .register((1, 2, 3, 4, 5))
                    .unwrap();
            } else {
                fs.capture_state().unwrap();
            }
            fs.destroy();
            assert!(!fs.lower_data_cache_active.load(Ordering::Relaxed));
            if expected {
                assert_eq!(fs.lower_data_objects.lock().unwrap().ids.len(), 1);
            }
            fs.init(FsOptions::empty()).unwrap();
            if expected {
                assert_eq!(
                    fs.capture_state().unwrap_err().kind(),
                    io::ErrorKind::Unsupported
                );
            } else {
                fs.capture_state().unwrap();
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_flush_negotiation_requires_explicit_compatible_configuration() {
        for (requested, guest_cache, dax, immutable, complete, writeback, guest, expected) in [
            (false, true, false, true, true, false, true, false),
            (true, false, false, true, true, false, true, false),
            (true, true, true, true, true, false, true, false),
            (true, true, false, false, true, false, true, false),
            (true, true, false, true, false, false, true, false),
            (true, true, false, true, true, true, true, false),
            (true, true, false, true, true, false, false, false),
            (true, true, false, true, true, false, true, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let mut fs = local_flush_fixture(temp.path(), immutable, writeback);
            if !complete {
                fs.snapshot_config.semantics = passthrough::PermissionSemantics::LinuxSimplified;
            }
            fs.configure_readonly_flush_elision(requested, guest_cache, dax)
                .unwrap();
            assert_eq!(
                fs.configure_readonly_flush_elision(requested, guest_cache, dax)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            let capable = FsOptions::WRITEBACK_CACHE
                | if guest {
                    FsOptions::PVISOR_LOCAL_FLUSH
                } else {
                    FsOptions::empty()
                };
            let options = fs.init(capable).unwrap();
            assert_eq!(options.contains(FsOptions::PVISOR_LOCAL_FLUSH), expected);
            let profile = fs.profile.report().unwrap();
            assert_eq!(
                profile.measurements["readonly_flush_elision_requested"].units,
                u64::from(requested)
            );
            assert_eq!(
                profile.measurements["readonly_flush_elision_enabled"].units,
                u64::from(expected)
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_flush_open_flags_preserve_native_binding_and_exclude_upper_writes_and_denials() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("file"), b"lower").unwrap();
        std::fs::write(lower.join("private/key"), b"denied").unwrap();
        let mut fs = local_flush_fixture(temp.path(), true, false);
        std::fs::write(temp.path().join("upper/upper"), b"upper").unwrap();
        fs.configure_readonly_flush_elision(true, true, false)
            .unwrap();
        fs.init(FsOptions::PVISOR_LOCAL_FLUSH).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        let (handle, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        let handle = handle.unwrap();
        let native = fs.handles.lock().unwrap().get(&handle).cloned().unwrap();
        let Handle::File(native) = native else {
            panic!("expected native file")
        };
        let qualified =
            fs.layers[native.layer.0].readonly_local_flush_eligible(native.inode, native.handle);
        assert_eq!(options.contains(OpenOptions::PVISOR_LOCAL_FLUSH), qualified);
        assert_eq!(native.layer.0, 1);
        assert!(!options.contains(OpenOptions::KEEP_CACHE)); // workspace data policy is unchanged
        let (writable, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDWR as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
        let (upper, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::PVISOR_LOCAL_FLUSH));
        let denied = fs.allocate_inode(PathBuf::from("private/key"));
        assert!(fs.open(ctx, denied, false, libc::O_RDONLY as u32).is_err());
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"private").is_err());
        for handle in [Some(handle), writable, upper].into_iter().flatten() {
            fs.release(ctx, entry.inode, 0, handle, false, false, None)
                .unwrap();
        }
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"lower");
        assert!(fs.handles.lock().unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_flush_snapshot_boundary_and_destroy_reset_keep_old_sessions_compatible() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let mut fs = local_flush_fixture(temp.path(), true, false);
        fs.configure_readonly_flush_elision(true, true, false)
            .unwrap();
        fs.init(FsOptions::empty()).unwrap(); // old firmware never negotiates
        fs.capture_state().unwrap();
        fs.init(FsOptions::PVISOR_LOCAL_FLUSH).unwrap();
        assert_eq!(
            fs.capture_state().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        fs.destroy();
        assert!(!fs.readonly_flush_elision_active.load(Ordering::Relaxed));
        fs.init(FsOptions::empty()).unwrap();
        fs.capture_state().unwrap();
        fs.destroy();
        fs.init(FsOptions::PVISOR_LOCAL_FLUSH).unwrap();
        assert!(fs.readonly_flush_elision_active.load(Ordering::Relaxed));
        assert_eq!(
            fs.profile.report().unwrap().measurements["readonly_flush_elision_enabled"].units,
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn regular_metadata_reply_keeps_the_pinned_root_after_host_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let original = temp.path().join("original-lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"old root").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        std::fs::rename(&lower, &original).unwrap();
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"different root contents").unwrap();
        assert_eq!(fs.core.metadata(Path::new("file")).unwrap().len(), 23);
        let before = std::fs::symlink_metadata(original.join("file")).unwrap();
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(entry.attr.st_size, 8);
        assert_eq!(fs.getattr(ctx, entry.inode, None).unwrap().0.st_size, 8);
        let after = std::fs::symlink_metadata(original.join("file")).unwrap();
        assert_eq!(
            (after.atime(), after.atime_nsec()),
            (before.atime(), before.atime_nsec())
        );
        let handle = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        assert_eq!(
            fs.read(ctx, entry.inode, handle, &mut output, 64, 0, None, 0)
                .unwrap(),
            8
        );
        assert_eq!(output.bytes, b"old root");
        fs.release(ctx, entry.inode, 0, handle, false, false, None)
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn regular_metadata_reply_revalidates_parents_and_rejects_symlink_retargeting() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("directory")).unwrap();
        std::fs::write(lower.join("directory/file"), b"old").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let directory = fs.lookup(ctx, fuse::ROOT_ID, c"directory").unwrap().inode;
        let original = fs.lookup(ctx, directory, c"file").unwrap();
        assert_eq!(original.attr.st_size, 3);
        assert_eq!(fs.getattr(ctx, original.inode, None).unwrap().0.st_size, 3);
        std::fs::rename(lower.join("directory"), lower.join("old-directory")).unwrap();
        std::fs::create_dir(lower.join("directory")).unwrap();
        std::fs::write(lower.join("directory/file"), b"updated").unwrap();
        let replaced = fs.lookup(ctx, directory, c"file").unwrap();
        assert_eq!(replaced.attr.st_size, 7);
        let handle = fs
            .open(ctx, replaced.inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        fs.read(ctx, replaced.inode, handle, &mut output, 64, 0, None, 0)
            .unwrap();
        assert_eq!(output.bytes, b"updated");
        fs.release(ctx, replaced.inode, 0, handle, false, false, None)
            .unwrap();
        std::fs::rename(lower.join("directory"), lower.join("replaced-directory")).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("file"), b"must not follow the ancestor link").unwrap();
        std::os::unix::fs::symlink(&outside, lower.join("directory")).unwrap();
        assert_eq!(
            fs.lookup(ctx, directory, c"file")
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::ENOTDIR)
        );
        let symlink = fs.lookup(ctx, fuse::ROOT_ID, c"directory").unwrap();
        assert_eq!(symlink.attr.st_mode & libc::S_IFMT, libc::S_IFLNK);
        assert_eq!(
            fs.getattr(ctx, symlink.inode, None).unwrap().0.st_mode & libc::S_IFMT,
            libc::S_IFLNK
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn regular_metadata_hint_preserves_leaf_replacement_type_and_absence() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"original").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let path = Path::new("file");
        let inode = fs.allocate_inode(path.to_path_buf());
        let backing = fs.core.metadata_for_backing_lookup(path).unwrap();
        std::fs::remove_file(lower.join("file")).unwrap();
        std::os::unix::fs::symlink("outside", lower.join("file")).unwrap();
        let entry = fs.entry_on_backing(ctx, path, inode, &backing).unwrap();
        assert_eq!(entry.attr.st_mode & libc::S_IFMT, libc::S_IFLNK);
        assert_eq!(entry.attr.st_size, 7);
        std::fs::remove_file(lower.join("file")).unwrap();
        std::fs::create_dir(lower.join("file")).unwrap();
        let entry = fs.entry_on_backing(ctx, path, inode, &backing).unwrap();
        assert_eq!(entry.attr.st_mode & libc::S_IFMT, libc::S_IFDIR);
        std::fs::remove_dir(lower.join("file")).unwrap();
        assert_eq!(
            fs.entry_on_backing(ctx, path, inode, &backing)
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn regular_metadata_reply_keeps_upper_virtual_owners_and_hardlink_denials() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        std::fs::create_dir(&lower).unwrap();
        std::fs::create_dir(&upper).unwrap();
        let marker = OsStr::new("user.containers.override_stat");
        pvisor_overlay_core::sys::set_xattr(&upper, marker, b"1234:2345:700", 0).unwrap();
        std::fs::write(upper.join("file"), b"upper bytes").unwrap();
        pvisor_overlay_core::sys::set_xattr(&upper.join("file"), marker, b"1234:2345:751", 0)
            .unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!((entry.attr.st_uid, entry.attr.st_gid), (1234, 2345));
        assert_eq!(entry.attr.st_mode & libc::S_IFMT, libc::S_IFREG);
        assert_eq!(entry.attr.st_mode & 0o7777, 0o751);
        assert_eq!(entry.attr.st_size, 11);
        std::fs::write(upper.join("file"), b"fresh upper metadata").unwrap();
        let (attr, _) = fs.getattr(ctx, entry.inode, None).unwrap();
        assert_eq!((attr.st_uid, attr.st_gid), (1234, 2345));
        assert_eq!(attr.st_mode & 0o7777, 0o751);
        assert_eq!(attr.st_size, 20);
        std::fs::write(lower.join("public"), b"protected alias").unwrap();
        std::fs::create_dir(lower.join("private")).unwrap();
        std::fs::hard_link(lower.join("public"), lower.join("private/alias")).unwrap();
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"public")
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"private").is_err());
    }

    #[test]
    fn restored_exclusions_only_add_unobserved_private_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = parent_cache_fixture(temp.path());
        let super::super::snapshot::FsSnapshot::Overlay(mut state) = fs.capture_state().unwrap()
        else {
            panic!("overlay expected")
        };
        state.nodes.push((123, b"visible/file".to_vec()));
        let original = state.config.excluded_paths.clone();
        for paths in [
            vec![],
            vec![PathBuf::from("private"), PathBuf::from("visible")],
            vec![PathBuf::from("private"), PathBuf::from("../outside")],
            vec![PathBuf::from("private"), PathBuf::from("/absolute")],
        ] {
            assert!(state.rebind_exclusions(&paths).is_err());
            assert_eq!(
                state.config.excluded_paths, original,
                "failed rebinding changed exclusions"
            );
        }
        state
            .rebind_exclusions(&[PathBuf::from("private"), PathBuf::from("tmp/new-control")])
            .unwrap();
        assert_eq!(state.config.excluded_paths, ["private", "tmp/new-control"]);
        assert_eq!(state.nodes.last().unwrap().1, b"visible/file");
    }

    #[test]
    fn overlay_negotiates_its_directory_capabilities_independently_of_layers() {
        let directory = FsOptions::DO_READDIRPLUS | FsOptions::READDIRPLUS_AUTO;
        for capable in [
            FsOptions::empty(),
            FsOptions::READDIRPLUS_AUTO,
            FsOptions::DO_READDIRPLUS,
            directory,
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            let fs = parent_cache_fixture(temp.path());
            let negotiated = fs.init(capable).unwrap();
            let expected = if capable.contains(FsOptions::DO_READDIRPLUS) {
                capable & directory
            } else {
                FsOptions::empty()
            };
            assert_eq!(negotiated & directory, expected);
            fs.destroy();
        }
    }

    #[test]
    fn copied_snapshot_discards_receipts_from_the_original_generation() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = temp.path().join("source");
        let copied = temp.path().join("copied");
        std::fs::create_dir_all(source.join("lower")).unwrap();
        std::fs::write(source.join("lower/file"), b"content").unwrap();
        let mut fs = parent_cache_fixture(&source);
        let digest = |bytes: &[u8]| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let content_digest = digest(b"content");
        let index = pvisor_overlay_core::encode_content_index([(
            Path::new("file"),
            content_digest.as_str(),
        )])
        .unwrap();
        let receipt = temp.path().join("content-index.bin");
        std::fs::write(&receipt, &index).unwrap();
        let index = crate::api::BaselineContentIndex {
            root: source.join("lower"),
            file: receipt,
            sha256: digest(&index),
        };
        fs.core = fs
            .core
            .with_immutable_content_index(&index.root, index.file.clone(), &index.sha256)
            .unwrap();
        fs.snapshot_config.baseline_content_index = Some(index);
        let super::super::snapshot::FsSnapshot::Overlay(mut snapshot) = fs.capture_state().unwrap()
        else {
            panic!("expected overlay");
        };
        pvisor::environment_snapshot::copy_owned_tree(&source, &copied).unwrap();
        snapshot.rebind_owned_copy(&source, &copied).unwrap();
        assert!(snapshot.config.baseline_content_index.is_none());
        let restored =
            OverlayFs::open_existing(snapshot.config.clone(), Arc::new(InodeAllocator::new()))
                .unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored
            .restore_state(&super::super::snapshot::FsSnapshot::Overlay(snapshot))
            .unwrap();
        assert_eq!(restored.core.metadata(Path::new("file")).unwrap().len(), 7);
    }

    #[test]
    fn subtree_index_keeps_component_siblings_and_inode_identities() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        let fs = parent_cache_fixture(temp.path());
        let paths = ["a", "a/x", "a/x/y", "a-b", "a0", "ab/x", "z"];
        let inodes = paths.map(|path| fs.allocate_inode(PathBuf::from(path)));
        fs.remap_path(Path::new("a"), Path::new("renamed"));
        for (index, path) in paths.iter().enumerate() {
            let expected = if index < 3 {
                path.replacen('a', "renamed", 1)
            } else {
                path.to_string()
            };
            assert_eq!(fs.path(inodes[index]).unwrap(), PathBuf::from(expected));
        }
        fs.remove_path(Path::new("renamed/x"));
        assert!(fs.path(inodes[1]).is_err());
        assert!(fs.path(inodes[2]).is_err());
        for index in [0, 3, 4, 5, 6] {
            assert!(fs.path(inodes[index]).is_ok());
        }
        let nodes = fs.nodes.lock().unwrap();
        assert_eq!(nodes.by_path.len(), nodes.by_inode.len());
        assert_eq!(nodes.by_path.len(), nodes.ordered_paths.len());
        for path in &nodes.ordered_paths {
            assert!(nodes.by_path.contains_key(path));
        }
        for (path, inode) in &nodes.by_path {
            assert_eq!(nodes.by_inode.get(inode), Some(path));
        }
    }

    #[test]
    fn directory_cookie_survives_partial_reads_deletion_and_snapshot_restore() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(lower.join(name), name).unwrap();
        }
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
        assert_eq!(
            fs.nodes.lock().unwrap().by_inode.len(),
            1,
            "OPENDIR must not allocate every child inode"
        );
        let mut first = Vec::new();
        fs.readdir(ctx, fuse::ROOT_ID, handle, 4096, 0, |entry| {
            if !first.is_empty() {
                return Ok(0);
            }
            first.push((entry.name.to_vec(), entry.offset));
            Ok(1)
        })
        .unwrap();
        assert_eq!(first, vec![(b"a".to_vec(), 1)]);
        fs.unlink(ctx, fuse::ROOT_ID, c"b").unwrap();
        std::fs::write(lower.join("d"), b"new entry after OPENDIR").unwrap();
        let snapshot = fs.capture_state().unwrap();
        let restored =
            OverlayFs::open_existing(fs.snapshot_config.clone(), Arc::new(InodeAllocator::new()))
                .unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored.restore_state(&snapshot).unwrap();
        let mut remaining = Vec::new();
        restored
            .readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 1, |entry, attr| {
                assert_eq!(attr.attr.st_size, 1);
                remaining.push((entry.name.to_vec(), entry.offset));
                Ok(1)
            })
            .unwrap();
        assert_eq!(remaining, vec![(b"c".to_vec(), 3)]);
        restored.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
        assert_eq!(
            restored
                .readdir(ctx, fuse::ROOT_ID, handle, 4096, 0, |_| Ok(1))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[test]
    fn plain_directory_types_preserve_layer_priority_without_child_inode_allocation() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("a"), b"file").unwrap();
        std::os::unix::fs::symlink("a", lower.join("link")).unwrap();
        let fs = parent_cache_fixture(temp.path());
        std::fs::create_dir(fs.core.upper().join("a")).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
        assert_eq!(fs.nodes.lock().unwrap().by_inode.len(), 1);
        // Plain READDIR retains the type snapshot. READDIRPLUS must still
        // resolve the changed object, rather than treating types as metadata.
        std::fs::remove_dir(fs.core.upper().join("a")).unwrap();
        let mut plain = Vec::new();
        fs.readdir(ctx, fuse::ROOT_ID, handle, 4096, 0, |entry| {
            plain.push((entry.name.to_vec(), entry.type_));
            Ok(1)
        })
        .unwrap();
        assert_eq!(
            plain,
            vec![
                (b"a".to_vec(), libc::DT_DIR as u32),
                (b"link".to_vec(), libc::DT_LNK as u32)
            ]
        );
        let mut plus = Vec::new();
        fs.readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 0, |entry, attr| {
            plus.push((
                entry.name.to_vec(),
                entry.type_,
                attr.attr.st_mode & libc::S_IFMT,
            ));
            Ok(1)
        })
        .unwrap();
        assert_eq!(
            plus,
            vec![
                (b"a".to_vec(), libc::DT_REG as u32, libc::S_IFREG),
                (b"link".to_vec(), libc::DT_LNK as u32, libc::S_IFLNK)
            ]
        );
        let mut snapshot = fs.capture_state().unwrap();
        let super::super::snapshot::FsSnapshot::Overlay(state) = &mut snapshot else {
            panic!("overlay snapshot");
        };
        let (_, Handle::DirectoryNames { types, .. }) = state
            .handles
            .iter_mut()
            .find(|(id, _)| *id == handle)
            .unwrap()
        else {
            panic!("lazy directory handle");
        };
        *types = Arc::new(vec![libc::DT_REG as u32]);
        assert!(
            fs.restore_state(&snapshot).is_err(),
            "reject misaligned type snapshots"
        );
    }

    #[test]
    fn readdirplus_preserves_fresh_attributes_offsets_and_alias_denials() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("a"), b"old").unwrap();
        std::fs::write(lower.join("b"), b"data").unwrap();
        std::fs::write(lower.join("private/secret"), b"secret").unwrap();
        let fs = parent_cache_fixture(temp.path());
        fs.init(FsOptions::DO_READDIRPLUS | FsOptions::READDIRPLUS_AUTO)
            .unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let handle = fs.opendir(ctx, fuse::ROOT_ID, 0).unwrap().0.unwrap();
        std::fs::write(lower.join("a"), b"changed").unwrap();
        let mut accepted = Vec::new();
        fs.readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 0, |dir, entry| {
            if !accepted.is_empty() {
                return Ok(0);
            }
            assert_eq!(dir.ino, entry.inode);
            accepted.push((dir.name.to_vec(), dir.offset, entry.attr.st_size));
            Ok(1)
        })
        .unwrap();
        assert_eq!(accepted, vec![(b"a".to_vec(), 1, 7)]);
        let mut remaining = Vec::new();
        fs.readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 1, |dir, entry| {
            remaining.push((dir.name.to_vec(), dir.offset, entry.attr.st_size));
            Ok(1)
        })
        .unwrap();
        assert_eq!(remaining, vec![(b"b".to_vec(), 2, 4)]);
        std::fs::remove_file(lower.join("b")).unwrap();
        std::fs::hard_link(lower.join("private/secret"), lower.join("b")).unwrap();
        let error = fs
            .readdirplus(ctx, fuse::ROOT_ID, handle, 4096, 1, |_, _| {
                panic!("denied alias must not return attributes")
            })
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        fs.releasedir(ctx, fuse::ROOT_ID, 0, handle).unwrap();
        fs.destroy();
    }

    #[test]
    fn observed_backing_follows_live_parent_replacement_and_rejects_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("allowed")).unwrap();
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("private/secret"), b"secret").unwrap();
        std::os::unix::fs::symlink("old", lower.join("allowed/link")).unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let parent = fs.lookup(ctx, fuse::ROOT_ID, c"allowed").unwrap();
        let link = fs.lookup(ctx, parent.inode, c"link").unwrap();
        assert_eq!(fs.readlink(ctx, link.inode).unwrap(), b"old");
        std::fs::rename(lower.join("allowed"), lower.join("old-allowed")).unwrap();
        std::fs::create_dir(lower.join("allowed")).unwrap();
        std::os::unix::fs::symlink("replacement", lower.join("allowed/link")).unwrap();
        assert_eq!(fs.readlink(ctx, link.inode).unwrap(), b"replacement");
        std::fs::remove_file(lower.join("allowed/link")).unwrap();
        std::fs::hard_link(lower.join("private/secret"), lower.join("allowed/link")).unwrap();
        assert_eq!(
            fs.getxattr(ctx, link.inode, c"user.test", 0)
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert_eq!(
            fs.listxattr(ctx, link.inode, 0)
                .err()
                .unwrap()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        fs.destroy();
    }

    #[test]
    fn deepest_cached_parent_uses_fresh_ancestors_and_file_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("allowed/a/b")).unwrap();
        std::fs::write(lower.join("allowed/a/b/file"), b"old").unwrap();
        let mut fs = parent_cache_fixture(temp.path());
        fs.profile = pvisor_overlay_core::profile::Profile::enabled("deep-cache-test");
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let path = Path::new("allowed/a/b/file");
        let inode = fs.allocate_inode(path.to_path_buf());
        assert_eq!(fs.entry(ctx, path, inode).unwrap().attr.st_size, 3);
        std::fs::write(lower.join(path), b"fresh attributes").unwrap();
        assert_eq!(fs.entry(ctx, path, inode).unwrap().attr.st_size, 16);
        let report = fs.profile.report().unwrap();
        assert_eq!(report.measurements["directory_cache_deep_hits"].units, 1);
        // Replace ancestors while retaining the deepest directory's identity.
        // The cached inode remains useful only after a fresh complete Core walk.
        std::fs::rename(lower.join("allowed"), lower.join("old-allowed")).unwrap();
        std::fs::create_dir_all(lower.join("allowed/a")).unwrap();
        std::fs::rename(lower.join("old-allowed/a/b"), lower.join("allowed/a/b")).unwrap();
        assert_eq!(fs.entry(ctx, path, inode).unwrap().attr.st_size, 16);
        std::fs::rename(lower.join("allowed/a"), lower.join("moved-a")).unwrap();
        std::os::unix::fs::symlink(lower.join("moved-a"), lower.join("allowed/a")).unwrap();
        assert!(fs.entry(ctx, path, inode).is_err());
        assert_eq!(
            fs.profile.report().unwrap().measurements["directory_cache_deep_hits"].units,
            2
        );
        fs.destroy();
    }

    #[test]
    fn cached_parents_follow_replacement_and_do_not_authorize_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("allowed")).unwrap();
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("allowed/file"), b"old").unwrap();
        std::fs::write(lower.join("private/secret"), b"secret").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let parent = fs.lookup(ctx, fuse::ROOT_ID, c"allowed").unwrap();
        assert_eq!(
            fs.lookup(ctx, parent.inode, c"file").unwrap().attr.st_size,
            3
        );
        assert_eq!(fs.directory_cache.lock().unwrap().entries.len(), 1);
        std::fs::rename(lower.join("allowed"), lower.join("old-allowed")).unwrap();
        std::fs::create_dir(lower.join("allowed")).unwrap();
        std::fs::write(lower.join("allowed/file"), b"replacement").unwrap();
        let file = fs.lookup(ctx, parent.inode, c"file").unwrap();
        assert_eq!(file.attr.st_size, 11);
        std::fs::write(lower.join("allowed/file"), b"live attributes").unwrap();
        assert_eq!(fs.getattr(ctx, file.inode, None).unwrap().0.st_size, 15);
        std::fs::remove_file(lower.join("allowed/file")).unwrap();
        std::fs::hard_link(lower.join("private/secret"), lower.join("allowed/file")).unwrap();
        assert_eq!(
            fs.lookup(ctx, parent.inode, c"file")
                .err()
                .expect("excluded hard-link alias must be denied")
                .raw_os_error(),
            Some(libc::EACCES)
        );
        std::fs::rename(lower.join("allowed"), lower.join("replaced-allowed")).unwrap();
        std::os::unix::fs::symlink(lower.join("private"), lower.join("allowed")).unwrap();
        assert!(fs.lookup(ctx, parent.inode, c"secret").is_err());
    }

    #[test]
    fn larger_directory_capacity_retains_a_reused_working_set_without_caching_attributes() {
        // The same 288-parent demand exceeds the default but fits the candidate.
        // Check actual native-lookup reuse and fresh attributes, rather than just
        // asserting the configured field or the cache container's length.
        for capacity in [256, 1024] {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            let working_set = 288;
            for index in 0..working_set {
                let directory = lower.join(format!("d{index}"));
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join("file"), b"data").unwrap();
            }
            let mut fs = parent_cache_fixture(temp.path());
            fs.profile = pvisor_overlay_core::profile::Profile::enabled("directory-capacity");
            fs.configure_directory_cache_capacity(capacity).unwrap();
            let ctx = Context {
                uid: 0,
                gid: 0,
                pid: 1,
            };
            let visit = |fs: &OverlayFs| {
                for index in 0..working_set {
                    let path = PathBuf::from(format!("d{index}/file"));
                    let inode = fs.allocate_inode(path.clone());
                    assert_eq!(fs.entry(ctx, &path, inode).unwrap().attr.st_size, 4);
                }
            };
            visit(&fs);
            let before = fs.profile.report().unwrap();
            visit(&fs);
            let after = fs.profile.report().unwrap();
            let units = |report: &pvisor_overlay_core::profile::ProfileReport, label: &str| {
                report
                    .measurements
                    .get(label)
                    .map_or(0, |value| value.units)
            };
            let repeated_misses =
                units(&after, "directory_cache_misses") - units(&before, "directory_cache_misses");
            let repeated_evictions = units(&after, "directory_cache_evictions")
                - units(&before, "directory_cache_evictions");
            if capacity == 256 {
                assert!(repeated_misses > 0);
                assert!(repeated_evictions > 0);
            } else {
                assert_eq!(repeated_misses, 0);
                assert_eq!(repeated_evictions, 0);
            }
            assert_eq!(units(&after, "directory_cache_capacity"), capacity as u64);
            std::fs::write(lower.join("d287/file"), b"updated contents").unwrap();
            let path = Path::new("d287/file");
            let inode = fs.allocate_inode(path.to_path_buf());
            assert_eq!(fs.entry(ctx, path, inode).unwrap().attr.st_size, 16);
        }
    }

    #[test]
    fn directory_capacity_configuration_is_once_per_owner_and_not_snapshot_state() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("lower")).unwrap();
        let mut fs = parent_cache_fixture(temp.path());
        assert_eq!(
            fs.directory_cache_capacity,
            DEFAULT_DIRECTORY_CACHE_CAPACITY
        );
        for invalid in [0, MAX_DIRECTORY_CACHE_CAPACITY + 1] {
            assert_eq!(
                fs.configure_directory_cache_capacity(invalid)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        fs.configure_directory_cache_capacity(1024).unwrap();
        assert_eq!(
            fs.configure_directory_cache_capacity(256)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let snapshot = fs.capture_state().unwrap();
        let mut restored =
            OverlayFs::open_existing(fs.snapshot_config.clone(), Arc::new(InodeAllocator::new()))
                .unwrap();
        assert_eq!(
            restored.directory_cache_capacity,
            DEFAULT_DIRECTORY_CACHE_CAPACITY
        );
        restored.configure_directory_cache_capacity(1).unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored.restore_state(&snapshot).unwrap();
        assert_eq!(restored.directory_cache_capacity, 1);
    }

    #[test]
    fn cached_parents_are_bounded_and_released_before_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for i in 0..DEFAULT_DIRECTORY_CACHE_CAPACITY + 4 {
            let name = format!("d{i}");
            std::fs::create_dir(lower.join(&name)).unwrap();
            std::fs::write(lower.join(&name).join("file"), b"data").unwrap();
            let parent = fs
                .lookup(ctx, fuse::ROOT_ID, &CString::new(name).unwrap())
                .unwrap();
            fs.lookup(ctx, parent.inode, c"file").unwrap();
            assert!(
                fs.directory_cache.lock().unwrap().entries.len() <= fs.directory_cache_capacity
            );
        }
        let snapshot = fs.capture_state().unwrap();
        assert!(fs.directory_cache.lock().unwrap().entries.is_empty());
        let restored =
            OverlayFs::open_existing(fs.snapshot_config.clone(), Arc::new(InodeAllocator::new()))
                .unwrap();
        restored.init(FsOptions::empty()).unwrap();
        restored.restore_state(&snapshot).unwrap();
        let parent = restored.lookup(ctx, fuse::ROOT_ID, c"d0").unwrap();
        assert_eq!(
            restored
                .lookup(ctx, parent.inode, c"file")
                .unwrap()
                .attr
                .st_size,
            4
        );
    }

    #[test]
    fn failed_file_opens_do_not_retain_native_lookup_references() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("lower/dir")).unwrap();
        std::fs::write(temp.path().join("lower/dir/file"), b"data").unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"dir").unwrap();
        for _ in 0..16 {
            assert!(fs
                .open(ctx, entry.inode, false, libc::O_WRONLY as u32)
                .is_err());
        }
        assert!(fs.handles.lock().unwrap().is_empty());
        let super::super::snapshot::FsSnapshot::Overlay(snapshot) = fs.capture_state().unwrap()
        else {
            panic!("expected an overlay snapshot");
        };
        for layer in &snapshot.layers {
            let super::super::snapshot::FsSnapshot::Passthrough(native) = layer else {
                panic!("expected a native layer");
            };
            assert!(native.handles.is_empty());
            assert!(native
                .inodes
                .iter()
                .all(|inode| inode.inode == fuse::ROOT_ID));
        }
    }

    #[test]
    fn overlay_snapshot_preserves_the_unboxed_json_contract() {
        let state = OverlaySnapshot {
            config: Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec!["/lower".into()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: "/upper".into(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths: vec![],
                access_policy: Default::default(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            hard_links: vec![],
            hard_link_origins: vec![],
            layers: vec![],
            nodes: vec![(1, b"/".to_vec())],
            lookup_refs: None,
            legacy_lookup_refs: vec![],
            handles: vec![(
                1,
                Handle::File(Arc::new(FileHandle {
                    overlay_inode: 1,
                    layer: Layer(0),
                    inode: 1,
                    handle: 7,
                    relative_path: b"file".to_vec(),
                })),
            )],
            next_handle: 2,
        };
        let legacy = serde_json::to_value(&state.config).unwrap();
        assert!(legacy.get("lower_mutability").is_none());
        let decoded: Config = serde_json::from_value(legacy).unwrap();
        assert!(decoded.lower_mutability.is_empty());
        let mut declared = decoded;
        declared.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable];
        let roundtrip: Config =
            serde_json::from_value(serde_json::to_value(&declared).unwrap()).unwrap();
        assert_eq!(roundtrip.lower_mutability, declared.lower_mutability);
        // Shared in-memory ownership adds no fields or wrapper to old handles.
        let legacy_handle = serde_json::json!({
            "kind": "File",
            "state": { "overlay_inode": 1, "layer": 0, "inode": 1,
                       "handle": 7, "relative_path": [102, 105, 108, 101] }
        });
        assert_eq!(
            serde_json::to_value(&state.handles[0].1).unwrap(),
            legacy_handle
        );
        let decoded_handle: Handle = serde_json::from_value(legacy_handle.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded_handle).unwrap(), legacy_handle);
        // Box must add no wrapper to the tagged enum's persisted state format,
        // including when nested in layers.
        let old_wire = serde_json::json!({
            "kind": "Overlay",
            "state": serde_json::to_value(&state).unwrap(),
        });
        let snapshot = super::super::snapshot::FsSnapshot::Overlay(Box::new(state));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), old_wire);
        let restored: super::super::snapshot::FsSnapshot =
            serde_json::from_value(old_wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), old_wire);
    }

    #[test]
    fn access_policy_reaches_virtiofs_lookup_and_open() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("private.key"), b"dummy-private").unwrap();
        std::fs::write(lower.join(".env"), b"warn-only").unwrap();
        std::os::unix::fs::symlink("private.key", lower.join("alias")).unwrap();
        let fs = OverlayFs::new(
            Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec![lower.to_string_lossy().into_owned()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: temp.path().join("upper").to_string_lossy().into_owned(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths: vec![],
                access_policy: pvisor_overlay_core::FileAccessPolicy::new(
                    vec!["private.key".into()],
                    vec![".env".into()],
                )
                .unwrap(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        assert!(fs
            .lookup(ctx, fuse::ROOT_ID, &CString::new("private.key").unwrap())
            .is_err());
        let alias = fs
            .lookup(ctx, fuse::ROOT_ID, &CString::new("alias").unwrap())
            .unwrap();
        assert!(fs
            .open(ctx, alias.inode, false, libc::O_RDONLY as u32)
            .is_err());
        let env = fs
            .lookup(ctx, fuse::ROOT_ID, &CString::new(".env").unwrap())
            .unwrap();
        assert!(fs
            .open(ctx, env.inode, false, libc::O_RDONLY as u32)
            .is_ok());
    }

    fn exclusive_upper_config(root: &Path) -> Config {
        let fixture = parent_cache_fixture(root);
        let mut config = fixture.snapshot_config.clone();
        config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable];
        config.excluded_paths.clear();
        config.access_policy = Default::default();
        config
    }

    fn exclusive_upper_owner(config: Config, preference: Option<(bool, bool)>) -> OverlayFs {
        let mut fs = OverlayFs::build_with_writeback(
            config,
            Arc::new(InodeAllocator::new()),
            false,
            Some("0"),
        )
        .unwrap();
        fs.core = fs
            .core
            .with_profile(pvisor_overlay_core::profile::Profile::enabled(
                "upper-epoch",
            ));
        if let Some((requested, guest_cache)) = preference {
            fs.configure_exclusive_upper_absence_cache(requested, guest_cache)
                .unwrap();
        }
        fs.init(FsOptions::empty()).unwrap();
        fs
    }

    fn upper_absence_units(fs: &OverlayFs, label: &str) -> u64 {
        fs.core
            .profile_report()
            .unwrap()
            .measurements
            .get(label)
            .map_or(0, |measurement| measurement.units)
    }

    #[test]
    fn exclusive_upper_absence_requires_vm_ownership_and_complete_permissions() {
        use pvisor_overlay_core::LayerMutability::{Immutable, Mutable};
        for (mutability, semantics, preference, expected) in [
            (
                vec![Immutable],
                passthrough::PermissionSemantics::LinuxComplete,
                None,
                false,
            ),
            (
                vec![Immutable],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((false, true)),
                false,
            ),
            (
                vec![Immutable],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((true, false)),
                false,
            ),
            (
                vec![],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((true, true)),
                false,
            ),
            (
                vec![Mutable],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((true, true)),
                false,
            ),
            (
                vec![Mutable, Immutable],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((true, true)),
                false,
            ),
            (
                vec![Immutable],
                passthrough::PermissionSemantics::LinuxSimplified,
                Some((true, true)),
                false,
            ),
            (
                vec![Immutable],
                passthrough::PermissionSemantics::LinuxComplete,
                Some((true, true)),
                true,
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir(temp.path().join("lower")).unwrap();
            std::fs::write(temp.path().join("lower/file"), b"lower").unwrap();
            let mut config = exclusive_upper_config(temp.path());
            if mutability.len() == 2 {
                let other = temp.path().join("other");
                std::fs::create_dir(&other).unwrap();
                config
                    .lower_dirs
                    .insert(0, other.to_string_lossy().into_owned());
            }
            config.lower_mutability = mutability;
            config.semantics = semantics;
            let fs = exclusive_upper_owner(config, preference);
            assert_eq!(fs.core.exclusive_upper_absence_cache_enabled(), expected);
            let ctx = Context {
                uid: 0,
                gid: 0,
                pid: 1,
            };
            let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
            assert_eq!(entry.attr.st_size, 5);
            assert_eq!(entry.entry_timeout, TTL);
            if semantics == passthrough::PermissionSemantics::LinuxComplete {
                assert_eq!(entry.attr_timeout, TTL);
            }
        }
    }

    #[test]
    fn exclusive_upper_write_epoch_bypasses_cache_and_clears_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("lower/directory")).unwrap();
        std::fs::write(temp.path().join("lower/directory/file"), b"lower").unwrap();
        let fs = exclusive_upper_owner(exclusive_upper_config(temp.path()), Some((true, true)));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let directory = fs.lookup(ctx, fuse::ROOT_ID, c"directory").unwrap();
        for _ in 0..2 {
            assert_eq!(
                fs.lookup(ctx, directory.inode, c"file")
                    .unwrap()
                    .attr
                    .st_size,
                5
            );
        }
        let hits = upper_absence_units(&fs, "exclusive_upper_absence_cache_hits");
        assert!(hits > 0);
        assert!(
            upper_absence_units(&fs, "exclusive_upper_absence_cache_hits_unavailable_parent") > 0
        );
        let failed = (|| -> io::Result<()> {
            let _operation = fs.write_operation()?;
            for _ in 0..2 {
                assert_eq!(fs.core.metadata(Path::new("directory/file"))?.len(), 5);
            }
            assert_eq!(
                upper_absence_units(&fs, "exclusive_upper_absence_cache_hits"),
                hits
            );
            // Model a native upper publication followed by an operation error.
            // Its earlier missing result must not survive either side of Drop.
            std::fs::create_dir(temp.path().join("upper/directory"))?;
            std::fs::write(temp.path().join("upper/directory/file"), b"new upper")?;
            assert_eq!(fs.core.metadata(Path::new("directory/file"))?.len(), 9);
            Err(io::Error::from_raw_os_error(libc::EIO))
        })();
        assert_eq!(failed.unwrap_err().raw_os_error(), Some(libc::EIO));
        let entry = fs.lookup(ctx, directory.inode, c"file").unwrap();
        assert_eq!(entry.attr.st_size, 9);
        assert_eq!(entry.attr_timeout, TTL);
        assert_eq!(entry.entry_timeout, TTL);
    }

    #[test]
    fn exclusive_upper_absence_keeps_policy_and_hardlink_denials() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("file"), b"allowed").unwrap();
        std::fs::write(lower.join("private/key"), b"secret").unwrap();
        std::fs::write(lower.join("linked"), b"shared").unwrap();
        std::fs::hard_link(lower.join("linked"), lower.join("alias")).unwrap();
        let mut config = exclusive_upper_config(temp.path());
        config.access_policy = pvisor_overlay_core::FileAccessPolicy::new(
            vec!["private".into(), "private/**".into()],
            vec![],
        )
        .unwrap();
        let fs = exclusive_upper_owner(config, Some((true, true)));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for _ in 0..2 {
            let allowed = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
            assert_eq!((allowed.attr.st_size, allowed.attr_timeout), (7, TTL));
            for name in [c"private", c"linked", c"alias"] {
                assert_eq!(
                    fs.lookup(ctx, fuse::ROOT_ID, name)
                        .err()
                        .expect("policy must deny this name")
                        .raw_os_error(),
                    Some(libc::EACCES)
                );
            }
            let denied = fs.allocate_inode(PathBuf::from("private/key"));
            assert!(fs.getattr(ctx, denied, None).is_err());
            assert!(fs.open(ctx, denied, false, libc::O_RDONLY as u32).is_err());
        }
        assert!(upper_absence_units(&fs, "exclusive_upper_absence_cache_hits") > 0);
    }

    #[test]
    fn exclusive_upper_absence_preserves_copyup_native_creation_and_alias_checks() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"lower").unwrap();
        let fs = exclusive_upper_owner(exclusive_upper_config(temp.path()), Some((true, true)));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let file = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(fs.getattr(ctx, file.inode, None).unwrap().0.st_size, 5);
        let old = fs
            .open(ctx, file.inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let writable = fs
            .open(ctx, file.inode, false, libc::O_RDWR as u32)
            .unwrap()
            .0
            .unwrap();
        fs.write(
            ctx,
            file.inode,
            writable,
            WriteReader(io::Cursor::new(b"changed upper".to_vec())),
            13,
            0,
            None,
            false,
            false,
            libc::O_RDWR as u32,
        )
        .unwrap();
        assert_eq!(fs.getattr(ctx, file.inode, None).unwrap().0.st_size, 13);
        let mut output = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        fs.read(ctx, file.inode, old, &mut output, 64, 0, None, 0)
            .unwrap();
        assert_eq!(output.bytes, b"lower");
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"newdir").is_err());
        let directory = fs
            .mkdir(
                ctx,
                fuse::ROOT_ID,
                c"newdir",
                0o755,
                0,
                Extensions::default(),
            )
            .unwrap();
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"newdir").unwrap().inode,
            directory.inode
        );
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"newlink").is_err());
        let link = fs
            .symlink(
                ctx,
                c"file",
                fuse::ROOT_ID,
                c"newlink",
                Extensions::default(),
            )
            .unwrap();
        assert_eq!(fs.readlink(ctx, link.inode).unwrap(), b"file");
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"newlink").unwrap().inode,
            link.inode
        );
        assert!(fs
            .symlink(
                ctx,
                c"file",
                fuse::ROOT_ID,
                c"newlink",
                Extensions::default()
            )
            .is_err());
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"newlink").unwrap().inode,
            link.inode
        );
        let alias = fs.link(ctx, file.inode, fuse::ROOT_ID, c"alias").unwrap();
        assert_eq!(alias.attr.st_nlink, 2);
        assert_eq!(fs.getattr(ctx, file.inode, None).unwrap().0.st_nlink, 2);
        fs.rename(ctx, fuse::ROOT_ID, c"alias", fuse::ROOT_ID, c"renamed", 0)
            .unwrap();
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"alias").is_err());
        assert_eq!(
            fs.lookup(ctx, fuse::ROOT_ID, c"renamed")
                .unwrap()
                .attr
                .st_nlink,
            2
        );
        fs.unlink(ctx, fuse::ROOT_ID, c"renamed").unwrap();
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"renamed").is_err());
        assert_eq!(fs.getattr(ctx, file.inode, None).unwrap().0.st_nlink, 1);
        for handle in [old, writable] {
            fs.release(ctx, file.inode, 0, handle, false, false, None)
                .unwrap();
        }
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"lower");
    }

    #[test]
    fn exclusive_upper_epoch_clears_at_capture_restore_and_teardown_without_persistence() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("lower")).unwrap();
        std::fs::write(temp.path().join("lower/file"), b"lower").unwrap();
        let config = exclusive_upper_config(temp.path());
        let fs = exclusive_upper_owner(config.clone(), Some((true, true)));
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let file = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        let hits = upper_absence_units(&fs, "exclusive_upper_absence_cache_hits");
        assert!(hits > 0);
        let saved = fs.capture_state().unwrap();
        let encoded = serde_json::to_vec(&saved).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("exclusive_upper_absence_cache"));
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(
            upper_absence_units(&fs, "exclusive_upper_absence_cache_hits"),
            hits
        );
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert!(upper_absence_units(&fs, "exclusive_upper_absence_cache_hits") > hits);
        for enabled in [false, true] {
            let mut restored = OverlayFs::build_with_writeback(
                config.clone(),
                Arc::new(InodeAllocator::new()),
                true,
                Some("0"),
            )
            .unwrap();
            restored
                .configure_exclusive_upper_absence_cache(enabled, true)
                .unwrap();
            restored.restore_state(&saved).unwrap();
            assert_eq!(
                restored.core.exclusive_upper_absence_cache_enabled(),
                enabled
            );
            assert_eq!(
                restored.getattr(ctx, file.inode, None).unwrap().0.st_size,
                5
            );
            assert_eq!(restored.getattr(ctx, file.inode, None).unwrap().1, TTL);
        }
        let invalidations = upper_absence_units(&fs, "exclusive_upper_absence_cache_invalidations");
        fs.destroy();
        assert!(
            upper_absence_units(&fs, "exclusive_upper_absence_cache_invalidations") > invalidations
        );
        fs.init(FsOptions::empty()).unwrap();
        let hits = upper_absence_units(&fs, "exclusive_upper_absence_cache_hits");
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(
            upper_absence_units(&fs, "exclusive_upper_absence_cache_hits"),
            hits
        );
    }

    #[test]
    fn immutable_data_cache_preserves_open_checks_and_invalidates_copy_up() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(lower.join("private")).unwrap();
        std::fs::write(lower.join("file"), b"lower").unwrap();
        std::fs::write(lower.join("private/key"), b"secret").unwrap();
        let fixture = parent_cache_fixture(temp.path());
        let super::super::snapshot::FsSnapshot::Overlay(state) = fixture.capture_state().unwrap()
        else {
            panic!("overlay expected")
        };
        let mut config = state.config;
        config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable];
        let mut fs = OverlayFs::new(config, Arc::new(InodeAllocator::new())).unwrap();
        fs.configure_immutable_data_cache(true);
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(entry.entry_timeout, TTL);
        assert_eq!(entry.attr_timeout, TTL);
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"private").is_err());
        let denied = fs.allocate_inode(PathBuf::from("private/key"));
        assert!(fs.open(ctx, denied, false, libc::O_RDONLY as u32).is_err());
        let (old, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(options.contains(OpenOptions::KEEP_CACHE));
        let old = old.unwrap();
        fs.configure_immutable_data_cache(false);
        let (uncached, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::KEEP_CACHE));
        fs.release(ctx, entry.inode, 0, uncached.unwrap(), false, false, None)
            .unwrap();
        fs.configure_immutable_data_cache(true);
        let (writable, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDWR as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::KEEP_CACHE));
        let writable = writable.unwrap();
        fs.write(
            ctx,
            entry.inode,
            writable,
            WriteReader(io::Cursor::new(b"upper".to_vec())),
            5,
            0,
            None,
            false,
            false,
            libc::O_RDWR as u32,
        )
        .unwrap();
        let (new, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::KEEP_CACHE));
        for (handle, expected) in [(old, b"lower"), (new.unwrap(), b"upper")] {
            let mut writer = ReadWriter {
                gate: None,
                bytes: Vec::new(),
            };
            fs.read(ctx, entry.inode, handle, &mut writer, 5, 0, None, 0)
                .unwrap();
            assert_eq!(&writer.bytes, expected);
            fs.release(ctx, entry.inode, 0, handle, false, false, None)
                .unwrap();
        }
        fs.release(ctx, entry.inode, 0, writable, false, false, None)
            .unwrap();
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"lower");
        fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"file").is_err());
    }

    #[test]
    fn data_cache_requires_the_selected_physical_lower_to_be_immutable() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let mutable = temp.path().join("mutable");
        std::fs::create_dir(&lower).unwrap();
        std::fs::create_dir(&mutable).unwrap();
        std::fs::write(lower.join("stable"), b"stable").unwrap();
        std::fs::write(mutable.join("live"), b"before").unwrap();
        let fixture = parent_cache_fixture(temp.path());
        let super::super::snapshot::FsSnapshot::Overlay(state) = fixture.capture_state().unwrap()
        else {
            panic!("overlay expected")
        };
        let mut config = state.config;
        config
            .lower_dirs
            .insert(0, mutable.to_string_lossy().into_owned());
        config.lower_mutability = vec![
            pvisor_overlay_core::LayerMutability::Mutable,
            pvisor_overlay_core::LayerMutability::Immutable,
        ];
        let mut fs = OverlayFs::new(config, Arc::new(InodeAllocator::new())).unwrap();
        fs.configure_immutable_data_cache(true);
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        for (name, cached) in [(c"live", false), (c"stable", true)] {
            let entry = fs.lookup(ctx, fuse::ROOT_ID, name).unwrap();
            let (handle, options) = fs
                .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
                .unwrap();
            assert_eq!(options.contains(OpenOptions::KEEP_CACHE), cached);
            fs.release(ctx, entry.inode, 0, handle.unwrap(), false, false, None)
                .unwrap();
        }
        std::fs::write(mutable.join("live"), b"after!").unwrap();
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"live").unwrap();
        let (handle, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(!options.contains(OpenOptions::KEEP_CACHE));
        let mut writer = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        fs.read(
            ctx,
            entry.inode,
            handle.unwrap(),
            &mut writer,
            6,
            0,
            None,
            0,
        )
        .unwrap();
        assert_eq!(writer.bytes, b"after!");
        fs.release(ctx, entry.inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
    }

    #[test]
    fn immutable_data_cache_restores_backing_without_persisting_a_new_cache_object() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"stable").unwrap();
        let fixture = parent_cache_fixture(temp.path());
        let super::super::snapshot::FsSnapshot::Overlay(state) = fixture.capture_state().unwrap()
        else {
            panic!("overlay expected")
        };
        let mut config = state.config;
        config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable];
        let mut fs = OverlayFs::new(config.clone(), Arc::new(InodeAllocator::new())).unwrap();
        fs.configure_immutable_data_cache(true);
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        let (handle, options) = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(options.contains(OpenOptions::KEEP_CACHE));
        let saved = fs.capture_state().unwrap();
        let encoded = serde_json::to_vec(&saved).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("immutable_data_cache"));
        let saved = serde_json::from_slice(&encoded).unwrap();
        let mut restored =
            OverlayFs::open_existing(config, Arc::new(InodeAllocator::new())).unwrap();
        restored.configure_immutable_data_cache(true);
        restored.restore_state(&saved).unwrap();
        let mut writer = ReadWriter {
            gate: None,
            bytes: Vec::new(),
        };
        restored
            .read(
                ctx,
                entry.inode,
                handle.unwrap(),
                &mut writer,
                6,
                0,
                None,
                0,
            )
            .unwrap();
        assert_eq!(writer.bytes, b"stable");
        restored
            .release(ctx, entry.inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
        let (handle, options) = restored
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap();
        assert!(options.contains(OpenOptions::KEEP_CACHE));
        restored
            .release(ctx, entry.inode, 0, handle.unwrap(), false, false, None)
            .unwrap();
    }

    #[test]
    fn explicit_immutable_lower_reaches_core_without_caching_merged_whiteouts() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), b"stable").unwrap();
        let fixture = parent_cache_fixture(temp.path());
        let super::super::snapshot::FsSnapshot::Overlay(state) = fixture.capture_state().unwrap()
        else {
            panic!("overlay expected")
        };
        let mut config = state.config;
        config.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable];
        let mut fs = OverlayFs::new(config, Arc::new(InodeAllocator::new())).unwrap();
        let profile = pvisor_overlay_core::profile::Profile::enabled("immutable-adapter");
        fs.core = fs.core.with_profile(profile.clone());
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert!(profile.report().unwrap().measurements["immutable_lower_cache_hits"].units > 0);
        fs.unlink(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"file").is_err());
        assert_eq!(std::fs::read(lower.join("file")).unwrap(), b"stable");
        assert!(temp.path().join("upper/.wh.file").is_file());
    }

    #[test]
    fn mutations_land_in_upper_and_lower_stays_immutable() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&lower).unwrap();
        std::fs::write(lower.join("original"), b"lower").unwrap();
        let fs = OverlayFs::new(
            Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec![lower.to_string_lossy().into_owned()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: upper.to_string_lossy().into_owned(),
                work_dir: Some(work.to_string_lossy().into_owned()),
                preimage_dir: None,
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        // Linux ENOTEMPTY is 39; macOS 66 would become EREMOTE in the guest,
        // preventing dpkg from falling back to recursive directory cleanup.
        std::fs::create_dir(lower.join("directory")).unwrap();
        std::fs::write(lower.join("directory/file"), b"data").unwrap();
        for copy_up in [false, true] {
            if copy_up {
                fs.core.copy_up(Path::new("directory")).unwrap();
            }
            assert_eq!(
                fs.rmdir(ctx, fuse::ROOT_ID, c"directory")
                    .unwrap_err()
                    .raw_os_error(),
                Some(39),
            );
        }
        let directory = fs.lookup(ctx, fuse::ROOT_ID, c"directory").unwrap();
        fs.unlink(ctx, directory.inode, c"file").unwrap();
        fs.rmdir(ctx, fuse::ROOT_ID, c"directory").unwrap();
        assert!(fs.lookup(ctx, fuse::ROOT_ID, c"directory").is_err());

        let original = CString::new("original").unwrap();
        fs.lookup(ctx, fuse::ROOT_ID, &original).unwrap();
        fs.unlink(ctx, fuse::ROOT_ID, &original).unwrap();
        assert_eq!(std::fs::read(lower.join("original")).unwrap(), b"lower");
        assert!(upper.join(".wh.original").is_file());

        // mode_t is u16 on macOS and u32 on Linux.
        #[allow(clippy::unnecessary_cast)]
        let regular_file_mode = libc::S_IFREG as u32;
        let created = CString::new("created").unwrap();
        let (entry, handle, _) = fs
            .create(
                ctx,
                fuse::ROOT_ID,
                &created,
                regular_file_mode | 0o640,
                false,
                libc::O_RDWR as u32,
                0,
                Extensions::default(),
            )
            .unwrap();
        fs.release(
            ctx,
            entry.inode,
            libc::O_RDWR as u32,
            handle.unwrap(),
            false,
            false,
            None,
        )
        .unwrap();
        assert!(upper.join("created").is_file());
        assert!(!lower.join("created").exists());
    }

    #[test]
    fn getattr_keeps_open_file_identity_after_unlink_or_replacement() {
        for rename in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            let upper = temp.path().join("upper");
            std::fs::create_dir(&lower).unwrap();
            let fs = OverlayFs::new(
                Config {
                    lower_mutability: Vec::new(),
                    lower_dirs: vec![lower.to_string_lossy().into_owned()],
                    apply_target: None,
                    baseline_lower: None,
                    baseline_content_index: None,
                    upper_dir: upper.to_string_lossy().into_owned(),
                    work_dir: None,
                    preimage_dir: None,
                    excluded_paths: vec![],
                    access_policy: Default::default(),
                    semantics: passthrough::PermissionSemantics::LinuxComplete,
                },
                Arc::new(InodeAllocator::new()),
            )
            .unwrap();
            fs.init(FsOptions::empty()).unwrap();
            let ctx = Context {
                uid: 0,
                gid: 0,
                pid: 1,
            };
            // mode_t is u16 on macOS and u32 on Linux.
            #[allow(clippy::unnecessary_cast)]
            let regular_file_mode = libc::S_IFREG as u32;
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"temporary",
                    regular_file_mode | 0o600,
                    false,
                    libc::O_RDWR as u32,
                    0,
                    Extensions::default(),
                )
                .unwrap();
            std::fs::write(upper.join("temporary"), b"original").unwrap();
            assert_eq!(fs.getattr(ctx, entry.inode, handle).unwrap().0.st_size, 8);
            if rename {
                std::fs::write(upper.join("replacement"), b"new").unwrap();
                fs.rename(
                    ctx,
                    fuse::ROOT_ID,
                    c"replacement",
                    fuse::ROOT_ID,
                    c"temporary",
                    0,
                )
                .unwrap();
            } else {
                fs.unlink(ctx, fuse::ROOT_ID, c"temporary").unwrap();
                assert!(fs.lookup(ctx, fuse::ROOT_ID, c"temporary").is_err());
            }
            for fh in [handle, None] {
                let (attr, _) = fs.getattr(ctx, entry.inode, fh).unwrap();
                assert_eq!(attr.st_size, 8);
                assert_eq!(attr.st_ino, entry.inode);
                assert_eq!(attr.st_nlink, 0);
            }
            if !rename {
                std::fs::write(upper.join("temporary"), b"new").unwrap();
            }
            let replacement = fs.lookup(ctx, fuse::ROOT_ID, c"temporary").unwrap();
            assert_ne!(replacement.inode, entry.inode);
            assert_eq!(replacement.attr.st_size, 3);
            assert_eq!(fs.getattr(ctx, entry.inode, None).unwrap().0.st_size, 8);
            assert_eq!(
                fs.getattr(ctx, replacement.inode, handle)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EBADF)
            );
            fs.release(
                ctx,
                entry.inode,
                libc::O_RDWR as u32,
                handle.unwrap(),
                false,
                false,
                None,
            )
            .unwrap();
            assert!(fs.getattr(ctx, entry.inode, handle).is_err());
        }
    }

    #[test]
    fn shares_inode_allocator_with_virtual_entries() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        std::fs::create_dir_all(&lower).unwrap();
        std::fs::write(lower.join("real"), b"data").unwrap();
        let inode_alloc = Arc::new(InodeAllocator::new());
        let fs = OverlayFs::new(
            Config {
                lower_mutability: Vec::new(),
                lower_dirs: vec![lower.to_string_lossy().into_owned()],
                apply_target: None,
                baseline_lower: None,
                baseline_content_index: None,
                upper_dir: upper.to_string_lossy().into_owned(),
                work_dir: None,
                preimage_dir: None,
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            inode_alloc.clone(),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();

        // AugmentFs registers /init.krun after constructing its inner
        // filesystem. Simulate that allocation and verify the first real
        // lookup cannot reuse the virtual inode number.
        let virtual_inode = inode_alloc.next();
        let entry = fs
            .lookup(
                Context {
                    uid: 0,
                    gid: 0,
                    pid: 1,
                },
                fuse::ROOT_ID,
                c"real",
            )
            .unwrap();
        assert_ne!(entry.inode, virtual_inode);
    }
    #[test]
    fn direct_lower_reads_virtual_bytes_and_keeps_guest_metadata_through_copy_up() {
        use pvisor_overlay_core::backend::{
            BackendAttachment, FileAttr, FileType, ReadOnlyBackend,
        };
        use std::os::unix::fs::PermissionsExt;
        struct Image {
            root: PathBuf,
        }
        impl ReadOnlyBackend for Image {
            fn prepare_metadata(&self, relative: &Path) -> io::Result<()> {
                std::fs::symlink_metadata(self.root.join(relative)).map(|_| ())
            }
            fn prepare_directory(&self, relative: &Path) -> io::Result<()> {
                self.prepare_metadata(relative)
            }
            fn attributes(&self, relative: &Path) -> io::Result<FileAttr> {
                let metadata = std::fs::symlink_metadata(self.root.join(relative))?;
                let time = std::time::UNIX_EPOCH + Duration::from_secs(1000);
                Ok(FileAttr {
                    ino: metadata.ino(),
                    size: metadata.len(),
                    blocks: metadata.len().div_ceil(512),
                    atime: time,
                    mtime: time,
                    ctime: time,
                    crtime: time,
                    kind: if metadata.is_dir() {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    },
                    perm: 0o755,
                    uid: 1234,
                    gid: 2345,
                    nlink: 1,
                    rdev: 0,
                    blksize: 4096,
                    flags: 0,
                })
            }
            fn read_at(&self, _relative: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>> {
                let data = b"real image data";
                let start = (offset as usize).min(data.len());
                Ok(data[start..start.saturating_add(size as usize).min(data.len())].to_vec())
            }
            fn materialize_file(&self, relative: &Path) -> io::Result<()> {
                if !relative.as_os_str().is_empty() {
                    std::fs::write(self.root.join(relative), b"real image data")?;
                }
                Ok(())
            }
            fn materialize_tree(&self, _relative: &Path) -> io::Result<()> {
                self.materialize_file(Path::new("file"))
            }
        }
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let lower = temp.path().join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("file"), [0; 15]).unwrap();
        std::fs::set_permissions(lower.join("file"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let _attachment = BackendAttachment::new(
            &lower,
            Arc::new(Image {
                root: lower.clone(),
            }),
        )
        .unwrap();
        let fs = parent_cache_fixture(temp.path());
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let entry = fs.lookup(ctx, fuse::ROOT_ID, c"file").unwrap();
        assert_eq!(entry.attr.st_uid, 1234);
        assert_eq!(entry.attr.st_gid, 2345);
        assert_eq!(entry.attr.st_mode as u32 & 0o7777, 0o755);
        fs.access(ctx, entry.inode, libc::X_OK as u32).unwrap();
        assert!(fs
            .access(
                Context {
                    uid: 3456,
                    gid: 3456,
                    pid: 1
                },
                entry.inode,
                libc::W_OK as u32
            )
            .is_err());
        let handle = fs
            .open(ctx, entry.inode, false, libc::O_RDONLY as u32)
            .unwrap()
            .0
            .unwrap();
        let mut writer = ReadWriter {
            gate: None,
            bytes: vec![],
        };
        assert_eq!(
            fs.read(ctx, entry.inode, handle, &mut writer, 15, 0, None, 0)
                .unwrap(),
            15
        );
        assert_eq!(writer.bytes, b"real image data");
        let (attr, _) = fs.getattr(ctx, entry.inode, Some(handle)).unwrap();
        assert_eq!(attr.st_uid, 1234);
        fs.core.copy_up(Path::new("file")).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("upper/file")).unwrap(),
            b"real image data"
        );
        let mut writer = ReadWriter {
            gate: None,
            bytes: vec![],
        };
        fs.read(ctx, entry.inode, handle, &mut writer, 15, 0, None, 0)
            .unwrap();
        assert_eq!(
            writer.bytes, b"real image data",
            "the opened lower identity survives copy-up"
        );
        fs.capture_state().unwrap();
        assert_eq!(
            std::fs::read(lower.join("file")).unwrap(),
            b"real image data"
        );
    }
}
/// Adapter-only benchmark: no VM boot or guest/kernel cache. Use nextest
/// --run-ignored ignored-only and --no-capture. Setup is outside the timer.
#[test]
#[ignore = "manual small-file performance measurement"]
fn small_file_adapter_benchmark() {
    use pvisor_overlay_core::profile::Profile;
    use std::time::Instant;
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let deep = temp.path().join("deep");
    for directory in 0..32 {
        for root in [
            lower.join(format!("d{directory:02}")),
            deep.join(format!("d{directory:02}/a/b/c/d/e/f/g")),
        ] {
            std::fs::create_dir_all(&root).unwrap();
            for file in 0..64 {
                std::fs::write(root.join(format!("f{file:04}")), b"small-file-fixture").unwrap();
            }
        }
    }
    for case in [
        "lookup_getattr",
        "lookup_open_getattr",
        "deep_lookup_open_getattr",
        "directory_plus",
    ] {
        for trial in 0..11 {
            let backing_root = if case == "deep_lookup_open_getattr" {
                &deep
            } else {
                &lower
            };
            let mut fs = OverlayFs::new(
                Config {
                    lower_mutability: Vec::new(),
                    lower_dirs: vec![backing_root.to_string_lossy().into_owned()],
                    apply_target: None,
                    baseline_lower: None,
                    baseline_content_index: None,
                    upper_dir: temp
                        .path()
                        .join(format!("upper-{case}-{trial}"))
                        .to_string_lossy()
                        .into_owned(),
                    work_dir: None,
                    preimage_dir: None,
                    excluded_paths: vec![],
                    access_policy: Default::default(),
                    semantics: passthrough::PermissionSemantics::LinuxComplete,
                },
                Arc::new(InodeAllocator::new()),
            )
            .unwrap();
            fs.init(FsOptions::empty()).unwrap();
            if trial == 10 {
                fs.profile = Profile::enabled("virtio-fs-overlay");
                fs.core = fs.core.with_profile(Profile::enabled("overlay-core"));
            }
            let ctx = Context {
                uid: 0,
                gid: 0,
                pid: 1,
            };
            let started = Instant::now();
            let mut count = 0;
            for directory in 0..32 {
                let name = CString::new(format!("d{directory:02}")).unwrap();
                let mut parent = fs.lookup(ctx, fuse::ROOT_ID, &name).unwrap();
                if case == "deep_lookup_open_getattr" {
                    for component in [c"a", c"b", c"c", c"d", c"e", c"f", c"g"] {
                        parent = fs.lookup(ctx, parent.inode, component).unwrap();
                    }
                }
                if case != "directory_plus" {
                    for file in 0..64 {
                        let name = CString::new(format!("f{file:04}")).unwrap();
                        let entry = fs.lookup(ctx, parent.inode, &name).unwrap();
                        let handle =
                            if matches!(case, "lookup_open_getattr" | "deep_lookup_open_getattr") {
                                Some(
                                    fs.open(ctx, entry.inode, false, libc::O_RDONLY as u32)
                                        .unwrap()
                                        .0
                                        .expect("opened file must have a handle"),
                                )
                            } else {
                                None
                            };
                        let (attr, _) = fs.getattr(ctx, entry.inode, handle).unwrap();
                        assert_eq!(attr.st_size, 18);
                        if let Some(handle) = handle {
                            fs.release(
                                ctx,
                                entry.inode,
                                libc::O_RDONLY as u32,
                                handle,
                                false,
                                false,
                                None,
                            )
                            .unwrap();
                        }
                        count += 1;
                    }
                } else {
                    let handle = fs.opendir(ctx, parent.inode, 0).unwrap().0.unwrap();
                    fs.readdirplus(ctx, parent.inode, handle, 1 << 20, 0, |_, entry| {
                        assert_eq!(entry.attr.st_size, 18);
                        count += 1;
                        Ok(1)
                    })
                    .unwrap();
                    fs.releasedir(ctx, parent.inode, 0, handle).unwrap();
                }
            }
            assert_eq!(count, 2048);
            let record = serde_json::json!({"scope":"adapter-only, host cache warm, fresh inode tables; no guest kernel/VM",
                    "case":case,"trial":trial,"warmup":trial<2,"profiled":trial==10,
                    "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                    "adapter_profile":fs.profile.report(),"core_profile":fs.core.profile_report()});
            println!("PVISOR_FS_BENCH {record}");
        }
    }
}
