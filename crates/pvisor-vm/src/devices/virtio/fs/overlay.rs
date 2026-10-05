//! Portable copy-on-write overlay served directly over virtio-fs.
//!
//! The union semantics live in `pvisor-overlay-core`; the existing
//! platform passthrough implementation is retained for Linux permission
//! emulation and for the actual FUSE request I/O on each resolved layer.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pvisor_overlay_core::{BackingIdentity, BackingResolution, OverlayCore};

use super::super::linux_errno::linux_error;
use super::bindings;
use super::filesystem::{
    Context, DirEntry, Entry, Extensions, FileSystem, FsOptions, GetxattrReply, ListxattrReply,
    OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::fuse;
use super::inode_alloc::InodeAllocator;
use super::passthrough::{self, PassthroughFs};

const TTL: Duration = Duration::from_secs(1);
const DIRECTORY_CACHE_CAPACITY: usize = 256;
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

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
    File(FileHandle),
    Directory(Vec<DirectoryItem>),
}

#[derive(Default)]
struct Nodes {
    by_inode: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
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
    handles: Vec<(u64, Handle)>,
    next_handle: u64,
}

impl OverlaySnapshot {
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
        self.rebind_roots(relocate, &[])
    }

    pub(super) fn rebind_roots(
        &mut self,
        relocate: impl Fn(&str) -> io::Result<String>,
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
        config.work_dir = config.work_dir.as_deref().map(&relocate).transpose()?;
        config.preimage_dir = config.preimage_dir.as_deref().map(&relocate).transpose()?;
        config.apply_target = config.apply_target.as_deref().map(&relocate).transpose()?;
        if let Some(index) = &mut config.baseline_content_index {
            if !retained.iter().any(|root| root == &index.root) {
                index.root = PathBuf::from(relocate(
                    index
                        .root
                        .to_str()
                        .ok_or_else(|| invalid("baseline index root is not UTF-8"))?,
                )?);
                index.file = PathBuf::from(relocate(
                    index
                        .file
                        .to_str()
                        .ok_or_else(|| invalid("baseline index path is not UTF-8"))?,
                )?);
            }
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
            layer.rebind_owned_copy(Path::new(original), Path::new(copied))?;
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
        )?;
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

struct CachedDirectory {
    identity: BackingIdentity,
    inode: u64,
    used: u64,
}

#[derive(Default)]
struct DirectoryCache {
    entries: HashMap<(usize, PathBuf), CachedDirectory>,
    clock: u64,
}

pub struct OverlayFs {
    profile: pvisor_overlay_core::profile::Profile,
    // ponytail: serialize requests so guest renames cannot race path checks/open;
    // use directory-fd-based resolution before relaxing this for throughput.
    operation_lock: Mutex<()>,
    core: OverlayCore,
    snapshot_config: Config,
    roots: Vec<PathBuf>,
    layers: Vec<PassthroughFs>,
    // Owns one lookup reference per entry, never attributes or policy results.
    // Checked against fresh Core parent identities before every use.
    directory_cache: Mutex<DirectoryCache>,
    inode_alloc: Arc<InodeAllocator>,
    nodes: Mutex<Nodes>,
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

    fn build(cfg: Config, inode_alloc: Arc<InodeAllocator>, restoring: bool) -> io::Result<Self> {
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
        )?;
        let open = if restoring {
            OverlayCore::open_existing_for_layout
        } else {
            OverlayCore::new_for_layout_with_compact_preimages
        };
        let core = open(layout, upper.clone(), work, excluded, preimages)?;
        let core = core.with_access_policy(&cfg.access_policy);
        let core = if let Some(index) = &cfg.baseline_content_index {
            core.with_immutable_content_index(&index.root, index.file.clone(), &index.sha256)?
        } else {
            core
        };

        let mut roots = Vec::with_capacity(lowers.len() + 1);
        roots.push(upper);
        roots.extend(lowers);
        let layers = roots
            .iter()
            .map(|root| {
                PassthroughFs::new(
                    passthrough::Config {
                        root_dir: root.to_string_lossy().into_owned(),
                        semantics: cfg.semantics,
                        attr_timeout: TTL,
                        entry_timeout: TTL,
                        ..Default::default()
                    },
                    inode_alloc.clone(),
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut nodes = Nodes::default();
        nodes.by_inode.insert(fuse::ROOT_ID, PathBuf::new());
        nodes.by_path.insert(PathBuf::new(), fuse::ROOT_ID);
        Ok(Self {
            profile: pvisor_overlay_core::profile::Profile::from_env("virtio-fs-overlay"),
            operation_lock: Mutex::new(()),
            core,
            snapshot_config: cfg,
            roots,
            layers,
            directory_cache: Mutex::new(DirectoryCache::default()),
            inode_alloc,
            nodes: Mutex::new(nodes),
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
        nodes.by_path.insert(path.clone(), inode);
        nodes.by_inode.insert(inode, path);
        inode
    }

    fn remove_path(&self, prefix: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let paths = nodes
            .by_path
            .keys()
            .filter(|path| *path == prefix || path.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            if let Some(inode) = nodes.by_path.remove(&path) {
                nodes.by_inode.remove(&inode);
            }
        }
    }

    fn remap_path(&self, old: &Path, new: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let changes = nodes
            .by_path
            .iter()
            .filter(|(path, _)| *path == old || path.starts_with(old))
            .map(|(path, inode)| {
                let suffix = path.strip_prefix(old).unwrap();
                let replacement = if suffix.as_os_str().is_empty() {
                    new.to_path_buf()
                } else {
                    new.join(suffix)
                };
                (path.clone(), replacement, *inode)
            })
            .collect::<Vec<_>>();
        for (old_path, _, _) in &changes {
            nodes.by_path.remove(old_path);
        }
        for (_, new_path, inode) in changes {
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

    fn inner_entry_with_parents(
        &self,
        layer: Layer,
        path: &Path,
        ctx: Context,
        parents: &[BackingIdentity],
    ) -> io::Result<Entry> {
        let _span = self.profile.span("inner_inode");
        let fs = &self.layers[layer.0];
        let mut inode = fuse::ROOT_ID;
        let mut temporary_reference = false;
        let mut result = None;
        let mut prefix = PathBuf::new();
        let mut cache = (!parents.is_empty()
            && parents.len() + 1 == path.components().count()
            && parents
                .iter()
                .all(|identity| cfg!(target_os = "macos") || identity.mount_id.is_some()))
        .then(|| self.directory_cache.lock().unwrap());
        // Core has freshly checked every physical ancestor for this request.
        // A matching deepest directory therefore lets us start at its held
        // inode directly; walking all cached prefixes adds no validation.
        // Cache misses still take the complete lookup/identity path below.
        let start = if let (Some(cache), Some(identity), Some(parent)) =
            (&mut cache, parents.last(), path.parent())
        {
            cache.clock = cache.clock.saturating_add(1);
            let used = cache.clock;
            if let Some(directory) = cache
                .entries
                .get_mut(&(layer.0, parent.to_path_buf()))
                .filter(|directory| directory.identity == *identity)
            {
                inode = directory.inode;
                directory.used = used;
                self.profile.add("directory_cache_deep_hits", 1);
                parents.len()
            } else {
                0
            }
        } else {
            0
        };
        for (index, component) in path.components().enumerate().skip(start) {
            let key = if cache.is_some() && index < parents.len() {
                prefix.push(component.as_os_str());
                Some((layer.0, prefix.clone()))
            } else {
                None
            };
            if let (Some(cache), Some(identity), Some(key)) =
                (&mut cache, parents.get(index), key.as_ref())
            {
                cache.clock = cache.clock.saturating_add(1);
                let used = cache.clock;
                if let Some(directory) = cache
                    .entries
                    .get_mut(key)
                    .filter(|directory| directory.identity == *identity)
                {
                    if temporary_reference {
                        fs.forget(ctx, inode, 1);
                    }
                    inode = directory.inode;
                    directory.used = used;
                    temporary_reference = false;
                    self.profile.add("directory_cache_hits", 1);
                    continue;
                }
                if let Some(stale) = cache.entries.remove(key) {
                    fs.forget(ctx, stale.inode, 1);
                    self.profile.add("directory_cache_invalidations", 1);
                }
                self.profile.add("directory_cache_misses", 1);
            }
            let name = CString::new(component.as_os_str().as_bytes())?;
            let entry = {
                let _span = self.profile.span("backing_lookup");
                fs.lookup(ctx, inode, &name)
            };
            if temporary_reference {
                fs.forget(ctx, inode, 1);
            }
            let entry = entry?;
            inode = entry.inode;
            temporary_reference = inode != fuse::ROOT_ID;
            let matching_parent = parents.get(index).filter(|identity| {
                cache.is_some()
                    && **identity
                        == BackingIdentity {
                            device: entry.attr.st_dev as _,
                            inode: entry.attr.st_ino as _,
                            mount_id: fs.lookup_mount_id(inode),
                        }
            });
            if let (Some(cache), Some(identity), Some(key)) = (&mut cache, matching_parent, key) {
                if cache.entries.len() == DIRECTORY_CACHE_CAPACITY {
                    let oldest = cache
                        .entries
                        .iter()
                        .min_by_key(|(_, directory)| directory.used)
                        .map(|(key, _)| key.clone())
                        .unwrap();
                    let directory = cache.entries.remove(&oldest).unwrap();
                    self.layers[oldest.0].forget(ctx, directory.inode, 1);
                    self.profile.add("directory_cache_evictions", 1);
                }
                let used = cache.clock;
                cache.entries.insert(
                    key,
                    CachedDirectory {
                        identity: *identity,
                        inode,
                        used,
                    },
                );
                temporary_reference = false;
            }
            result = Some(entry);
        }
        if let Some(entry) = result {
            return Ok(entry);
        }
        let (attr, timeout) = fs.getattr(ctx, fuse::ROOT_ID, None)?;
        Ok(Entry {
            inode: fuse::ROOT_ID,
            generation: 0,
            attr,
            attr_flags: 0,
            attr_timeout: timeout,
            entry_timeout: TTL,
        })
    }

    fn inner_inode(&self, layer: Layer, path: &Path, ctx: Context) -> io::Result<u64> {
        Ok(self.inner_entry(layer, path, ctx)?.inode)
    }

    fn observed_inner(&self, ctx: Context, path: &Path) -> io::Result<(Layer, u64)> {
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
        let mut entry = self.inner_entry_with_parents(layer, path, ctx, &backing.parents)?;
        if entry.inode != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, entry.inode, 1);
        }
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

    fn clear_directory_cache(&self) {
        let mut cache = self.directory_cache.lock().unwrap();
        for ((layer, _), directory) in cache.entries.drain() {
            self.layers[layer].forget(
                Context {
                    uid: 0,
                    gid: 0,
                    pid: 0,
                },
                directory.inode,
                1,
            );
        }
    }

    fn writable_inner(&self, ctx: Context, path: &Path) -> io::Result<u64> {
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
        let handles = self.handles.lock().unwrap();
        match handles.get(&id) {
            Some(Handle::File(handle)) => f(&self.layers[handle.layer.0], handle),
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn dtype(mode: u32) -> u32 {
        // POSIX file type occupies bits 12..15; FUSE d_type uses bits 0..3.
        (mode >> 12) & 0xf
    }
}

impl FileSystem for OverlayFs {
    fn capture_state(&self) -> io::Result<super::snapshot::FsSnapshot> {
        let _operation = self.operation_lock.lock().unwrap();
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
        Ok(super::snapshot::FsSnapshot::Overlay(Box::new(
            OverlaySnapshot {
                config: self.snapshot_config.clone(),
                hard_links,
                hard_link_origins,
                layers: self
                    .layers
                    .iter()
                    .map(FileSystem::capture_state)
                    .collect::<io::Result<_>>()?,
                nodes: self
                    .nodes
                    .lock()
                    .unwrap()
                    .by_inode
                    .iter()
                    .map(|(inode, path)| (*inode, path.as_os_str().as_bytes().to_vec()))
                    .collect(),
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
        use super::snapshot::{invalid, FsSnapshot};
        let FsSnapshot::Overlay(state) = state else {
            return Err(invalid("overlay filesystem type mismatch"));
        };
        let _operation = self.operation_lock.lock().unwrap();
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
        let mut handles = HashMap::new();
        for (id, handle) in &state.handles {
            if *id == 0 || *id >= state.next_handle || handles.insert(*id, handle.clone()).is_some()
            {
                return Err(invalid("invalid overlay handle"));
            }
            match handle {
                Handle::File(file) => {
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
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
        Ok(options)
    }

    fn destroy(&self) {
        self.clear_directory_cache();
        self.handles.lock().unwrap().clear();
        for layer in &self.layers {
            layer.destroy();
        }
    }

    fn lookup(&self, ctx: Context, parent: u64, name: &CStr) -> io::Result<Entry> {
        let _span = self.profile.span("lookup");
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        let backing = self
            .core
            .metadata_for_backing_lookup(&path)
            .map_err(linux_error)?;
        let inode = self.allocate_inode(path.clone());
        self.entry_on_backing(ctx, &path, inode, &backing)
    }

    fn getattr(
        &self,
        ctx: Context,
        inode: u64,
        handle: Option<u64>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let _span = self.profile.span("getattr");
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        // GETATTR may omit FH (e.g. stat through /proc/self/fd). An unlinked
        // inode still belongs to its open file, even after its path is gone.
        let handle = handle.or_else(|| {
            if self.path(inode).is_ok() {
                return None;
            }
            // ponytail: scan open handles only for detached inodes; index by
            // inode if workloads with many deleted-open files make this costly.
            self.handles.lock().unwrap().iter().find_map(|(id, h)| {
                matches!(h, Handle::File(h) if h.overlay_inode == inode).then_some(*id)
            })
        });
        if let Some(handle) = handle {
            return self.with_file_handle(handle, |fs, h| {
                if h.overlay_inode != inode {
                    return Err(io::Error::from_raw_os_error(libc::EBADF));
                }
                let (mut attr, timeout) = fs.getattr(ctx, h.inode, Some(h.handle))?;
                attr.st_ino = inode as _;
                Ok((attr, timeout))
            });
        }
        let entry = self.entry(ctx, &self.path(inode)?, inode)?;
        Ok((entry.attr, entry.attr_timeout))
    }

    fn setattr(
        &self,
        ctx: Context,
        inode: u64,
        attr: bindings::stat64,
        _handle: Option<u64>,
        valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        self.core
            .prepare_metadata_change(&path)
            .map_err(linux_error)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].setattr(ctx, inner, attr, None, valid);
        self.layers[0].forget(ctx, inner, 1);
        let (mut attr, timeout) = result?;
        attr.st_ino = inode as _;
        Ok((attr, timeout))
    }

    fn readlink(&self, ctx: Context, inode: u64) -> io::Result<Vec<u8>> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0].symlink(ctx, linkname, upper_parent, &upper_name, extensions)?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0].mknod(
            ctx,
            upper_parent,
            &upper_name,
            mode,
            rdev,
            umask,
            extensions,
        )?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0]
            .mkdir(ctx, upper_parent, &upper_name, mode, umask, extensions)
            .inspect_err(|error| log::error!("overlay native mkdir failed: {error}"))?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn unlink(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        self.core.remove(&path, false).map_err(linux_error)?;
        self.remove_path(&path);
        Ok(())
    }

    fn rmdir(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let old = self.child(olddir, oldname)?;
        let new = self.child(newdir, newname)?;
        match flags {
            0 => self.core.rename(&old, &new, false).map_err(linux_error)?,
            RENAME_NOREPLACE => self.core.rename(&old, &new, true).map_err(linux_error)?,
            RENAME_EXCHANGE => self.core.exchange(&old, &new).map_err(linux_error)?,
            _ => return Err(linux_error(io::Error::from_raw_os_error(libc::ENOTSUP))),
        }
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let source = self.path(inode)?;
        let destination = self.child(newparent, newname)?;
        self.core
            .hard_link(&source, &destination)
            .map_err(linux_error)?;
        let new_inode = self.allocate_inode(destination.clone());
        self.entry(ctx, &destination, new_inode)
    }

    fn open(
        &self,
        ctx: Context,
        inode: u64,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let _span = self.profile.span("open");
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        let writing = flags as i32 & libc::O_ACCMODE != libc::O_RDONLY
            || flags as i32 & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let (layer, inner) = if writing {
            // Symlinks are resolved by guest lookups, never by passthrough.
            if self
                .core
                .metadata(&path)
                .map_err(linux_error)?
                .file_type()
                .is_symlink()
            {
                return Err(linux_error(io::Error::from_raw_os_error(libc::ELOOP)));
            }
            self.core.copy_up(&path).map_err(linux_error)?;
            (Layer(0), self.inner_inode(Layer(0), &path, ctx)?)
        } else {
            let backing = self
                .core
                .prepare_file_read_for_backing_lookup(&path)
                .map_err(linux_error)?;
            let layer = Layer(backing.entry.layer);
            let inner = self
                .inner_entry_with_parents(layer, &path, ctx, &backing.parents)?
                .inode;
            (layer, inner)
        };
        let opened = self.layers[layer.0].open(ctx, inner, kill_priv, flags);
        let (handle, options) = match opened {
            Ok((Some(handle), options)) => (handle, options),
            result => {
                self.layers[layer.0].forget(ctx, inner, 1);
                return match result {
                    Err(error) => Err(error),
                    _ => Err(io::Error::from_raw_os_error(libc::EIO)),
                };
            }
        };
        let id = self.allocate_handle(Handle::File(FileHandle {
            overlay_inode: inode,
            layer,
            inode: inner,
            handle,
        }));
        Ok((Some(id), options))
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
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
        let id = self.allocate_handle(Handle::File(FileHandle {
            overlay_inode: inode,
            layer: Layer(0),
            inode: inner_inode,
            handle,
        }));
        Ok((entry, Some(id), options))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        w: W,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        flags: u32,
    ) -> io::Result<usize> {
        let _span = self.profile.span("read");
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        self.with_file_handle(handle, |fs, h| {
            fs.read(ctx, h.inode, h.handle, w, size, offset, lock_owner, flags)
        })
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        r: R,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        delayed_write: bool,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<usize> {
        let _span = self.profile.span("write");
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        self.with_file_handle(handle, |fs, h| {
            fs.write(
                ctx,
                h.inode,
                h.handle,
                r,
                size,
                offset,
                lock_owner,
                delayed_write,
                kill_priv,
                flags,
            )
        })
    }

    fn flush(&self, ctx: Context, _inode: u64, handle: u64, lock_owner: u64) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        self.with_file_handle(handle, |fs, h| fs.flush(ctx, h.inode, h.handle, lock_owner))
    }

    fn fsync(&self, ctx: Context, _inode: u64, datasync: bool, handle: u64) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        self.with_file_handle(handle, |fs, h| fs.fsync(ctx, h.inode, datasync, h.handle))
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
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
                result
            }
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn statfs(&self, ctx: Context, inode: u64) -> io::Result<bindings::statvfs64> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        pvisor_overlay_core::validate_guest_xattr(OsStr::from_bytes(name.to_bytes()))
            .map_err(linux_error)?;
        let path = self.path(inode)?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        let (layer, inner) = self.observed_inner(ctx, &path)?;
        let result = self.layers[layer.0].getxattr(ctx, inner, name, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn listxattr(&self, ctx: Context, inode: u64, size: u32) -> io::Result<ListxattrReply> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        let (layer, inner) = self.observed_inner(ctx, &path)?;
        let result = self.layers[layer.0].listxattr(ctx, inner, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn removexattr(&self, ctx: Context, inode: u64, name: &CStr) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        pvisor_overlay_core::validate_guest_xattr(OsStr::from_bytes(name.to_bytes()))
            .map_err(linux_error)?;
        let path = self.path(inode)?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        let mut items = Vec::new();
        for entry in self.core.list_entries(&path).map_err(linux_error)? {
            let child = OverlayCore::child(&path, &entry.name).map_err(linux_error)?;
            let child_inode = self.allocate_inode(child);
            items.push(DirectoryItem {
                ino: child_inode,
                name: entry.name.as_bytes().to_vec(),
                type_: Self::dtype(entry.backing.metadata.mode()),
            });
        }
        let handle = self.allocate_handle(Handle::Directory(items));
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let handles = self.handles.lock().unwrap();
        let items = match handles.get(&handle) {
            Some(Handle::Directory(items)) => items,
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let handles = self.handles.lock().unwrap();
        let items = match handles.get(&handle) {
            Some(Handle::Directory(items)) => items,
            _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
        };
        for (index, item) in items.iter().enumerate().skip(offset as usize) {
            let path = self.path(item.ino)?;
            let entry = self.entry(ctx, &path, item.ino)?;
            if add_entry(
                DirEntry {
                    ino: item.ino as _,
                    offset: (index + 1) as u64,
                    type_: item.type_,
                    name: &item.name,
                },
                entry,
            )? == 0
            {
                break;
            }
        }
        Ok(())
    }

    fn releasedir(&self, _ctx: Context, _inode: u64, _flags: u32, handle: u64) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        match self.handles.lock().unwrap().remove(&handle) {
            Some(Handle::Directory(_)) => Ok(()),
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn access(&self, ctx: Context, inode: u64, mask: u32) -> io::Result<()> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        self.with_file_handle(handle, |fs, h| {
            fs.lseek(ctx, h.inode, h.handle, offset, whence)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ctx = Context { uid: 0, gid: 0, pid: 1 };
        let parent = fs.lookup(ctx, fuse::ROOT_ID, c"allowed").unwrap();
        let link = fs.lookup(ctx, parent.inode, c"link").unwrap();
        assert_eq!(fs.readlink(ctx, link.inode).unwrap(), b"old");
        std::fs::rename(lower.join("allowed"), lower.join("old-allowed")).unwrap();
        std::fs::create_dir(lower.join("allowed")).unwrap();
        std::os::unix::fs::symlink("replacement", lower.join("allowed/link")).unwrap();
        assert_eq!(fs.readlink(ctx, link.inode).unwrap(), b"replacement");
        std::fs::remove_file(lower.join("allowed/link")).unwrap();
        std::fs::hard_link(lower.join("private/secret"), lower.join("allowed/link")).unwrap();
        assert_eq!(fs.getxattr(ctx, link.inode, c"user.test", 0).err().unwrap().raw_os_error(), Some(libc::EACCES));
        assert_eq!(fs.listxattr(ctx, link.inode, 0).err().unwrap().raw_os_error(), Some(libc::EACCES));
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
        let ctx = Context { uid: 0, gid: 0, pid: 1 };
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
        assert_eq!(fs.profile.report().unwrap().measurements["directory_cache_deep_hits"].units, 2);
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
        for i in 0..DIRECTORY_CACHE_CAPACITY + 4 {
            let name = format!("d{i}");
            std::fs::create_dir(lower.join(&name)).unwrap();
            std::fs::write(lower.join(&name).join("file"), b"data").unwrap();
            let parent = fs
                .lookup(ctx, fuse::ROOT_ID, &CString::new(name).unwrap())
                .unwrap();
            fs.lookup(ctx, parent.inode, c"file").unwrap();
            assert!(fs.directory_cache.lock().unwrap().entries.len() <= DIRECTORY_CACHE_CAPACITY);
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
            handles: vec![],
            next_handle: 2,
        };
        // Previously the tagged enum stored this state directly. Box must add
        // no wrapper to the persisted format, including when nested in layers.
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
