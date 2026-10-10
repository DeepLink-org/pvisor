use crate::api::{KernelCacheConfig, KernelCachePolicy};
use crate::cache::{CacheHandle, EFFECT_LIMIT, Effects};
#[cfg(target_os = "macos")]
use fuser::ReplyXTimes;
use fuser::{
    FUSE_ROOT_ID, FileAttr, FileType, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyLseek, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, TimeOrNow,
};
use pvisor_core::overlay::FileAccessPolicy;
use pvisor_overlay_core::service::FilesystemService;
use pvisor_overlay_core::{OverlayCore, sys};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn directory_file_type(type_: u32) -> FileType {
    match type_ as u8 {
        libc::DT_DIR => FileType::Directory,
        libc::DT_REG => FileType::RegularFile,
        libc::DT_LNK => FileType::Symlink,
        libc::DT_FIFO => FileType::NamedPipe,
        libc::DT_SOCK => FileType::Socket,
        libc::DT_BLK => FileType::BlockDevice,
        libc::DT_CHR => FileType::CharDevice,
        _ => FileType::RegularFile,
    }
}

fn set_append(file: &File, append: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    let desired = (status & !libc::O_APPEND) | if append { libc::O_APPEND } else { 0 };
    if desired != status && unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, desired) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_request(file: &File, data: &[u8], offset: u64, flags: i32) -> io::Result<usize> {
    // F_SETFL on the mounted fd is reflected in each FUSE WRITE's flags, not
    // another OPEN. Linux pwrite honors O_APPEND; sync only that mutable bit.
    set_append(file, flags & libc::O_APPEND != 0)?;
    file.write_at(data, offset)
}

fn copy_range(
    len: u64,
    mut read: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
    mut write: impl FnMut(&[u8], u64) -> io::Result<usize>,
) -> io::Result<u64> {
    let mut copied = 0;
    let mut buffer = vec![0; len.min(128 * 1024) as usize];
    let result = (|| {
        while copied < len {
            let wanted = (len - copied).min(buffer.len() as u64) as usize;
            let amount = read(&mut buffer[..wanted], copied)?;
            if amount == 0 {
                break;
            }
            let mut written = 0;
            while written < amount {
                let count = write(&buffer[written..amount], copied)?;
                if count == 0 {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                written += count;
                copied += count as u64;
            }
        }
        Ok(copied)
    })();
    // Every modified byte is acknowledged even if a later read/write fails.
    // Linux can then expire exactly that target page-cache range.
    if copied != 0 { Ok(copied) } else { result }
}

macro_rules! mutation_or_reply {
    ($expression:expr, $reply:expr) => {
        match $expression {
            Ok(mutation) => mutation,
            Err(error) => {
                $reply.error(errno(&error));
                return;
            }
        }
    };
}

const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

#[derive(Clone, Debug)]
struct Node {
    paths: BTreeSet<PathBuf>,
    lookups: u64,
}

struct OpenFile {
    file: File,
    ino: u64,
    path: PathBuf,
    flags: i32,
    backing: PathBuf,
}
impl std::ops::Deref for OpenFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ObjectKey {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
struct DirectoryEntry {
    ino: u64,
    kind: FileType,
    name: OsString,
    attr: Option<FileAttr>,
}

pub(super) struct Mutation {
    handle: CacheHandle,
    effects: Effects,
    objects: BTreeSet<u64>,
    paths: Vec<PathBuf>,
    subtree: bool,
}

pub(crate) struct OverlayFs {
    core: Arc<FilesystemService>,
    pub(super) pending_requests: usize,
    pub(super) pending_bytes: usize,
    pub(super) pending_inodes: HashMap<u64, usize>,
    profile: pvisor_overlay_core::profile::Profile,
    read_only: bool,
    private_root: bool,
    access_policy: FileAccessPolicy,
    observation: Option<crate::api::FsMetrics>,
    kernel_cache: KernelCacheConfig,
    cache_transport: Arc<OnceLock<CacheHandle>>,
    // Advisory coordination only. The caller still owns the lifetime proof.
    _cache_locks: Vec<File>,
    #[cfg(test)]
    copy_fault_after: Option<u64>,
    nodes: HashMap<u64, Node>,
    by_path: HashMap<PathBuf, u64>,
    by_object: HashMap<ObjectKey, u64>,
    next_ino: u64,
    open_files: HashMap<u64, OpenFile>,
    open_directories: HashMap<u64, Vec<DirectoryEntry>>,
    next_handle: u64,
}

fn errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

pub(super) fn setattr_requires_copy_up(
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    size: Option<u64>,
    _atime: Option<TimeOrNow>,
    mtime: Option<TimeOrNow>,
    flags: Option<u32>,
) -> bool {
    mode.is_some()
        || uid.is_some()
        || gid.is_some()
        || size.is_some()
        || mtime.is_some()
        || flags.is_some()
}

fn file_type(metadata: &fs::Metadata) -> FileType {
    let kind = metadata.file_type();
    if kind.is_dir() {
        FileType::Directory
    } else if kind.is_symlink() {
        FileType::Symlink
    } else if kind.is_block_device() {
        FileType::BlockDevice
    } else if kind.is_char_device() {
        FileType::CharDevice
    } else if kind.is_fifo() {
        FileType::NamedPipe
    } else if kind.is_socket() {
        FileType::Socket
    } else {
        FileType::RegularFile
    }
}

fn time_value(value: TimeOrNow) -> SystemTime {
    match value {
        TimeOrNow::SpecificTime(time) => time,
        TimeOrNow::Now => SystemTime::now(),
    }
}

impl OverlayFs {
    pub(super) fn finish_request(&mut self, bytes: usize, inodes: &[u64]) {
        self.pending_requests -= 1;
        self.pending_bytes -= bytes;
        for ino in inodes {
            if let Some(pins) = self.pending_inodes.get_mut(ino) {
                *pins -= 1;
                if *pins == 0 {
                    self.pending_inodes.remove(ino);
                    self.reclaim_inode(*ino);
                }
            }
        }
    }

    pub(super) fn begin_copy_preparation(&self, needed: bool) -> io::Result<Option<Mutation>> {
        if !needed {
            return Ok(None);
        }
        // Without a work directory, private staging changes upper-root attrs.
        // Reserve pending before preparation so metadata replies cannot cache
        // these transient attrs; invalidate the root even on preparation error.
        self.begin_mutation(&[Some(Path::new(""))], &[FUSE_ROOT_ID], false)
    }

    pub(super) fn finish_copy_preparation(
        &mut self,
        mutation: Option<Mutation>,
        reply: impl FnOnce(bool) + Send + 'static,
    ) {
        self.mutation_reply(mutation, reply);
    }

    pub(super) fn copy_plan(
        &self,
        request: &crate::dispatch::CopyRequest,
    ) -> io::Result<(Arc<FilesystemService>, crate::dispatch::CopyPlan)> {
        use crate::dispatch::{CopyPlan, CopyRequest};
        let mut paths = Vec::new();
        let mut truncate = false;
        if self.read_only {
            return Ok((self.core.clone(), CopyPlan::Files(paths, false)));
        }
        let inode = match request {
            CopyRequest::None => None,
            CopyRequest::Inode(ino) => Some(*ino),
            CopyRequest::Open { ino, flags } => {
                truncate = flags & libc::O_TRUNC != 0;
                (flags & libc::O_ACCMODE != libc::O_RDONLY
                    || flags & (libc::O_APPEND | libc::O_TRUNC) != 0)
                    .then_some(*ino)
            }
            CopyRequest::Setattr { ino, fh } => {
                if fh.is_some_and(|fh| {
                    !self
                        .open_files
                        .get(&fh)
                        .is_some_and(|file| file.ino == *ino)
                }) {
                    return Ok((self.core.clone(), CopyPlan::Files(paths, false)));
                }
                self.node_path(*ino).ok().map(|_| *ino)
            }
            CopyRequest::Unlink { parent, name } => {
                let path = self.child_path(*parent, name)?;
                self.by_path
                    .get(&path)
                    .copied()
                    .filter(|ino| self.open_files.values().any(|file| file.ino == *ino))
            }
            #[cfg(test)]
            CopyRequest::Paths(targets) => {
                paths.extend(targets.iter().cloned());
                None
            }
            CopyRequest::Rename {
                parent,
                name,
                newparent,
                newname,
                flags,
            } => {
                if flags & !3 != 0 || *flags == 3 {
                    return Ok((self.core.clone(), CopyPlan::Files(paths, false)));
                }
                let old = self.child_path(*parent, name)?;
                let new = self.child_path(*newparent, newname)?;
                for prefix in [&old, &new] {
                    for (path, ino) in &self.by_path {
                        if path.starts_with(prefix)
                            && self.open_files.values().any(|file| file.ino == *ino)
                        {
                            paths.extend(self.nodes[ino].paths.iter().cloned());
                        }
                    }
                }
                return Ok((
                    self.core.clone(),
                    CopyPlan::Rename {
                        old,
                        new,
                        flags: *flags,
                        open_paths: paths,
                    },
                ));
            }
        };
        if let Some(ino) = inode {
            self.node_path(ino)?;
            paths.extend(self.nodes[&ino].paths.iter().cloned());
        }
        paths.sort();
        paths.dedup();
        Ok((self.core.clone(), CopyPlan::Files(paths, truncate)))
    }

    #[cfg(test)]
    pub(crate) fn new(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Self::from_core(OverlayCore::new(lowers, upper, work)?)
    }

    pub(crate) fn with_access_policy(
        mut self,
        policy: &pvisor_overlay_core::FileAccessPolicy,
    ) -> Self {
        self.core = Arc::new(
            Arc::try_unwrap(self.core)
                .ok()
                .expect("configuration precedes dispatch")
                .with_access_policy(policy),
        );
        self.access_policy = policy.clone();
        self
    }

    pub(crate) fn with_private_root(mut self, private_root: bool) -> Self {
        self.private_root = private_root;
        self
    }

    pub(crate) fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub(crate) fn with_kernel_cache(mut self, config: KernelCacheConfig, locks: Vec<File>) -> Self {
        self.kernel_cache = config;
        self._cache_locks = locks;
        self
    }

    pub(crate) fn cache_slot(&self) -> Arc<OnceLock<CacheHandle>> {
        self.cache_transport.clone()
    }

    pub(crate) fn needs_notifications(&self) -> bool {
        !self.read_only && self.kernel_cache.policy == KernelCachePolicy::Metadata
    }

    fn cache_ttl(&self) -> Duration {
        if self.cache_transport.get().is_some_and(CacheHandle::pending) {
            return Duration::ZERO;
        }
        match self.kernel_cache.policy {
            KernelCachePolicy::Disabled => Duration::from_secs(1),
            KernelCachePolicy::Uncached => Duration::ZERO,
            KernelCachePolicy::Metadata | KernelCachePolicy::MetadataAndData => {
                self.kernel_cache.ttl
            }
        }
    }

    fn negative_ttl(&self) -> Duration {
        match self.kernel_cache.policy {
            KernelCachePolicy::Disabled | KernelCachePolicy::Uncached => Duration::ZERO,
            _ => self.cache_ttl(),
        }
    }

    fn begin_mutation(
        &self,
        paths: &[Option<&Path>],
        inodes: &[u64],
        subtree: bool,
    ) -> io::Result<Option<Mutation>> {
        let Some(handle) = self.cache_transport.get().cloned() else {
            return Ok(None);
        };
        if !handle.begin() {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let paths: Vec<_> = paths
            .iter()
            .flatten()
            .map(|path| path.to_path_buf())
            .collect();
        let mut effects = Effects::default();
        let objects = self.extend_effects(&mut effects, &paths, inodes, subtree);
        if effects.overflow {
            handle.submit(effects, |_| {});
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(Some(Mutation {
            handle,
            effects,
            objects,
            paths,
            subtree,
        }))
    }

    fn extend_effects(
        &self,
        effects: &mut Effects,
        paths: &[PathBuf],
        inodes: &[u64],
        subtree: bool,
    ) -> BTreeSet<u64> {
        if effects.overflow {
            return BTreeSet::new();
        }
        let mut objects: BTreeSet<_> = inodes.iter().copied().collect();
        for (path, ino) in &self.by_path {
            if paths
                .iter()
                .any(|prefix| path == prefix || (subtree && path.starts_with(prefix)))
            {
                objects.insert(*ino);
                if objects.len() > EFFECT_LIMIT {
                    effects.overflow = true;
                    return BTreeSet::new();
                }
            }
        }
        for ino in &objects {
            effects.inodes.insert(*ino);
            if effects.inodes.len() + effects.entries.len() > EFFECT_LIMIT {
                effects.overflow = true;
                return objects;
            }
        }
        // Stream aliases: never allocate an unbounded subtree/path snapshot.
        for path in paths.iter().chain(
            self.by_path
                .iter()
                .filter(|(_, ino)| objects.contains(ino))
                .map(|(path, _)| path),
        ) {
            if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
                && let Some(ino) = self.by_path.get(parent)
            {
                effects.entries.insert((*ino, name.to_os_string()));
            }
            // Copy-up can materialize every physical ancestor. Their merged
            // inode numbers remain stable but size/nlink/times may have changed.
            for ancestor in path.ancestors() {
                if let Some(ino) = self.by_path.get(ancestor) {
                    effects.inodes.insert(*ino);
                    if effects.inodes.len() + effects.entries.len() > EFFECT_LIMIT {
                        break;
                    }
                }
            }
            if effects.inodes.len() + effects.entries.len() > EFFECT_LIMIT {
                effects.overflow = true;
                break;
            }
        }
        objects
    }

    fn mutation_reply(
        &mut self,
        mutation: Option<Mutation>,
        reply: impl FnOnce(bool) + Send + 'static,
    ) {
        let Some(mut mutation) = mutation else {
            reply(true);
            return;
        };
        // Parent attributes are effects, not target objects. Do not expand
        // their aliases into parent-entry invalidations that would unnecessarily
        // evict unrelated sibling subtrees.
        let inodes: Vec<_> = mutation.objects.iter().copied().collect();
        self.extend_effects(
            &mut mutation.effects,
            &mutation.paths,
            &inodes,
            mutation.subtree,
        );
        // Recursive directory copy-up/rename changes backing object identities
        // without changing the existing FUSE inode identity. Bind those new
        // physical keys before a later LOOKUP can accidentally allocate a new ino.
        let bindings: Vec<_> = self
            .by_path
            .iter()
            .filter(|(_, ino)| mutation.effects.inodes.contains(ino))
            .filter_map(|(path, ino)| {
                self.core
                    .metadata(path)
                    .ok()
                    .filter(|m| !m.is_dir())
                    .map(|m| {
                        (
                            ObjectKey {
                                device: m.dev(),
                                inode: m.ino(),
                            },
                            *ino,
                        )
                    })
            })
            .collect();
        self.by_object.extend(bindings);
        mutation.handle.submit(mutation.effects, reply);
    }

    fn cache_open_flags(&self, metadata: &fs::Metadata) -> u32 {
        if self.read_only
            && self.kernel_cache.policy == KernelCachePolicy::MetadataAndData
            && metadata.is_file()
        {
            fuser::consts::FOPEN_KEEP_CACHE
        } else {
            0
        }
    }

    fn open_inode_with_backing(&mut self, ino: u64, flags: i32) -> io::Result<(File, PathBuf)> {
        // FSKit may send O_RDWR even for a read. A read-only inspection must
        // never copy lower files into the persistent upper merely by opening.
        let flags = if self.read_only {
            if flags & (libc::O_TRUNC | libc::O_APPEND) != 0 {
                return Err(io::Error::from_raw_os_error(libc::EROFS));
            }
            flags & !libc::O_ACCMODE
        } else {
            flags
        };
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let path = if writing {
            self.copy_up_inode_for_open(ino, flags)?
        } else {
            let path = self.node_path(ino)?;
            if !self.read_only {
                self.inode_for_path_with_metadata(path.clone())?;
            }
            path
        };
        self.open_path_with_backing(&path, flags)
    }

    #[cfg(test)]
    fn open_inode(&mut self, ino: u64, flags: i32) -> io::Result<File> {
        self.open_inode_with_backing(ino, flags)
            .map(|(file, _)| file)
    }

    pub(crate) fn with_observation(mut self, observation: Option<crate::api::FsMetrics>) -> Self {
        self.observation = observation;
        self
    }

    fn observe(&self, path: &Path, operation: &str, outcome: io::Result<u64>, mutating: bool) {
        if let Some(metrics) = &self.observation {
            let _span = self.profile.span("observation");
            let decision = self.access_policy.authorize(path);
            let rules = self.access_policy.matched_rule_ids(path);
            metrics.observe(
                path,
                operation,
                outcome.map_err(|error| error.raw_os_error().unwrap_or(libc::EIO)),
                mutating,
                decision,
                &rules,
            );
        }
    }

    fn observe_result<T>(
        &self,
        path: Option<&Path>,
        operation: &str,
        result: &io::Result<T>,
        bytes: u64,
        mutating: bool,
    ) {
        if let Some(path) = path {
            self.observe(
                path,
                operation,
                result
                    .as_ref()
                    .map(|_| bytes)
                    .map_err(|error| io::Error::from_raw_os_error(errno(error))),
                mutating,
            );
        }
    }

    pub(crate) fn from_core(core: OverlayCore) -> anyhow::Result<Self> {
        let mut root_paths = BTreeSet::new();
        root_paths.insert(PathBuf::new());
        let mut nodes = HashMap::new();
        nodes.insert(
            FUSE_ROOT_ID,
            Node {
                paths: root_paths,
                lookups: 0,
            },
        );
        let mut by_path = HashMap::new();
        by_path.insert(PathBuf::new(), FUSE_ROOT_ID);
        Ok(Self {
            core: Arc::new(FilesystemService::new(core)),
            pending_requests: 0,
            pending_bytes: 0,
            pending_inodes: HashMap::new(),
            profile: pvisor_overlay_core::profile::Profile::from_env("host-fuse"),
            read_only: false,
            private_root: false,
            access_policy: FileAccessPolicy::default(),
            observation: None,
            kernel_cache: KernelCacheConfig::default(),
            cache_transport: Arc::new(OnceLock::new()),
            _cache_locks: Vec::new(),
            #[cfg(test)]
            copy_fault_after: None,
            nodes,
            by_path,
            by_object: HashMap::new(),
            next_ino: FUSE_ROOT_ID + 1,
            open_files: HashMap::new(),
            open_directories: HashMap::new(),
            next_handle: 1,
        })
    }

    fn node_path(&self, ino: u64) -> io::Result<PathBuf> {
        self.nodes
            .get(&ino)
            .and_then(|node| node.paths.iter().next())
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn retain_lookup(&mut self, ino: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.lookups = node.lookups.saturating_add(1);
        }
    }

    fn reclaim_inode(&mut self, ino: u64) {
        let profile = self.profile.clone();
        let _span = profile.span("reclaim_inode");
        if self.pending_inodes.contains_key(&ino) {
            return;
        }
        if ino == FUSE_ROOT_ID || self.nodes.get(&ino).is_some_and(|node| node.lookups != 0) {
            return;
        }
        // Active file and directory handles pin their inodes.
        if self.open_files.values().any(|file| file.ino == ino)
            || self
                .open_directories
                .values()
                .any(|entries| entries.iter().any(|entry| entry.ino == ino))
        {
            return;
        }
        self.profile
            .add("reclaim_paths_scanned", self.by_path.len() as u64);
        self.profile
            .add("reclaim_objects_scanned", self.by_object.len() as u64);
        self.nodes.remove(&ino);
        self.by_path.retain(|_, value| *value != ino);
        self.by_object.retain(|_, value| *value != ino);
    }

    pub(super) fn is_directory_inode(&self, ino: u64) -> bool {
        self.inode_metadata(ino, None)
            .is_ok_and(|metadata| metadata.is_dir())
    }

    fn inode_metadata(&self, ino: u64, fh: Option<u64>) -> io::Result<fs::Metadata> {
        if let Some(fh) = fh {
            return self
                .open_files
                .get(&fh)
                .filter(|file| file.ino == ino)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?
                .metadata();
        }
        if let Ok(path) = self.node_path(ino) {
            // Recursive materialization need not have copied every known alias.
            // The lexicographically first node path can still be a lower alias.
            return match self.core.copied_hard_link_metadata(&path)? {
                Some(metadata) => Ok(metadata),
                None => self.core.metadata(&path),
            };
        }
        self.open_files
            .values()
            .find(|file| file.ino == ino)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?
            .metadata()
    }

    fn inode_for_path_with_metadata(&mut self, path: PathBuf) -> io::Result<(u64, fs::Metadata)> {
        let mut metadata = self.core.metadata(&path)?;
        let key = ObjectKey {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Some(current) = self.core.copied_hard_link_metadata(&path)? {
            // Core's session-owned mapping outlives FORGET/reclaim. Neither the
            // presence of an adapter inode nor its canonical path is authority
            // for deciding whether this lower alias already has upper contents.
            let upper_key = ObjectKey {
                device: current.dev(),
                inode: current.ino(),
            };
            let known = self
                .by_object
                .get(&upper_key)
                .or_else(|| self.by_object.get(&key))
                .copied();
            let inodes: Vec<_> = known.into_iter().collect();
            let mutation = self.begin_mutation(&[Some(&path)], &inodes, false)?;
            let result = self.core.copy_up(&path).and_then(|_| {
                metadata = self.core.metadata(&path)?;
                if let Some(ino) = known {
                    self.by_object.insert(upper_key, ino);
                }
                let ino = self.allocate_inode(path.clone(), &metadata);
                Ok((ino, metadata.clone()))
            });
            self.mutation_reply(mutation, |_| {});
            return result;
        }
        let ino = self.allocate_inode(path, &metadata);
        Ok((ino, metadata))
    }

    fn allocate_inode(&mut self, path: PathBuf, metadata: &fs::Metadata) -> u64 {
        let object = (!metadata.is_dir()).then_some(ObjectKey {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        if let Some(ino) = self.by_path.get(&path).copied() {
            // A pathname may have been replaced since its last lookup. Track
            // single-link objects too: a surviving hardlink can now have nlink=1.
            if metadata.is_dir()
                || object.and_then(|key| self.by_object.get(&key).copied()) == Some(ino)
            {
                return ino;
            }
            self.remove_inode_prefix(&path);
        }
        if let Some(ino) = object.and_then(|key| self.by_object.get(&key).copied()) {
            self.add_inode_alias(ino, path);
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        let mut paths = BTreeSet::new();
        paths.insert(path.clone());
        self.nodes.insert(ino, Node { paths, lookups: 0 });
        self.by_path.insert(path, ino);
        if let Some(object) = object {
            self.by_object.insert(object, ino);
        }
        ino
    }

    fn add_inode_alias(&mut self, ino: u64, path: PathBuf) {
        self.by_path.insert(path.clone(), ino);
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.paths.insert(path);
        }
    }

    fn remove_inode_prefix(&mut self, prefix: &Path) {
        let paths: Vec<_> = self
            .by_path
            .keys()
            .filter(|path| *path == prefix || path.starts_with(prefix))
            .cloned()
            .collect();
        for path in paths {
            if let Some(ino) = self.by_path.remove(&path)
                && let Some(node) = self.nodes.get_mut(&ino)
            {
                node.paths.remove(&path);
                if node.paths.is_empty() {
                    self.by_object.retain(|_, value| *value != ino);
                }
                self.reclaim_inode(ino);
            }
        }
    }

    fn remap_inode_prefix(&mut self, old: &Path, new: &Path) {
        if old == new {
            return;
        }
        self.remove_inode_prefix(new);
        let mappings: Vec<_> = self
            .by_path
            .iter()
            .filter(|(path, _)| *path == old || path.starts_with(old))
            .map(|(path, ino)| (path.clone(), *ino))
            .collect();
        for (old_path, ino) in mappings {
            let suffix = old_path.strip_prefix(old).unwrap_or_else(|_| Path::new(""));
            let new_path = if suffix.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(suffix)
            };
            self.by_path.remove(&old_path);
            self.by_path.insert(new_path.clone(), ino);
            if let Some(node) = self.nodes.get_mut(&ino) {
                node.paths.remove(&old_path);
                node.paths.insert(new_path.clone());
            }
            for file in self
                .open_files
                .values_mut()
                .filter(|file| file.ino == ino && file.path == old_path)
            {
                file.path = new_path.clone();
            }
        }
    }

    fn exchange_inode_prefixes(&mut self, first: &Path, second: &Path) {
        let mappings: Vec<_> = self
            .by_path
            .iter()
            .filter_map(|(path, ino)| {
                if path == first || path.starts_with(first) {
                    let suffix = path.strip_prefix(first).ok()?;
                    Some((path.clone(), *ino, second.join(suffix)))
                } else if path == second || path.starts_with(second) {
                    let suffix = path.strip_prefix(second).ok()?;
                    Some((path.clone(), *ino, first.join(suffix)))
                } else {
                    None
                }
            })
            .collect();
        for (old, ino, _) in &mappings {
            self.by_path.remove(old);
            if let Some(node) = self.nodes.get_mut(ino) {
                node.paths.remove(old);
            }
        }
        for file in self.open_files.values_mut() {
            if let Some((_, _, new)) = mappings
                .iter()
                .find(|(old, ino, _)| *ino == file.ino && *old == file.path)
            {
                file.path = new.clone();
            }
        }
        for (_, ino, new) in mappings {
            self.by_path.insert(new.clone(), ino);
            if let Some(node) = self.nodes.get_mut(&ino) {
                node.paths.insert(new);
            }
        }
    }

    fn allocate_handle(&mut self) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        handle
    }

    fn attr_from_metadata(&self, ino: u64, metadata: &fs::Metadata) -> FileAttr {
        let mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
        let atime = metadata.accessed().unwrap_or(mtime);
        let ctime = sys::unix_time(metadata.ctime(), metadata.ctime_nsec());
        #[cfg(target_os = "macos")]
        let flags = {
            use std::os::macos::fs::MetadataExt as MacMetadataExt;
            MacMetadataExt::st_flags(metadata)
        };
        #[cfg(not(target_os = "macos"))]
        let flags = 0;
        FileAttr {
            ino,
            size: metadata.len(),
            blocks: metadata.blocks(),
            atime,
            mtime,
            ctime,
            crtime: metadata.created().unwrap_or(ctime),
            kind: file_type(metadata),
            // FSKit has no request credentials to enforce fuser's owner ACL.
            // Keep its root private using the OS file permission check instead.
            perm: if self.private_root && ino == FUSE_ROOT_ID {
                0o700
            } else {
                (metadata.mode() & 0o7777) as u16
            },
            nlink: metadata.nlink().min(u32::MAX as u64) as u32,
            uid: if self.private_root && ino == FUSE_ROOT_ID {
                unsafe { libc::geteuid() }
            } else {
                metadata.uid()
            },
            gid: metadata.gid(),
            rdev: metadata.rdev() as u32,
            blksize: metadata.blksize().min(u32::MAX as u64) as u32,
            flags,
        }
    }

    fn attr(&self, ino: u64, path: &Path) -> io::Result<FileAttr> {
        Ok(self.attr_from_metadata(ino, &self.core.metadata(path)?))
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> io::Result<PathBuf> {
        OverlayCore::child(&self.node_path(parent)?, name)
    }

    fn copy_up_inode(&mut self, ino: u64) -> io::Result<PathBuf> {
        self.copy_up_inode_for_open(ino, 0)
    }

    fn copy_up_inode_for_open(&mut self, ino: u64, flags: i32) -> io::Result<PathBuf> {
        let path = self.node_path(ino)?;
        let aliases = self
            .nodes
            .get(&ino)
            .map(|node| node.paths.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![path.clone()]);
        for alias in aliases {
            if flags & libc::O_TRUNC != 0 {
                self.core.prepare_open(&alias, flags)?;
            } else {
                self.core.copy_up(&alias)?;
            }
        }
        let upper = self.core.upper_path(&path);
        let copied = fs::symlink_metadata(&upper)?;
        if !copied.is_dir() {
            self.by_object.insert(
                ObjectKey {
                    device: copied.dev(),
                    inode: copied.ino(),
                },
                ino,
            );
        }
        for file in self.open_files.values_mut().filter(|file| file.ino == ino) {
            let metadata = file.metadata()?;
            if metadata.dev() == copied.dev() && metadata.ino() == copied.ino() {
                continue;
            }
            let access = file.flags & libc::O_ACCMODE;
            let replacement = OpenOptions::new()
                .read(access != libc::O_WRONLY)
                .write(access != libc::O_RDONLY)
                .append(file.flags & libc::O_APPEND != 0)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&upper)?;
            let offset = sys::seek(file, 0, libc::SEEK_CUR)?;
            sys::seek(&replacement, offset, libc::SEEK_SET)?;
            file.file = replacement;
        }
        Ok(path)
    }

    fn directory_snapshot(&mut self, ino: u64) -> io::Result<Vec<DirectoryEntry>> {
        let profile = self.profile.clone();
        let _span = profile.span("directory_snapshot");
        let path = self.node_path(ino)?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
        let parent_ino = self
            .by_path
            .get(parent_path)
            .copied()
            .unwrap_or(FUSE_ROOT_ID);
        let mut entries = vec![
            DirectoryEntry {
                ino,
                kind: FileType::Directory,
                name: OsString::from("."),
                attr: None,
            },
            DirectoryEntry {
                ino: parent_ino,
                kind: FileType::Directory,
                name: OsString::from(".."),
                attr: None,
            },
        ];
        for (name, type_) in self.core.directory_candidates(&path)? {
            let child = OverlayCore::child(&path, &name)?;
            let kind = if type_ == u32::from(libc::DT_UNKNOWN) {
                file_type(&self.core.metadata(&child)?)
            } else {
                directory_file_type(type_)
            };
            entries.push(DirectoryEntry {
                // FUSE permits unknown inode numbers in plain READDIR. Assign
                // object-aware inodes only when LOOKUP/READDIRPLUS needs them.
                ino: self.by_path.get(&child).copied().unwrap_or(0),
                kind,
                name,
                attr: None,
            });
        }
        Ok(entries)
    }

    fn directory_entry_attr(&mut self, fh: u64, index: usize) -> io::Result<FileAttr> {
        let entry = &self.open_directories[&fh][index];

        let previous_ino = entry.ino;
        let name = entry.name.clone();
        // The first snapshot entry pins the handle-owning directory inode.
        // Follow its current path after rename/exchange, never an old pathname
        // that could now identify an unrelated replacement directory.
        let owner = self.open_directories[&fh][0].ino;
        let directory = self.node_path(owner)?;
        let path = match index {
            0 => directory,
            1 => directory
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf(),
            _ => OverlayCore::child(&directory, &name)?,
        };
        let (ino, metadata) = if index == 0 {
            (owner, self.core.metadata(&path)?)
        } else {
            self.inode_for_path_with_metadata(path)?
        };
        let attr = self.attr_from_metadata(ino, &metadata);
        let entry = &mut self.open_directories.get_mut(&fh).unwrap()[index];
        entry.ino = ino;
        entry.kind = attr.kind;
        // Retain the last materialization for diagnostics, never reuse it as a
        // fresh reply: handle snapshots pin names/cookies, not attributes.
        entry.attr = Some(attr);
        // Replace the entry's handle pin before attempting to reclaim the old
        // inode; other aliases, handles and lookup references still protect it.
        if previous_ino != ino {
            self.reclaim_inode(previous_ino);
        }
        self.profile.add("directory_attrs_loaded", 1);
        Ok(attr)
    }

    fn buffer_readdirplus(
        &mut self,
        fh: u64,
        offset: i64,
        mut add: impl FnMut(&DirectoryEntry, i64, &FileAttr) -> bool,
    ) -> io::Result<()> {
        if offset < 0 {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let count = self
            .open_directories
            .get(&fh)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?
            .len();
        let mut delivered = Vec::new();
        for index in offset as usize..count {
            let attr = match self.directory_entry_attr(fh, index) {
                Ok(attr) => attr,
                Err(error) if errno(&error) == libc::ENOENT => continue,
                // reply.error discards the entire buffer, including children
                // added earlier. None of them acquired a kernel lookup reference.
                Err(error) => return Err(error),
            };
            let entry = &self.open_directories[&fh][index];
            if add(entry, (index + 1) as i64, &attr) {
                break;
            }
            if index >= 2 {
                delivered.push(entry.ino);
            }
        }
        for ino in delivered {
            self.retain_lookup(ino);
        }
        Ok(())
    }

    #[cfg(test)]
    fn open_path(&self, path: &Path, flags: i32) -> io::Result<File> {
        self.open_path_with_backing(path, flags)
            .map(|(file, _)| file)
    }

    fn open_path_with_backing(&self, path: &Path, flags: i32) -> io::Result<(File, PathBuf)> {
        let real = self.core.prepare_open(path, flags)?;
        let access_mode = flags & libc::O_ACCMODE;
        let mut options = OpenOptions::new();
        options
            .read(access_mode != libc::O_WRONLY)
            .write(access_mode != libc::O_RDONLY)
            .append(flags & libc::O_APPEND != 0)
            .truncate(flags & libc::O_TRUNC != 0)
            .custom_flags(
                libc::O_NOFOLLOW
                    | flags
                        & !(libc::O_ACCMODE
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_TRUNC
                            | libc::O_APPEND),
            );
        options.open(&real).map(|file| (file, real))
    }
}

// Keep handlers aligned with the native FUSE callback signatures.
#[allow(clippy::too_many_arguments)]
impl OverlayFs {
    pub(super) fn forget(&mut self, ino: u64, nlookup: u64) {
        let profile = self.profile.clone();
        let _span = profile.span("forget");
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.lookups = node.lookups.saturating_sub(nlookup);
        }
        self.reclaim_inode(ino);
    }
    pub(super) fn lookup(&mut self, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let profile = self.profile.clone();
        let _span = profile.span("lookup");
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.inode_for_path_with_metadata(path)
        })();
        self.observe_result(observed_path.as_deref(), "lookup", &result, 0, false);
        match result {
            Ok((ino, metadata)) => {
                self.retain_lookup(ino);
                reply.entry(
                    &self.cache_ttl(),
                    &self.attr_from_metadata(ino, &metadata),
                    0,
                )
            }
            Err(error) if errno(&error) == libc::ENOENT && !self.negative_ttl().is_zero() => {
                // Inode zero is FUSE's negative entry representation. Attribute
                // payload is ignored, but the parent must still be resolvable.
                match self
                    .node_path(parent)
                    .and_then(|path| self.attr(parent, &path))
                {
                    Ok(mut attr) => {
                        attr.ino = 0;
                        reply.entry(&self.negative_ttl(), &attr, 0);
                    }
                    Err(_) => reply.error(libc::ENOENT),
                }
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn getattr(&mut self, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        let profile = self.profile.clone();
        let _span = profile.span("getattr");
        let observed_path = self.node_path(ino).ok();
        let result = self
            .inode_metadata(ino, fh)
            .map(|metadata| self.attr_from_metadata(ino, &metadata));
        self.observe_result(observed_path.as_deref(), "getattr", &result, 0, false);
        match result {
            Ok(attr) => reply.attr(&self.cache_ttl(), &attr),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn setattr(
        &mut self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("setattr");
        let observed_path = self.node_path(ino).ok();
        let mutating = setattr_requires_copy_up(mode, uid, gid, size, atime, mtime, flags);
        let mutation = if mutating {
            mutation_or_reply!(
                self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
                reply
            )
        } else {
            None
        };
        let result = (|| {
            if fh.is_some_and(|handle| {
                !self
                    .open_files
                    .get(&handle)
                    .is_some_and(|file| file.ino == ino)
            }) {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if mutating && self.read_only {
                return Err(io::Error::from_raw_os_error(libc::EROFS));
            }
            if mutating && self.node_path(ino).is_ok() {
                self.copy_up_inode(ino)?;
            }
            if let Some(file) = fh
                .and_then(|fh| self.open_files.get(&fh).filter(|file| file.ino == ino))
                .or_else(|| {
                    self.node_path(ino)
                        .is_err()
                        .then(|| self.open_files.values().find(|file| file.ino == ino))
                        .flatten()
                })
            {
                if mutating {
                    sys::set_file_metadata(
                        file,
                        sys::FileMetadataUpdate {
                            mode,
                            uid,
                            gid,
                            size,
                            atime: atime.map(time_value),
                            mtime: mtime.map(time_value),
                            flags,
                        },
                    )?;
                }
                return file
                    .metadata()
                    .map(|metadata| self.attr_from_metadata(ino, &metadata));
            }
            if fh.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            let path = self.node_path(ino)?;
            // macFUSE can report a read-induced atime update through SETATTR.
            // Overlay views are mounted noatime, and an atime-only request must
            // not turn every file read into a full lower-to-upper copy-up.
            if !setattr_requires_copy_up(mode, uid, gid, size, atime, mtime, flags) {
                return self.attr(ino, &path);
            }
            let upper = self.core.prepare_metadata_change(&path)?;
            if let Some(size) = size {
                OpenOptions::new().write(true).open(&upper)?.set_len(size)?;
            }
            if let Some(mode) = mode {
                fs::set_permissions(&upper, fs::Permissions::from_mode(mode & 0o7777))?;
            }
            if uid.is_some() || gid.is_some() {
                let metadata = fs::symlink_metadata(&upper)?;
                sys::chown(
                    &upper,
                    uid.unwrap_or_else(|| metadata.uid()),
                    gid.unwrap_or_else(|| metadata.gid()),
                    metadata.file_type().is_symlink(),
                )?;
            }
            if atime.is_some() || mtime.is_some() {
                let nofollow = fs::symlink_metadata(&upper)?.file_type().is_symlink();
                sys::set_times(
                    &upper,
                    atime.map(time_value),
                    mtime.map(time_value),
                    nofollow,
                )?;
            }
            if let Some(flags) = flags {
                #[cfg(target_os = "macos")]
                sys::set_flags(&upper, flags)?;
                #[cfg(not(target_os = "macos"))]
                if flags != 0 {
                    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
                }
            }
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "setattr", &result, 0, mutating);
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(attr) => reply.attr(&ttl, &attr),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn readlink(&mut self, ino: u64, reply: ReplyData) {
        let profile = self.profile.clone();
        let _span = profile.span("readlink");
        let observed_path = self.node_path(ino).ok();
        let result = self.node_path(ino).and_then(|path| {
            let backing = self.core.observe_read_resolved(&path)?;
            fs::read_link(backing.resolved.path)
        });
        self.observe_result(observed_path.as_deref(), "readlink", &result, 0, false);
        match result {
            Ok(target) => reply.data(target.as_os_str().as_encoded_bytes()),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn mknod(
        &mut self,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("mknod");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], false),
            reply
        );
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_node(&path, mode & !umask, rdev)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "mknod", &result, 0, true);
        if let Ok(attr) = &result {
            self.retain_lookup(attr.ino);
        }
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(attr) => reply.entry(&ttl, &attr, 0),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn mkdir(
        &mut self,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("mkdir");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], true),
            reply
        );
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_dir(&path, mode & !umask)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "mkdir", &result, 0, true);
        if let Ok(attr) = &result {
            self.retain_lookup(attr.ino);
        }
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(attr) => reply.entry(&ttl, &attr, 0),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn unlink(&mut self, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("unlink");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], false),
            reply
        );
        let result = self.child_path(parent, name).and_then(|path| {
            if let Some(ino) = self.by_path.get(&path).copied()
                && self.open_files.values().any(|file| file.ino == ino)
            {
                self.copy_up_inode(ino)?;
            }
            self.core.remove(&path, false).map(|()| path)
        });
        self.observe_result(observed_path.as_deref(), "unlink", &result, 0, true);
        if let Ok(path) = &result {
            self.remove_inode_prefix(path);
        }
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(_) => {
                    reply.ok();
                }
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn rmdir(&mut self, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("rmdir");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], true),
            reply
        );
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.core.remove(&path, true).map(|()| path));
        self.observe_result(observed_path.as_deref(), "rmdir", &result, 0, true);
        if let Ok(path) = &result {
            self.remove_inode_prefix(path);
        }
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(_) => {
                    reply.ok();
                }
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn symlink(&mut self, parent: u64, name: &OsStr, target: &Path, reply: ReplyEntry) {
        let profile = self.profile.clone();
        let _span = profile.span("symlink");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], false),
            reply
        );
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_symlink(&path, target)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "symlink", &result, 0, true);
        if let Ok(attr) = &result {
            self.retain_lookup(attr.ino);
        }
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(attr) => reply.entry(&ttl, &attr, 0),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn rename(
        &mut self,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("rename");
        let old_path = self.child_path(parent, name).ok();
        let new_path = self.child_path(newparent, newname).ok();
        if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0
            || flags == (RENAME_NOREPLACE | RENAME_EXCHANGE)
        {
            if let Some(path) = old_path.as_deref() {
                self.observe(
                    path,
                    "rename_from",
                    Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
                    false,
                );
            }
            reply.error(libc::ENOTSUP);
            return;
        }
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[old_path.as_deref(), new_path.as_deref()], &[], true),
            reply
        );
        let result = (|| {
            let old = self.child_path(parent, name)?;
            let new = self.child_path(newparent, newname)?;
            // Pin open objects to upper storage before rename can hide their lower paths.
            for prefix in [&old, &new] {
                let inodes: BTreeSet<_> = self
                    .by_path
                    .iter()
                    .filter(|(path, ino)| {
                        path.starts_with(prefix)
                            && self.open_files.values().any(|file| file.ino == **ino)
                    })
                    .map(|(_, ino)| *ino)
                    .collect();
                for ino in inodes {
                    self.copy_up_inode(ino)?;
                }
            }
            if flags & RENAME_EXCHANGE != 0 {
                self.core.exchange(&old, &new)?;
                Ok((old, new, true))
            } else {
                self.core
                    .rename(&old, &new, flags & RENAME_NOREPLACE != 0)?;
                Ok((old, new, false))
            }
        })();
        self.observe_result(old_path.as_deref(), "rename_from", &result, 0, true);
        self.observe_result(new_path.as_deref(), "rename_to", &result, 0, true);
        if let Ok((old, new, exchange)) = &result {
            if *exchange {
                self.exchange_inode_prefixes(old, new);
            } else {
                self.remap_inode_prefix(old, new);
            }
        }
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(_) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn link(&mut self, ino: u64, newparent: u64, newname: &OsStr, reply: ReplyEntry) {
        let profile = self.profile.clone();
        let _span = profile.span("link");
        let observed_path = self.child_path(newparent, newname).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
            reply
        );
        let result = (|| {
            let source = self.copy_up_inode(ino)?;
            let destination = self.child_path(newparent, newname)?;
            self.core.hard_link(&source, &destination)?;
            self.add_inode_alias(ino, destination.clone());
            self.attr(ino, &destination)
        })();
        self.observe_result(observed_path.as_deref(), "link", &result, 0, true);
        if let Ok(attr) = &result {
            self.retain_lookup(attr.ino);
        }
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(attr) => reply.entry(&ttl, &attr, 0),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn open(&mut self, ino: u64, flags: i32, reply: ReplyOpen) {
        let profile = self.profile.clone();
        let _span = profile.span("open");
        let observed_path = self.node_path(ino).ok();
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_TRUNC | libc::O_APPEND) != 0;
        let mutation = if writing {
            mutation_or_reply!(
                self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
                reply
            )
        } else {
            None
        };
        let result = self
            .open_inode_with_backing(ino, flags)
            .and_then(|(file, backing)| {
                let cache_flags = self.cache_open_flags(&file.metadata()?);
                Ok((file, backing, cache_flags))
            });
        self.observe_result(
            observed_path.as_deref(),
            "open",
            &result,
            0,
            flags & libc::O_TRUNC != 0,
        );
        let result = result.map(|(file, backing, cache_flags)| {
            let handle = self.allocate_handle();
            self.open_files.insert(
                handle,
                OpenFile {
                    file,
                    backing,
                    ino,
                    path: observed_path.unwrap_or_default(),
                    flags,
                },
            );
            (handle, cache_flags)
        });
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok((handle, cache_flags)) => reply.opened(handle, cache_flags),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn read(
        &mut self,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("read");
        let observed_path = self
            .open_files
            .get(&fh)
            .map(|file| file.path.clone())
            .or_else(|| self.node_path(ino).ok());
        if offset < 0 {
            self.observe_result(
                observed_path.as_deref(),
                "read",
                &Err::<(), _>(io::Error::from_raw_os_error(libc::EINVAL)),
                0,
                false,
            );
            reply.error(libc::EINVAL);
            return;
        }
        let Some(file) = self.open_files.get(&fh) else {
            self.observe_result(
                observed_path.as_deref(),
                "read",
                &Err::<(), _>(io::Error::from_raw_os_error(libc::EBADF)),
                0,
                false,
            );
            reply.error(libc::EBADF);
            return;
        };
        let mut data = vec![0; size as usize];
        let result = self
            .core
            .read_at(&file.backing, &file.file, &mut data, offset as u64);
        self.observe_result(
            observed_path.as_deref(),
            "read",
            &result,
            result.as_ref().copied().unwrap_or(0) as u64,
            false,
        );
        match result {
            Ok(read) => {
                data.truncate(read);
                reply.data(&data);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn write(
        &mut self,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("write");
        let observed_path = self
            .open_files
            .get(&fh)
            .map(|file| file.path.clone())
            .or_else(|| self.node_path(ino).ok());
        if offset < 0 {
            self.observe_result(
                observed_path.as_deref(),
                "write",
                &Err::<(), _>(io::Error::from_raw_os_error(libc::EINVAL)),
                0,
                false,
            );
            reply.error(libc::EINVAL);
            return;
        }
        let Some(file) = self.open_files.get(&fh) else {
            self.observe_result(
                observed_path.as_deref(),
                "write",
                &Err::<(), _>(io::Error::from_raw_os_error(libc::EBADF)),
                0,
                false,
            );
            reply.error(libc::EBADF);
            return;
        };
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
            reply
        );
        let result = write_request(file, data, offset as u64, flags);
        self.observe_result(
            observed_path.as_deref(),
            "write",
            &result,
            result.as_ref().copied().unwrap_or(0) as u64,
            true,
        );
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(written) => reply.written(written as u32),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn flush(&mut self, _ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("flush");
        if self.open_files.contains_key(&fh) {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    pub(super) fn release(
        &mut self,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("release");
        if let Some(file) = self.open_files.remove(&fh) {
            self.reclaim_inode(file.ino);
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    pub(super) fn fsync(&mut self, _ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("fsync");
        match self.open_files.get(&fh) {
            Some(file) => match self
                .core
                .sync_preimages()
                .and_then(|()| sys::fsync(file, datasync))
            {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            },
            None => reply.error(libc::EBADF),
        }
    }

    pub(super) fn opendir(&mut self, ino: u64, _flags: i32, reply: ReplyOpen) {
        let profile = self.profile.clone();
        let _span = profile.span("opendir");
        let observed_path = self.node_path(ino).ok();
        let result = self.directory_snapshot(ino);
        self.observe_result(observed_path.as_deref(), "opendir", &result, 0, false);
        match result {
            Ok(entries) => {
                let handle = self.allocate_handle();
                self.open_directories.insert(handle, entries);
                reply.opened(handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn readdir(&mut self, _ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let profile = self.profile.clone();
        let _span = profile.span("readdir");
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(entry.ino, (index + 1) as i64, entry.kind, &entry.name) {
                break;
            }
        }
        reply.ok();
    }

    pub(super) fn readdirplus(
        &mut self,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("readdirplus");
        let ttl = self.cache_ttl();
        let result = self.buffer_readdirplus(fh, offset, |entry, cookie, attr| {
            reply.add(entry.ino, cookie, &entry.name, &ttl, attr, 0)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn releasedir(&mut self, _ino: u64, fh: u64, _flags: i32, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("releasedir");
        if let Some(entries) = self.open_directories.remove(&fh) {
            for entry in entries {
                self.reclaim_inode(entry.ino);
            }
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    pub(super) fn fsyncdir(&mut self, ino: u64, _fh: u64, datasync: bool, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("fsyncdir");
        let result = self.node_path(ino).and_then(|path| {
            self.core.sync_preimages()?;
            let resolved = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            let directory = File::open(resolved.path)?;
            sys::fsync(&directory, datasync)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn statfs(&mut self, _ino: u64, reply: ReplyStatfs) {
        let profile = self.profile.clone();
        let _span = profile.span("statfs");
        match sys::statfs(self.core.upper()) {
            Ok(stat) => reply.statfs(
                stat.blocks,
                stat.bfree,
                stat.bavail,
                stat.files,
                stat.ffree,
                stat.bsize,
                stat.namelen,
                stat.frsize,
            ),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn setxattr(
        &mut self,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("setxattr");
        let observed_path = self.node_path(ino).ok();
        if position != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
            reply
        );
        let result = pvisor_overlay_core::validate_guest_xattr(name)
            .and_then(|()| self.copy_up_inode(ino))
            .and_then(|path| self.core.prepare_metadata_change(&path))
            .and_then(|path| sys::set_xattr(&path, name, value, flags));
        self.observe_result(
            observed_path.as_deref(),
            "setxattr",
            &result,
            value.len() as u64,
            true,
        );
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn getxattr(&mut self, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        let profile = self.profile.clone();
        let _span = profile.span("getxattr");
        let observed_path = self.node_path(ino).ok();
        let result = self.node_path(ino).and_then(|path| {
            let backing = self.core.observe_read_resolved(&path)?;
            sys::get_xattr(&backing.resolved.path, name)
        });
        self.observe_result(observed_path.as_deref(), "getxattr", &result, 0, false);
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn listxattr(&mut self, ino: u64, size: u32, reply: ReplyXattr) {
        let profile = self.profile.clone();
        let _span = profile.span("listxattr");
        let result = self.node_path(ino).and_then(|path| {
            let backing = self.core.observe_read_resolved(&path)?;
            let names = sys::list_xattrs(&backing.resolved.path)?;
            let mut encoded = Vec::new();
            for name in names {
                encoded.extend_from_slice(&name);
                encoded.push(0);
            }
            Ok(encoded)
        });
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn removexattr(&mut self, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("removexattr");
        let observed_path = self.node_path(ino).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
            reply
        );
        let result = pvisor_overlay_core::validate_guest_xattr(name)
            .and_then(|()| self.copy_up_inode(ino))
            .and_then(|path| self.core.prepare_metadata_change(&path))
            .and_then(|path| sys::remove_xattr(&path, name));
        self.observe_result(observed_path.as_deref(), "removexattr", &result, 0, true);
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn access(&mut self, ino: u64, mask: i32, reply: ReplyEmpty) {
        let profile = self.profile.clone();
        let _span = profile.span("access");
        let observed_path = self.node_path(ino).ok();
        let result = self.node_path(ino).and_then(|path| {
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            sys::access(&real.path, mask)
        });
        self.observe_result(observed_path.as_deref(), "access", &result, 0, false);
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    pub(super) fn create(
        &mut self,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("create");
        let observed_path = self.child_path(parent, name).ok();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[], false),
            reply
        );
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let file = self.core.create_file(&path, mode & !umask, flags)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            let attr = self.attr(ino, &path)?;
            Ok((file, attr))
        })();
        self.observe_result(observed_path.as_deref(), "create", &result, 0, true);
        let result = result.map(|(file, attr)| {
            let handle = self.allocate_handle();
            self.retain_lookup(attr.ino);
            self.open_files.insert(
                handle,
                OpenFile {
                    file,
                    ino: attr.ino,
                    backing: self
                        .core
                        .upper_path(observed_path.as_deref().unwrap_or_else(|| Path::new(""))),
                    path: observed_path.unwrap_or_default(),
                    flags,
                },
            );
            (attr, handle)
        });
        let ttl = self.cache_ttl();
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok((attr, handle)) => reply.created(&ttl, &attr, 0, handle, 0),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn fallocate(
        &mut self,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("fallocate");
        let observed_path = self
            .open_files
            .get(&fh)
            .map(|file| file.path.clone())
            .or_else(|| self.node_path(ino).ok());
        if offset < 0 || length < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        if mode != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
        let Some(file) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[observed_path.as_deref()], &[ino], false),
            reply
        );
        let result = sys::allocate(file, offset, length);
        self.observe_result(observed_path.as_deref(), "fallocate", &result, 0, true);
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    pub(super) fn lseek(
        &mut self,
        _ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("lseek");
        match self.open_files.get(&fh) {
            Some(file) => match sys::seek(file, offset, whence) {
                Ok(offset) => reply.offset(offset),
                Err(error) => reply.error(errno(&error)),
            },
            None => reply.error(libc::EBADF),
        }
    }

    pub(super) fn copy_file_range(
        &mut self,
        _ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        _ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        let profile = self.profile.clone();
        let _span = profile.span("copy_file_range");
        if offset_in < 0 || offset_out < 0 || flags != 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(input) = self
            .open_files
            .get(&fh_in)
            .and_then(|file| file.try_clone().ok())
        else {
            reply.error(libc::EBADF);
            return;
        };
        let Some(output) = self
            .open_files
            .get(&fh_out)
            .and_then(|file| file.try_clone().ok())
        else {
            reply.error(libc::EBADF);
            return;
        };
        let output_path = self.open_files.get(&fh_out).map(|file| file.path.clone());
        let output_ino = self.open_files.get(&fh_out).map(|file| file.ino).unwrap();
        let mutation = mutation_or_reply!(
            self.begin_mutation(&[output_path.as_deref()], &[output_ino], false),
            reply
        );
        // The FUSE reply count is u32. Never modify bytes outside the range
        // reported to the kernel, including after a short write / later error.
        // Linux rejects copy_file_range while the mounted target has O_APPEND.
        // Unlike WRITE this opcode carries no current open flags. A received
        // request is positional: clear any stale backing OPEN/WRITE append bit
        // before touching bytes (F_SETFL may have cleared it without a WRITE).
        let result = set_append(&output, false).and_then(|()| {
            copy_range(
                len.min(u32::MAX as u64),
                |buffer, copied| input.read_at(buffer, offset_in as u64 + copied),
                |buffer, copied| {
                    #[cfg(test)]
                    let buffer = if let Some(limit) = self.copy_fault_after {
                        if copied >= limit {
                            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
                        }
                        &buffer[..buffer.len().min((limit - copied) as usize)]
                    } else {
                        buffer
                    };
                    output.write_at(buffer, offset_out as u64 + copied)
                },
            )
        });
        self.mutation_reply(mutation, move |valid| {
            if !valid {
                reply.error(libc::EIO);
                return;
            }
            match result {
                Ok(copied) => reply.written(copied as u32),
                Err(error) => reply.error(errno(&error)),
            }
        });
    }

    #[cfg(target_os = "macos")]
    pub(super) fn exchange(
        &mut self,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        self.rename(parent, name, newparent, newname, RENAME_EXCHANGE, reply);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn getxtimes(&mut self, ino: u64, reply: ReplyXTimes) {
        let profile = self.profile.clone();
        let _span = profile.span("getxtimes");
        let result = self
            .node_path(ino)
            .and_then(|path| self.core.metadata(&path));
        match result {
            Ok(metadata) => {
                let created = metadata.created().unwrap_or(UNIX_EPOCH);
                reply.xtimes(created, created);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn queued_requests_pin_only_their_referenced_inodes() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("held"), b"held").unwrap();
        fs::write(lower.join("unrelated"), b"other").unwrap();
        let mut fs = OverlayFs::new(vec![lower], root.path().join("upper"), None).unwrap();
        let held = fs.inode_for_path_with_metadata("held".into()).unwrap().0;
        let unrelated = fs
            .inode_for_path_with_metadata("unrelated".into())
            .unwrap()
            .0;
        fs.retain_lookup(held);
        fs.retain_lookup(unrelated);
        fs.pending_requests = 1;
        fs.pending_inodes.insert(held, 1);
        fs.forget(held, 1);
        fs.forget(unrelated, 1);
        assert!(fs.nodes.contains_key(&held));
        assert!(!fs.nodes.contains_key(&unrelated));
        fs.finish_request(0, &[held]);
        assert!(!fs.nodes.contains_key(&held));
    }

    #[test]
    fn prepared_copy_rebinds_open_handles_and_aliases_added_during_preparation() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("file"), b"original").unwrap();
        fs::hard_link(lower.join("file"), lower.join("alias")).unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], root.path().join("upper"), None).unwrap();
        let ino = overlay
            .inode_for_path_with_metadata("file".into())
            .unwrap()
            .0;
        let (file, backing) = overlay
            .open_inode_with_backing(ino, libc::O_RDONLY)
            .unwrap();
        sys::seek(&file, 3, libc::SEEK_SET).unwrap();
        overlay.open_files.insert(
            1,
            OpenFile {
                file,
                ino,
                backing,
                flags: libc::O_RDONLY,
                path: "file".into(),
            },
        );
        let (core, plan) = overlay
            .copy_plan(&crate::dispatch::CopyRequest::Open {
                ino,
                flags: libc::O_RDWR,
            })
            .unwrap();
        let crate::dispatch::CopyPlan::Files(paths, truncate) = plan else {
            panic!("file plan")
        };
        let copies = core.prepare_copy_ups(&paths, truncate).unwrap();
        assert!(!root.path().join("upper/file").exists());
        assert_eq!(
            overlay
                .inode_for_path_with_metadata("alias".into())
                .unwrap()
                .0,
            ino
        );
        let guard = core.use_prepared_copy_ups(copies).unwrap();
        overlay.copy_up_inode(ino).unwrap();
        assert_eq!(
            sys::seek(&overlay.open_files[&1], 0, libc::SEEK_CUR).unwrap(),
            3
        );
        fs::write(root.path().join("upper/file"), b"updated").unwrap();
        let mut bytes = [0; 7];
        overlay.open_files[&1].read_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"updated");
        assert_eq!(
            overlay.inode_metadata(ino, Some(1)).unwrap().ino(),
            fs::metadata(root.path().join("upper/alias")).unwrap().ino()
        );
        assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
        drop(guard);
    }

    #[test]
    fn late_lower_alias_binds_upper_before_attributes_and_open() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"abcd").unwrap();
        fs::hard_link(lower.join("a"), lower.join("z")).unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], root.path().join("upper"), None).unwrap();
        let (ino, _) = overlay.inode_for_path_with_metadata("a".into()).unwrap();
        let file = overlay
            .open_inode(ino, libc::O_WRONLY | libc::O_APPEND)
            .unwrap();
        // macOS pwrite honors the offset even with O_APPEND.
        write_request(&file, b"efgh", 4, libc::O_APPEND).unwrap();
        assert!(!overlay.by_path.contains_key(Path::new("z")));
        let (alias, metadata) = overlay.inode_for_path_with_metadata("z".into()).unwrap();
        assert_eq!(alias, ino);
        assert_eq!(metadata.len(), 8);
        assert_eq!(metadata.ino(), file.metadata().unwrap().ino());
        let reader = overlay.open_inode(alias, libc::O_RDONLY).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(reader.read_at(&mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"abcdefgh");
        assert_eq!(fs::read(lower.join("z")).unwrap(), b"abcd");
    }

    #[test]
    fn late_lower_alias_after_forget_reclaim_uses_core_owned_upper_identity() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"abcd").unwrap();
        fs::hard_link(lower.join("a"), lower.join("z")).unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], root.path().join("upper"), None).unwrap();
        let (old, _) = overlay.inode_for_path_with_metadata("a".into()).unwrap();
        overlay.retain_lookup(old);
        let appender = overlay
            .open_inode(old, libc::O_WRONLY | libc::O_APPEND)
            .unwrap();
        // macOS pwrite honors the offset even with O_APPEND.
        write_request(&appender, b"efgh", 4, libc::O_APPEND).unwrap();
        drop(appender);
        // Apply the final FORGET's accounting and the same reclaim helper used
        // by the callback. No live handle, inode or by_object entry survives.
        overlay.nodes.get_mut(&old).unwrap().lookups -= 1;
        overlay.reclaim_inode(old);
        assert!(!overlay.nodes.contains_key(&old));
        assert!(overlay.by_object.is_empty());
        assert!(!overlay.by_path.contains_key(Path::new("z")));
        let (alias, metadata) = overlay.inode_for_path_with_metadata("z".into()).unwrap();
        assert_ne!(
            alias, old,
            "reclaimed protocol inode must not be resurrected"
        );
        assert_eq!(metadata.len(), 8);
        let reader = overlay.open_inode(alias, libc::O_RDONLY).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(reader.read_at(&mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"abcdefgh");
        assert_eq!(fs::read(lower.join("z")).unwrap(), b"abcd");
        let (source, source_metadata) = overlay.inode_for_path_with_metadata("a".into()).unwrap();
        assert_eq!(source, alias);
        assert_eq!(source_metadata.ino(), metadata.ino());
    }

    #[test]
    fn recursive_materialization_reads_upper_despite_canonical_lower_alias() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        let upper = root.path().join("upper");
        fs::create_dir_all(lower.join("dir")).unwrap();
        fs::write(lower.join("dir/child"), b"abcd").unwrap();
        fs::hard_link(lower.join("dir/child"), lower.join("a-outside")).unwrap();
        let mut overlay = OverlayFs::new(vec![lower.clone()], upper.clone(), None).unwrap();
        let (ino, _) = overlay
            .inode_for_path_with_metadata("a-outside".into())
            .unwrap();
        assert_eq!(
            overlay
                .inode_for_path_with_metadata("dir/child".into())
                .unwrap()
                .0,
            ino
        );
        overlay
            .core
            .rename(Path::new("dir"), Path::new("moved"), false)
            .unwrap();
        overlay.remap_inode_prefix(Path::new("dir"), Path::new("moved"));
        assert_eq!(overlay.node_path(ino).unwrap(), Path::new("a-outside"));
        assert!(!upper.join("a-outside").exists());
        // Model modification of the already materialized upper child. Neither
        // its lower alias nor adapter by_object has been rebound by this step.
        fs::write(upper.join("moved/child"), b"abcdefgh").unwrap();
        assert_eq!(overlay.inode_metadata(ino, None).unwrap().len(), 8);
        let reader = overlay.open_inode(ino, libc::O_RDONLY).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(reader.read_at(&mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"abcdefgh");
        assert_eq!(overlay.by_path[Path::new("a-outside")], ino);
        assert_eq!(
            reader.metadata().unwrap().ino(),
            fs::metadata(upper.join("moved/child")).unwrap().ino()
        );
        assert_eq!(fs::read(lower.join("a-outside")).unwrap(), b"abcd");
    }

    #[test]
    fn copy_range_acknowledges_partial_write_read_error_and_write_zero() {
        for zero in [false, true] {
            let mut target = Vec::new();
            let count = copy_range(
                8,
                |buffer, _| {
                    buffer.fill(b'x');
                    Ok(buffer.len())
                },
                |buffer, offset| {
                    if offset != 0 {
                        return if zero {
                            Ok(0)
                        } else {
                            Err(io::Error::from_raw_os_error(libc::ENOSPC))
                        };
                    }
                    target.extend_from_slice(&buffer[..3]);
                    Ok(3)
                },
            )
            .unwrap();
            assert_eq!(count, 3);
            assert_eq!(target, b"xxx");
        }
        let count = copy_range(
            256 * 1024,
            |buffer, offset| {
                if offset != 0 {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                buffer.fill(b'x');
                Ok(buffer.len())
            },
            |buffer, _| Ok(buffer.len()),
        )
        .unwrap();
        assert_eq!(count, 128 * 1024);
        assert_eq!(
            copy_range(
                8,
                |_, _| Err(io::Error::from_raw_os_error(libc::EIO)),
                |_, _| unreachable!()
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EIO)
        );
        assert_eq!(
            copy_range(
                8,
                |buffer, _| Ok(buffer.len()),
                |_, _| Err(io::Error::from_raw_os_error(libc::ENOSPC))
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::ENOSPC)
        );
    }

    #[test]
    fn mutation_subtree_effects_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        let mut overlay = OverlayFs::new(vec![lower], root.path().join("upper"), None).unwrap();
        for index in 0..EFFECT_LIMIT + 20 {
            overlay
                .by_path
                .insert(PathBuf::from(format!("tree/{index}")), index as u64 + 2);
        }
        let mut effects = Effects::default();
        let objects = overlay.extend_effects(&mut effects, &["tree".into()], &[], true);
        assert!(effects.overflow);
        assert!(objects.len() <= EFFECT_LIMIT);
        assert!(effects.entries.len() + effects.inodes.len() <= EFFECT_LIMIT + 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires real HOST FUSE and writable fusectl abort"]
    fn host_kernel_cache_reclaimed_late_alias_first_lookup_and_read() {
        use crate::cache::CacheWorkers;
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let lower = root.path().join("lower");
        let upper = root.path().join("upper");
        let mount = root.path().join("merged");
        for path in [&lower, &upper, &mount] {
            fs::create_dir(path).unwrap();
        }
        fs::write(lower.join("a"), b"abcd").unwrap();
        fs::hard_link(lower.join("a"), lower.join("z")).unwrap();
        let future = SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60);
        for path in [&lower, &upper, &lower.join("a")] {
            File::open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_accessed(future))
                .unwrap();
        }
        let atime = fs::metadata(lower.join("a")).unwrap().accessed().unwrap();
        let mut filesystem = OverlayFs::new(vec![lower.clone()], upper, None).unwrap();
        let (old, _) = filesystem.inode_for_path_with_metadata("a".into()).unwrap();
        filesystem.retain_lookup(old);
        let appender = filesystem
            .open_inode(old, libc::O_WRONLY | libc::O_APPEND)
            .unwrap();
        write_request(&appender, b"efgh", 0, libc::O_APPEND).unwrap();
        drop(appender);
        filesystem.nodes.get_mut(&old).unwrap().lookups -= 1;
        filesystem.reclaim_inode(old);
        assert!(!filesystem.nodes.contains_key(&old));
        assert!(filesystem.by_object.is_empty());
        // Start the real kernel adapter with deterministically reclaimed state.
        // This avoids relying on Linux's discretionary dcache/inode eviction;
        // every kernel lookup below is after source-close and final reclaim.
        let filesystem = filesystem.with_kernel_cache(
            KernelCacheConfig {
                policy: KernelCachePolicy::Metadata,
                ..Default::default()
            },
            vec![],
        );
        let slot = filesystem.cache_slot();
        let session = fuser::Session::new(
            filesystem,
            &mount,
            &[
                fuser::MountOption::DefaultPermissions,
                fuser::MountOption::NoAtime,
            ],
        )
        .unwrap();
        let abort = crate::mount::connection_abort_file(&mount).unwrap();
        let (handle, workers) = CacheWorkers::start(session.notifier(), abort, &mount).unwrap();
        assert!(slot.set(handle).is_ok());
        drop(slot);
        let background = fuser::BackgroundSession::new(session).unwrap();
        assert_eq!(fs::metadata(mount.join("z")).unwrap().len(), 8);
        assert_eq!(fs::read(mount.join("z")).unwrap(), b"abcdefgh");
        assert_eq!(
            fs::metadata(mount.join("z")).unwrap().ino(),
            fs::metadata(mount.join("a")).unwrap().ino()
        );
        assert_eq!(fs::read(lower.join("z")).unwrap(), b"abcd");
        assert_eq!(
            fs::metadata(lower.join("a")).unwrap().accessed().unwrap(),
            atime
        );
        workers.shutdown();
        background.unmount().unwrap();
        workers.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires real HOST FUSE and writable fusectl abort"]
    fn host_kernel_cache_partial_copy_error_updates_warmed_target_pages() {
        use crate::cache::CacheWorkers;
        use std::os::fd::AsRawFd;
        // Private per-instance fault injection, no process-global environment.
        for limit in [0, 3, 128 * 1024 + 3] {
            let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let lower = root.path().join("lower");
            let upper = root.path().join("upper");
            let mount = root.path().join("merged");
            for path in [&lower, &upper, &mount] {
                fs::create_dir(path).unwrap();
            }
            let bytes = vec![b'x'; 256 * 1024];
            fs::write(upper.join("input"), &bytes).unwrap();
            fs::write(upper.join("target"), vec![b'o'; bytes.len()]).unwrap();
            fs::hard_link(upper.join("target"), upper.join("alias")).unwrap();
            let future = SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60);
            for path in [&upper, &upper.join("input"), &upper.join("target")] {
                File::open(path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_accessed(future))
                    .unwrap();
            }
            let input_atime = fs::metadata(upper.join("input"))
                .unwrap()
                .accessed()
                .unwrap();
            let mut filesystem = OverlayFs::new(vec![lower], upper.clone(), None)
                .unwrap()
                .with_kernel_cache(
                    KernelCacheConfig {
                        policy: KernelCachePolicy::Metadata,
                        ..Default::default()
                    },
                    vec![],
                );
            filesystem.copy_fault_after = Some(limit);
            let slot = filesystem.cache_slot();
            let session = fuser::Session::new(
                filesystem,
                &mount,
                &[
                    fuser::MountOption::DefaultPermissions,
                    fuser::MountOption::NoAtime,
                ],
            )
            .unwrap();
            let abort = crate::mount::connection_abort_file(&mount).unwrap();
            let (handle, workers) = CacheWorkers::start(session.notifier(), abort, &mount).unwrap();
            assert!(slot.set(handle).is_ok());
            drop(slot);
            let background = fuser::BackgroundSession::new(session).unwrap();
            let input = File::open(mount.join("input")).unwrap();
            let output = OpenOptions::new()
                .write(true)
                .open(mount.join("target"))
                .unwrap();
            let cached = File::open(mount.join("alias")).unwrap();
            let mut warm = vec![0; bytes.len()];
            assert_eq!(cached.read_at(&mut warm, 0).unwrap(), bytes.len());
            assert!(warm.iter().all(|byte| *byte == b'o'));
            let mut src = 0;
            let mut dst = 0;
            let copied = unsafe {
                libc::copy_file_range(
                    input.as_raw_fd(),
                    &mut src,
                    output.as_raw_fd(),
                    &mut dst,
                    bytes.len(),
                    0,
                )
            };
            if limit == 0 {
                assert_eq!(copied, -1);
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ENOSPC)
                );
            } else {
                assert_eq!(copied, limit as isize);
            }
            assert_eq!(cached.read_at(&mut warm, 0).unwrap(), bytes.len());
            assert!(warm[..limit as usize].iter().all(|byte| *byte == b'x'));
            assert!(warm[limit as usize..].iter().all(|byte| *byte == b'o'));
            assert_eq!(fs::read(upper.join("target")).unwrap(), warm);
            assert_eq!(
                fs::metadata(upper.join("input"))
                    .unwrap()
                    .accessed()
                    .unwrap(),
                input_atime,
                "fault fixture must preserve backing atime"
            );
            drop((input, output, cached));
            workers.shutdown();
            background.unmount().unwrap();
            workers.join().unwrap();
        }
    }

    #[test]
    fn mutation_effects_cover_aliases_parents_and_subtrees_without_sibling_eviction() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir_all(lower.join("dir/sub")).unwrap();
        fs::create_dir_all(lower.join("other")).unwrap();
        fs::write(lower.join("dir/a"), b"a").unwrap();
        fs::hard_link(lower.join("dir/a"), lower.join("other/alias")).unwrap();
        fs::write(lower.join("dir/sub/child"), b"child").unwrap();
        fs::write(lower.join("other/unrelated"), b"unrelated").unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        for path in [
            "dir",
            "dir/sub",
            "dir/a",
            "dir/sub/child",
            "other",
            "other/alias",
            "other/unrelated",
        ] {
            let path = PathBuf::from(path);
            overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        }
        let dir = overlay.by_path[Path::new("dir")];
        let other = overlay.by_path[Path::new("other")];
        let a = overlay.by_path[Path::new("dir/a")];
        let child = overlay.by_path[Path::new("dir/sub/child")];
        let unrelated = overlay.by_path[Path::new("other/unrelated")];
        let mut effects = Effects::default();
        let objects = overlay.extend_effects(&mut effects, &["dir/a".into()], &[], false);
        assert_eq!(objects, BTreeSet::from([a]));
        assert_eq!(
            effects.inodes,
            BTreeSet::from([FUSE_ROOT_ID, dir, other, a])
        );
        assert_eq!(
            effects.entries,
            BTreeSet::from([(dir, "a".into()), (other, "alias".into())])
        );
        assert!(!effects.inodes.contains(&child));
        assert!(!effects.inodes.contains(&unrelated));
        let mut renamed = Effects::default();
        let _ = overlay.extend_effects(&mut renamed, &["dir".into(), "moved".into()], &[], true);
        assert!(renamed.inodes.contains(&child));
        assert!(renamed.entries.contains(&(FUSE_ROOT_ID, "dir".into())));
        assert!(renamed.entries.contains(&(FUSE_ROOT_ID, "moved".into())));
        assert!(renamed.entries.contains(&(other, "alias".into())));
        assert!(!renamed.entries.contains(&(FUSE_ROOT_ID, "other".into())));
        assert!(!renamed.inodes.contains(&unrelated));
    }

    #[test]
    fn replacement_with_denied_hardlink_is_rejected_before_content_observation() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let stage = temp.path().join("stage");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("private"), b"private contents").unwrap();
        fs::write(target.join("allowed"), b"public contents").unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            Some(stage.join("preimages")),
        )
        .unwrap();
        let mut overlay = OverlayFs::from_core(core).unwrap().with_access_policy(
            &pvisor_overlay_core::FileAccessPolicy::new(vec!["private".into()], vec![]).unwrap(),
        );
        let path = PathBuf::from("allowed");
        let ino = overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        fs::remove_file(target.join("allowed")).unwrap();
        fs::hard_link(target.join("private"), target.join("allowed")).unwrap();
        assert_eq!(
            overlay
                .open_inode(ino, libc::O_RDONLY)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(
            pvisor_overlay_core::load_preimages(&stage.join("preimages"))
                .unwrap()
                .is_empty()
        );
        assert!(!stage.join("upper/allowed").exists());
    }

    fn snapshot_handle(overlay: &mut OverlayFs, ino: u64) -> u64 {
        let entries = overlay.directory_snapshot(ino).unwrap();
        let fh = overlay.allocate_handle();
        overlay.open_directories.insert(fh, entries);
        fh
    }

    #[test]
    fn deferred_plus_rebinds_recreated_hardlink_alias_and_pins_current_inode() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"original").unwrap();
        fs::hard_link(lower.join("a"), lower.join("b")).unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        let old = overlay.allocate_inode(
            PathBuf::from("a"),
            &overlay.core.metadata(Path::new("a")).unwrap(),
        );
        assert_eq!(
            overlay.allocate_inode(
                PathBuf::from("b"),
                &overlay.core.metadata(Path::new("b")).unwrap()
            ),
            old
        );
        let fh = snapshot_handle(&mut overlay, FUSE_ROOT_ID);
        overlay.core.remove(Path::new("a"), false).unwrap();
        overlay.remove_inode_prefix(Path::new("a"));
        overlay
            .core
            .create_file(Path::new("a"), 0o600, libc::O_WRONLY)
            .unwrap()
            .write_at(b"new", 0)
            .unwrap();
        let new = overlay.allocate_inode(
            PathBuf::from("a"),
            &overlay.core.metadata(Path::new("a")).unwrap(),
        );
        assert_ne!(new, old);
        let a = overlay.directory_entry_attr(fh, 2).unwrap();
        let b = overlay.directory_entry_attr(fh, 3).unwrap();
        assert_eq!(a.ino, new);
        assert_eq!(a.size, 3);
        assert_eq!(b.ino, old);
        assert_eq!(b.size, 8);
        overlay.reclaim_inode(new);
        overlay.reclaim_inode(old);
        assert!(overlay.nodes.contains_key(&new));
        assert!(overlay.nodes.contains_key(&old));
        overlay.open_directories.remove(&fh);
        overlay.reclaim_inode(new);
        overlay.reclaim_inode(old);
        assert!(!overlay.nodes.contains_key(&new));
        assert!(!overlay.nodes.contains_key(&old));
    }

    #[test]
    fn deferred_plus_validates_known_path_against_fresh_object_identity() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"original").unwrap();
        fs::hard_link(lower.join("a"), lower.join("b")).unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], temp.path().join("upper"), None).unwrap();
        let old = overlay.allocate_inode(
            PathBuf::from("a"),
            &overlay.core.metadata(Path::new("a")).unwrap(),
        );
        overlay.allocate_inode(
            PathBuf::from("b"),
            &overlay.core.metadata(Path::new("b")).unwrap(),
        );
        let fh = snapshot_handle(&mut overlay, FUSE_ROOT_ID);
        // External changes do not update by_path through FUSE's unlink/create.
        fs::remove_file(lower.join("a")).unwrap();
        fs::write(lower.join("a"), b"new").unwrap();
        let a = overlay.directory_entry_attr(fh, 2).unwrap();
        let b = overlay.directory_entry_attr(fh, 3).unwrap();
        assert_ne!(a.ino, old);
        assert_eq!(b.ino, old);
        assert_eq!(overlay.nodes[&old].paths, [PathBuf::from("b")].into());
        assert_eq!(overlay.by_path[Path::new("a")], a.ino);
        assert_eq!(a.size, 3);
        assert_eq!(b.size, 8);
    }

    #[test]
    fn deferred_plus_does_not_rebind_replaced_directory_handle() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir_all(lower.join("source")).unwrap();
        fs::create_dir(lower.join("destination")).unwrap();
        fs::write(lower.join("source/file"), b"source").unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        let source = overlay.allocate_inode(
            PathBuf::from("source"),
            &overlay.core.metadata(Path::new("source")).unwrap(),
        );
        let destination = overlay.allocate_inode(
            PathBuf::from("destination"),
            &overlay.core.metadata(Path::new("destination")).unwrap(),
        );
        let source_fh = snapshot_handle(&mut overlay, source);
        let destination_fh = snapshot_handle(&mut overlay, destination);
        overlay
            .core
            .rename(Path::new("source"), Path::new("destination"), false)
            .unwrap();
        overlay.remap_inode_prefix(Path::new("source"), Path::new("destination"));
        assert_eq!(
            errno(&overlay.directory_entry_attr(destination_fh, 0).unwrap_err()),
            libc::ENOENT
        );
        assert!(overlay.nodes.contains_key(&destination));
        assert_eq!(
            overlay.directory_entry_attr(source_fh, 0).unwrap().ino,
            source
        );
        assert_eq!(overlay.directory_entry_attr(source_fh, 2).unwrap().size, 6);
        overlay.open_directories.remove(&destination_fh);
        overlay.reclaim_inode(destination);
        assert!(!overlay.nodes.contains_key(&destination));
    }

    #[test]
    fn deferred_plus_follows_directory_rename_not_recreated_old_path() {
        for recreate in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            fs::create_dir_all(lower.join("dir")).unwrap();
            fs::write(lower.join("dir/file"), b"original").unwrap();
            let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
            let ino = overlay.allocate_inode(
                PathBuf::from("dir"),
                &overlay.core.metadata(Path::new("dir")).unwrap(),
            );
            let fh = snapshot_handle(&mut overlay, ino);
            overlay
                .core
                .rename(Path::new("dir"), Path::new("moved"), false)
                .unwrap();
            overlay.remap_inode_prefix(Path::new("dir"), Path::new("moved"));
            if recreate {
                overlay.core.create_dir(Path::new("dir"), 0o700).unwrap();
                overlay
                    .core
                    .create_file(Path::new("dir/file"), 0o600, libc::O_WRONLY)
                    .unwrap()
                    .write_at(b"unrelated replacement", 0)
                    .unwrap();
            }
            assert_eq!(overlay.directory_entry_attr(fh, 0).unwrap().ino, ino);
            let attr = overlay.directory_entry_attr(fh, 2).unwrap();
            assert_eq!(attr.size, 8);
            assert_eq!(
                overlay.node_path(attr.ino).unwrap(),
                Path::new("moved/file")
            );
        }
    }

    #[test]
    fn deferred_plus_follows_both_exchanged_directory_inodes() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        for (name, bytes) in [
            ("first", b"one".as_slice()),
            ("second", b"second contents".as_slice()),
        ] {
            fs::create_dir_all(lower.join(name)).unwrap();
            fs::write(lower.join(name).join("file"), bytes).unwrap();
        }
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        let first = overlay.allocate_inode(
            PathBuf::from("first"),
            &overlay.core.metadata(Path::new("first")).unwrap(),
        );
        let second = overlay.allocate_inode(
            PathBuf::from("second"),
            &overlay.core.metadata(Path::new("second")).unwrap(),
        );
        let first_fh = snapshot_handle(&mut overlay, first);
        let second_fh = snapshot_handle(&mut overlay, second);
        overlay
            .core
            .exchange(Path::new("first"), Path::new("second"))
            .unwrap();
        overlay.exchange_inode_prefixes(Path::new("first"), Path::new("second"));
        let a = overlay.directory_entry_attr(first_fh, 2).unwrap();
        let b = overlay.directory_entry_attr(second_fh, 2).unwrap();
        assert_eq!(a.size, 3);
        assert_eq!(b.size, 15);
        assert_eq!(overlay.node_path(a.ino).unwrap(), Path::new("second/file"));
        assert_eq!(overlay.node_path(b.ino).unwrap(), Path::new("first/file"));
    }

    #[test]
    fn deferred_plus_error_discards_buffer_without_retaining_lookups() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"allowed").unwrap();
        fs::write(lower.join("b"), b"later denied").unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        let fh = snapshot_handle(&mut overlay, FUSE_ROOT_ID);
        overlay =
            overlay.with_access_policy(&FileAccessPolicy::new(vec!["b".into()], vec![]).unwrap());
        let mut buffered = Vec::new();
        let error = overlay
            .buffer_readdirplus(fh, 2, |entry, cookie, attr| {
                buffered.push((entry.name.clone(), cookie, attr.ino));
                false
            })
            .unwrap_err();
        assert_eq!(errno(&error), libc::EACCES);
        assert_eq!(buffered.len(), 1);
        let ino = buffered[0].2;
        // The callback sends reply.error, which discards these buffered entries.
        assert_eq!(overlay.nodes[&ino].lookups, 0);
        overlay.open_directories.remove(&fh);
        overlay.reclaim_inode(ino);
        assert!(!overlay.nodes.contains_key(&ino));
    }

    #[test]
    fn deferred_plus_success_retains_only_children_accepted_by_buffer() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"a").unwrap();
        fs::write(lower.join("b"), b"b").unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        let fh = snapshot_handle(&mut overlay, FUSE_ROOT_ID);
        let mut visited = Vec::new();
        overlay
            .buffer_readdirplus(fh, 0, |entry, cookie, attr| {
                visited.push((cookie, attr.ino));
                entry.name == OsStr::new("b")
            })
            .unwrap();
        assert_eq!(
            visited.iter().map(|e| e.0).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(overlay.nodes[&visited[0].1].lookups, 0);
        assert_eq!(overlay.nodes[&visited[2].1].lookups, 1);
        assert_eq!(overlay.nodes[&visited[3].1].lookups, 0);
        overlay.buffer_readdirplus(fh, 3, |_, _, _| false).unwrap();
        assert_eq!(overlay.nodes[&visited[3].1].lookups, 1);
    }

    #[test]
    fn directory_snapshot_is_lazy_stable_and_pins_materialized_inodes() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"first").unwrap();
        fs::hard_link(lower.join("a"), lower.join("b")).unwrap();
        fs::write(lower.join("hidden"), b"hidden").unwrap();
        let core = OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap();
        core.remove(Path::new("hidden"), false).unwrap();
        let mut overlay = OverlayFs::from_core(core).unwrap();
        let entries = overlay.directory_snapshot(FUSE_ROOT_ID).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            [".", "..", "a", "b"].map(OsString::from)
        );
        assert!(entries.iter().all(|e| e.attr.is_none()));
        assert!(entries[2..].iter().all(|e| e.ino == 0));
        assert!(!overlay.by_path.contains_key(Path::new("a")));
        let fh = overlay.allocate_handle();
        overlay.open_directories.insert(fh, entries);
        fs::write(lower.join("a"), b"updated before plus").unwrap();
        fs::write(lower.join("new"), b"new").unwrap();
        let a = overlay.directory_entry_attr(fh, 2).unwrap();
        let b = overlay.directory_entry_attr(fh, 3).unwrap();
        assert_eq!(a.size, 19);
        assert_eq!(a.ino, b.ino);
        assert_eq!(overlay.open_directories[&fh].len(), 4);
        overlay.reclaim_inode(a.ino);
        assert!(overlay.nodes.contains_key(&a.ino));
        fs::remove_file(lower.join("a")).unwrap();
        assert_eq!(
            errno(&overlay.directory_entry_attr(fh, 2).unwrap_err()),
            libc::ENOENT
        );
        overlay.open_directories.remove(&fh);
        overlay.reclaim_inode(a.ino);
        assert!(!overlay.nodes.contains_key(&a.ino));
    }

    #[test]
    fn lazy_directory_snapshot_hides_denied_hardlinks_and_opaque_lower_children() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("private"), b"secret").unwrap();
        fs::hard_link(lower.join("private"), lower.join("alias")).unwrap();
        fs::create_dir(lower.join("dir")).unwrap();
        fs::write(lower.join("dir/old"), b"old").unwrap();
        let policy = FileAccessPolicy::new(vec!["private".into()], vec![]).unwrap();
        let core = OverlayCore::new(vec![lower], upper.clone(), None)
            .unwrap()
            .with_access_policy(&policy);
        fs::create_dir(upper.join("dir")).unwrap();
        fs::write(upper.join("dir/.wh..wh..opq"), b"").unwrap();
        fs::write(upper.join("dir/new"), b"new").unwrap();
        let mut overlay = OverlayFs::from_core(core).unwrap();
        let root = overlay.directory_snapshot(FUSE_ROOT_ID).unwrap();
        assert_eq!(
            root.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            [".", "..", "dir"].map(OsString::from)
        );
        let path = PathBuf::from("dir");
        let ino = overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        let dir = overlay.directory_snapshot(ino).unwrap();
        assert_eq!(
            dir.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            [".", "..", "new"].map(OsString::from)
        );
        assert!(dir.iter().all(|e| e.attr.is_none()));
    }

    #[test]
    fn fuse_truncate_avoids_content_copy_and_preserves_lower() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("file"), b"original").unwrap();
        let profile = pvisor_overlay_core::profile::Profile::enabled("fuse-truncate");
        let core = OverlayCore::new(vec![lower.clone()], temp.path().join("upper"), None)
            .unwrap()
            .with_profile(profile.clone());
        let mut overlay = OverlayFs::from_core(core).unwrap();
        let path = PathBuf::from("file");
        let ino = overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        let file = overlay
            .open_inode(ino, libc::O_RDWR | libc::O_TRUNC)
            .unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
        assert_eq!(
            profile.report().unwrap().measurements["copy_up_truncate_skipped_bytes"].units,
            8
        );
    }

    #[test]
    fn fuse_open_inode_preserves_the_first_read_before_copy_up() {
        use pvisor_overlay_core::apply::{
            OverlayRecord, OverlayState, OverlayUpper, apply_overlay,
        };
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let stage = temp.path().join("stage");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("value"), b"original").unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            Some(stage.join("preimages")),
        )
        .unwrap();
        let mut overlay = OverlayFs::from_core(core).unwrap();
        let path = PathBuf::from("value");
        let ino = overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        // Real FUSE open callback delegates to open_inode/open_path. A plain
        // lookup must not hash/write a content observation.
        assert!(
            pvisor_overlay_core::load_preimages(&stage.join("preimages"))
                .unwrap()
                .is_empty()
        );
        let input = overlay.open_inode(ino, libc::O_RDONLY).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(input.read_at(&mut bytes, 0).unwrap(), 8);
        assert_eq!(&bytes, b"original");
        fs::write(target.join("value"), b"host edit").unwrap();
        let output = overlay
            .open_inode(ino, libc::O_WRONLY | libc::O_TRUNC)
            .unwrap();
        output.write_at(b"agent edit", 0).unwrap();
        drop(output);
        drop(input);
        drop(overlay);
        let mut record = OverlayRecord {
            id: "fuse-read".into(),
            generation: 0,
            target: target.clone(),
            baseline_lower: None,
            upper: OverlayUpper {
                upper_dir: stage.join("upper"),
                work_dir: stage.join("work"),
            },
            merged_dir: stage.join("merged"),
            stage_dir: stage,
            excluded_paths: vec![],
            access_policy: Default::default(),
            auto_apply: false,
            auto_discard: false,
            protect_target: false,
            state: OverlayState::Staged,
        };
        assert!(apply_overlay(&mut record).is_err());
        assert_eq!(fs::read(target.join("value")).unwrap(), b"host edit");
    }

    #[test]
    fn newly_discovered_upper_hardlink_keeps_the_existing_fuse_inode() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"original").unwrap();
        fs::hard_link(lower.join("a"), lower.join("b")).unwrap();
        let mut overlay = OverlayFs::new(vec![lower], dir.path().join("upper"), None).unwrap();
        let metadata = overlay.core.metadata(Path::new("a")).unwrap();
        let ino = overlay.allocate_inode(PathBuf::from("a"), &metadata);
        overlay.copy_up_inode(ino).unwrap();
        overlay.core.copy_up(Path::new("b")).unwrap();
        let metadata = overlay.core.metadata(Path::new("b")).unwrap();
        assert_eq!(overlay.allocate_inode(PathBuf::from("b"), &metadata), ino);
    }

    #[test]
    fn detached_open_file_keeps_metadata_and_pins_its_inode_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("file"), b"original").unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], dir.path().join("upper"), None).unwrap();
        let path = PathBuf::from("file");
        let ino = overlay.allocate_inode(path.clone(), &overlay.core.metadata(&path).unwrap());
        overlay.retain_lookup(ino);
        let (file, backing) = overlay
            .open_inode_with_backing(ino, libc::O_RDONLY)
            .unwrap();
        overlay.open_files.insert(
            1,
            OpenFile {
                file,
                backing,
                ino,
                path: path.clone(),
                flags: libc::O_RDONLY,
            },
        );
        overlay.copy_up_inode(ino).unwrap();
        overlay.core.remove(&path, false).unwrap();
        overlay.remove_inode_prefix(&path);
        overlay.nodes.get_mut(&ino).unwrap().lookups = 0;
        overlay.reclaim_inode(ino);
        assert!(overlay.nodes.contains_key(&ino));
        assert_eq!(overlay.inode_metadata(ino, Some(1)).unwrap().len(), 8);
        assert!(overlay.inode_metadata(ino, Some(2)).is_err());
        assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
        overlay.open_files.remove(&1);
        overlay.reclaim_inode(ino);
        assert!(!overlay.nodes.contains_key(&ino));
        assert!(!overlay.by_object.values().any(|value| *value == ino));
    }
    use super::*;

    #[test]
    fn fskit_private_root_uses_mount_owner_without_changing_the_lower() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::set_permissions(&lower, fs::Permissions::from_mode(0o755)).unwrap();
        let overlay = OverlayFs::new(vec![lower.clone()], dir.path().join("upper"), None)
            .unwrap()
            .with_private_root(true);
        let attr = overlay.attr(FUSE_ROOT_ID, Path::new("")).unwrap();
        assert_eq!(attr.perm, 0o700);
        assert_eq!(attr.uid, unsafe { libc::geteuid() });
        assert_eq!(fs::metadata(lower).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn open_cannot_follow_a_link_around_file_rules() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        fs::create_dir_all(lower.join(".ssh")).unwrap();
        fs::write(lower.join(".ssh/id_rsa"), b"dummy-private").unwrap();
        fs::write(lower.join(".env"), b"warn-only").unwrap();
        std::os::unix::fs::symlink(".ssh/id_rsa", lower.join("alias")).unwrap();
        let overlay = OverlayFs::new(vec![lower], dir.path().join("upper"), None)
            .unwrap()
            .with_access_policy(
                &pvisor_overlay_core::FileAccessPolicy::new(
                    vec!["**/.ssh".into()],
                    vec!["**/.env".into()],
                )
                .unwrap(),
            );
        assert!(
            overlay
                .open_path(Path::new(".ssh/id_rsa"), libc::O_RDONLY)
                .is_err()
        );
        assert!(
            overlay
                .open_path(Path::new("alias"), libc::O_RDONLY)
                .is_err()
        );
        assert!(
            overlay
                .open_path(Path::new("alias"), libc::O_WRONLY)
                .is_err()
        );
        assert!(overlay.open_path(Path::new(".env"), libc::O_RDONLY).is_ok());
    }

    #[test]
    fn readonly_inspection_accepts_fskit_open_without_staging_or_writing() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        let upper = dir.path().join("upper");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("file"), b"original").unwrap();
        let mut overlay = OverlayFs::new(vec![lower.clone()], upper.clone(), None)
            .unwrap()
            .with_read_only(true);
        let metadata = overlay.core.metadata(Path::new("file")).unwrap();
        let ino = overlay.allocate_inode("file".into(), &metadata);
        let file = overlay.open_inode(ino, libc::O_RDWR).unwrap();
        let mut contents = [0; 8];
        assert_eq!(file.read_at(&mut contents, 0).unwrap(), 8);
        assert_eq!(&contents, b"original");
        assert!(file.write_at(b"changed", 0).is_err());
        assert!(!upper.join("file").exists());
        assert!(
            overlay
                .open_inode(ino, libc::O_RDWR | libc::O_TRUNC)
                .is_err()
        );
        assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
    }

    #[test]
    fn repeated_plus_refreshes_attributes_after_copyup_truncate_chmod_and_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"original").unwrap();
        let mut overlay =
            OverlayFs::new(vec![lower.clone()], temp.path().join("upper"), None).unwrap();
        let fh = snapshot_handle(&mut overlay, FUSE_ROOT_ID);
        let first = overlay.directory_entry_attr(fh, 2).unwrap();
        assert_eq!(first.size, 8);
        let upper = overlay.copy_up_inode(first.ino).unwrap();
        let physical = overlay.core.upper_path(&upper);
        OpenOptions::new()
            .write(true)
            .open(&physical)
            .unwrap()
            .set_len(2)
            .unwrap();
        fs::set_permissions(&physical, fs::Permissions::from_mode(0o600)).unwrap();
        let updated = overlay.directory_entry_attr(fh, 2).unwrap();
        assert_eq!(updated.ino, first.ino);
        assert_eq!(updated.size, 2);
        assert_eq!(updated.perm, 0o600);
        assert_eq!(fs::read(lower.join("a")).unwrap(), b"original");
        // A held physical fd prevents inode-number reuse in this replacement test.
        let held = File::open(&physical).unwrap();
        overlay.core.remove(Path::new("a"), false).unwrap();
        overlay.remove_inode_prefix(Path::new("a"));
        overlay
            .core
            .create_file(Path::new("a"), 0o640, libc::O_WRONLY)
            .unwrap()
            .write_at(b"replacement", 0)
            .unwrap();
        let replaced = overlay.directory_entry_attr(fh, 2).unwrap();
        assert_ne!(replaced.ino, first.ino);
        assert_eq!(replaced.size, 11);
        drop(held);
    }

    #[test]
    fn cache_reply_policy_is_bounded_and_keep_cache_only_for_readonly_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("a"), b"stable").unwrap();
        let metadata = fs::metadata(lower.join("a")).unwrap();
        let directory = fs::metadata(&lower).unwrap();
        let mut overlay = OverlayFs::new(vec![lower], temp.path().join("upper"), None).unwrap();
        assert_eq!(overlay.cache_ttl(), Duration::from_secs(1));
        assert_eq!(overlay.negative_ttl(), Duration::ZERO);
        overlay.kernel_cache.policy = KernelCachePolicy::Uncached;
        assert_eq!(overlay.cache_ttl(), Duration::ZERO);
        assert_eq!(overlay.cache_open_flags(&metadata), 0);
        overlay.kernel_cache.policy = KernelCachePolicy::Metadata;
        overlay.read_only = true;
        assert_eq!(overlay.cache_ttl(), Duration::from_secs(60));
        assert_eq!(overlay.cache_open_flags(&metadata), 0);
        overlay.kernel_cache.policy = KernelCachePolicy::MetadataAndData;
        assert_eq!(
            overlay.cache_open_flags(&metadata),
            fuser::consts::FOPEN_KEEP_CACHE
        );
        assert_eq!(overlay.cache_open_flags(&directory), 0);
        overlay.read_only = false;
        assert_eq!(overlay.cache_open_flags(&metadata), 0);
    }

    #[test]
    fn atime_only_setattr_does_not_require_copy_up() {
        assert!(!setattr_requires_copy_up(
            None,
            None,
            None,
            None,
            Some(TimeOrNow::Now),
            None,
            None
        ));
        assert!(setattr_requires_copy_up(
            None,
            None,
            None,
            None,
            None,
            Some(TimeOrNow::Now),
            None
        ));
    }
}
