//! Portable copy-on-write overlay served directly over virtio-fs.
//!
//! The union semantics live in `pvisor-overlay-core`; the existing
//! platform passthrough implementation is retained for Linux permission
//! emulation and for the actual FUSE request I/O on each resolved layer.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pvisor_overlay_core::OverlayCore;

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
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

#[derive(Clone, Debug)]
#[cfg_attr(
    target_os = "macos",
    derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(target_os = "macos", serde(deny_unknown_fields))]
pub struct Config {
    pub lower_dirs: Vec<String>,
    pub upper_dir: String,
    pub work_dir: Option<String>,
    pub preimage_dir: Option<String>,
    pub excluded_paths: Vec<String>,
    pub access_policy: pvisor_overlay_core::FileAccessPolicy,
    pub semantics: passthrough::PermissionSemantics,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(target_os = "macos", derive(serde::Serialize, serde::Deserialize))]
struct Layer(usize);

#[derive(Clone, Debug)]
#[cfg_attr(target_os = "macos", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(target_os = "macos", serde(deny_unknown_fields))]
struct FileHandle {
    overlay_inode: u64,
    layer: Layer,
    inode: u64,
    handle: u64,
}

#[derive(Clone, Debug)]
#[cfg_attr(target_os = "macos", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(target_os = "macos", serde(deny_unknown_fields))]
struct DirectoryItem {
    ino: u64,
    name: Vec<u8>,
    type_: u32,
}

#[derive(Clone, Debug)]
#[cfg_attr(target_os = "macos", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    target_os = "macos",
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

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlaySnapshot {
    config: Config,
    hard_links: Vec<(u64, u64, Vec<PathBuf>)>,
    layers: Vec<super::snapshot::FsSnapshot>,
    nodes: Vec<(u64, Vec<u8>)>,
    handles: Vec<(u64, Handle)>,
    next_handle: u64,
}

#[cfg(target_os = "macos")]
impl OverlaySnapshot {
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

pub struct OverlayFs {
    // ponytail: serialize requests so guest renames cannot race path checks/open;
    // use directory-fd-based resolution before relaxing this for throughput.
    operation_lock: Mutex<()>,
    core: OverlayCore,
    #[cfg(target_os = "macos")]
    snapshot_config: Config,
    roots: Vec<PathBuf>,
    layers: Vec<PassthroughFs>,
    inode_alloc: Arc<InodeAllocator>,
    nodes: Mutex<Nodes>,
    handles: Mutex<HashMap<u64, Handle>>,
    next_handle: AtomicU64,
}

impl OverlayFs {
    pub fn new(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        Self::build(cfg, inode_alloc, false)
    }

    #[cfg(target_os = "macos")]
    pub fn open_existing(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        Self::build(cfg, inode_alloc, true)
    }

    fn build(cfg: Config, inode_alloc: Arc<InodeAllocator>, restoring: bool) -> io::Result<Self> {
        if cfg.lower_dirs.is_empty() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let lowers = cfg.lower_dirs.iter().map(PathBuf::from).collect::<Vec<_>>();
        let upper = PathBuf::from(&cfg.upper_dir);
        let work = cfg.work_dir.as_ref().map(PathBuf::from);
        let preimages = cfg.preimage_dir.as_ref().map(PathBuf::from);
        let excluded = cfg.excluded_paths.iter().map(PathBuf::from).collect();
        let open = if restoring {
            OverlayCore::open_existing
        } else {
            OverlayCore::new_with_exclusions_and_preimages
        };
        let core = open(lowers.clone(), upper.clone(), work, excluded, preimages)?
            .with_access_policy(&cfg.access_policy);

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
            operation_lock: Mutex::new(()),
            core,
            #[cfg(target_os = "macos")]
            snapshot_config: cfg,
            roots,
            layers,
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

    fn inner_inode(&self, layer: Layer, path: &Path, ctx: Context) -> io::Result<u64> {
        let fs = &self.layers[layer.0];
        let mut inode = fuse::ROOT_ID;
        for component in path.components() {
            let name = CString::new(component.as_os_str().as_bytes())?;
            let entry = fs.lookup(ctx, inode, &name)?;
            if inode != fuse::ROOT_ID {
                fs.forget(ctx, inode, 1);
            }
            inode = entry.inode;
        }
        Ok(inode)
    }

    fn entry(&self, ctx: Context, path: &Path, inode: u64) -> io::Result<Entry> {
        let layer = self.layer(path)?;
        let inner = self.inner_inode(layer, path, ctx)?;
        let (mut attr, timeout) = self.layers[layer.0].getattr(ctx, inner, None)?;
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        attr.st_ino = inode as _;
        Ok(Entry {
            inode,
            generation: 0,
            attr,
            attr_flags: 0,
            attr_timeout: timeout,
            entry_timeout: TTL,
        })
    }

    fn writable_inner(&self, ctx: Context, path: &Path) -> io::Result<u64> {
        // OverlayCore reports host errno; passthrough already reports Linux
        // errno. Convert only at the core boundary, never the whole request.
        self.core.copy_up(path).map_err(linux_error)?;
        self.inner_inode(Layer(0), path, ctx)
    }

    fn upper_parent(&self, ctx: Context, path: &Path) -> io::Result<(u64, CString)> {
        self.core.clear_whiteout(path).map_err(linux_error)?;
        self.core.ensure_upper_parents(path).map_err(linux_error)?;
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

    fn dtype(mode: libc::mode_t) -> u32 {
        ((mode & libc::S_IFMT) >> 12) as u32
    }
}

impl FileSystem for OverlayFs {
    #[cfg(target_os = "macos")]
    fn capture_state(&self) -> io::Result<super::snapshot::FsSnapshot> {
        let _operation = self.operation_lock.lock().unwrap();
        Ok(super::snapshot::FsSnapshot::Overlay(OverlaySnapshot {
            config: self.snapshot_config.clone(),
            hard_links: self.core.capture_hard_links()?,
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
        }))
    }
    #[cfg(target_os = "macos")]
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
        for (layer, saved) in self.layers.iter().zip(&state.layers) {
            layer.restore_state(saved)?;
        }
        self.core.restore_hard_links(&state.hard_links)?;
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
        Ok(options.unwrap_or_else(FsOptions::empty))
    }

    fn destroy(&self) {
        self.handles.lock().unwrap().clear();
        for layer in &self.layers {
            layer.destroy();
        }
    }

    fn lookup(&self, ctx: Context, parent: u64, name: &CStr) -> io::Result<Entry> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.child(parent, name)?;
        self.core.metadata(&path).map_err(linux_error)?;
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn getattr(
        &self,
        ctx: Context,
        inode: u64,
        handle: Option<u64>,
    ) -> io::Result<(bindings::stat64, Duration)> {
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
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
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
        self.layers[0].mkdir(ctx, upper_parent, &upper_name, mode, umask, extensions)?;
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
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        // Symlinks are resolved by the guest through overlay lookups, never by
        // the passthrough layer against an unfiltered lower directory.
        if self
            .core
            .metadata(&path)
            .map_err(linux_error)?
            .file_type()
            .is_symlink()
        {
            return Err(linux_error(io::Error::from_raw_os_error(libc::ELOOP)));
        }
        let writing = flags as i32 & libc::O_ACCMODE != libc::O_RDONLY
            || flags as i32 & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let layer = if writing {
            self.core.copy_up(&path).map_err(linux_error)?;
            Layer(0)
        } else {
            self.layer(&path)?
        };
        let inner = self.inner_inode(layer, &path, ctx)?;
        let (handle, options) = self.layers[layer.0].open(ctx, inner, kill_priv, flags)?;
        let handle = handle.ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
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
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
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
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
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
        ctx: Context,
        inode: u64,
        _flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let path = self.path(inode)?;
        let mut items = Vec::new();
        for name in self.core.list_names(&path).map_err(linux_error)? {
            let child = OverlayCore::child(&path, &name).map_err(linux_error)?;
            let child_inode = self.allocate_inode(child.clone());
            let entry = self.entry(ctx, &child, child_inode)?;
            items.push(DirectoryItem {
                ino: child_inode,
                name: name.as_bytes().to_vec(),
                type_: Self::dtype(entry.attr.st_mode),
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

        let created = CString::new("created").unwrap();
        let (entry, handle, _) = fs
            .create(
                ctx,
                fuse::ROOT_ID,
                &created,
                libc::S_IFREG as u32 | 0o640,
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
            let (entry, handle, _) = fs
                .create(
                    ctx,
                    fuse::ROOT_ID,
                    c"temporary",
                    libc::S_IFREG as u32 | 0o600,
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
