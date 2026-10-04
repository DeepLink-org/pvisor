#[cfg(target_os = "macos")]
use fuser::ReplyXTimes;
use fuser::{
    FUSE_ROOT_ID, FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyLseek, ReplyOpen, ReplyStatfs,
    ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use pvisor_core::overlay::FileAccessPolicy;
use pvisor_overlay_core::{OverlayCore, sys};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);
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
    attr: FileAttr,
}

pub struct OverlayFs {
    core: OverlayCore,
    read_only: bool,
    private_root: bool,
    access_policy: FileAccessPolicy,
    observation: Option<crate::FsMetrics>,
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

fn setattr_requires_copy_up(
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
    #[cfg(test)]
    pub fn new(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Self::from_core(OverlayCore::new(lowers, upper, work)?)
    }

    pub fn with_access_policy(mut self, policy: &pvisor_overlay_core::FileAccessPolicy) -> Self {
        self.core = self.core.with_access_policy(policy);
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

    fn open_inode(&mut self, ino: u64, flags: i32) -> io::Result<File> {
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
            self.copy_up_inode(ino)?
        } else {
            self.node_path(ino)?
        };
        self.open_path(&path, flags)
    }

    pub fn with_observation(mut self, observation: Option<crate::FsMetrics>) -> Self {
        self.observation = observation;
        self
    }

    fn observe(&self, path: &Path, operation: &str, outcome: io::Result<u64>, mutating: bool) {
        if let Some(metrics) = &self.observation {
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
            core,
            read_only: false,
            private_root: false,
            access_policy: FileAccessPolicy::default(),
            observation: None,
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
        if ino == FUSE_ROOT_ID || self.nodes.get(&ino).is_some_and(|node| node.lookups != 0) {
            return;
        }
        // ponytail: scan active handles on reclaim; index pins if handle counts make it costly.
        if self.open_files.values().any(|file| file.ino == ino)
            || self
                .open_directories
                .values()
                .any(|entries| entries.iter().any(|entry| entry.ino == ino))
        {
            return;
        }
        self.nodes.remove(&ino);
        self.by_path.retain(|_, value| *value != ino);
        self.by_object.retain(|_, value| *value != ino);
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
            return self.core.metadata(&path);
        }
        self.open_files
            .values()
            .find(|file| file.ino == ino)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?
            .metadata()
    }

    fn allocate_inode(&mut self, path: PathBuf, metadata: &fs::Metadata) -> u64 {
        if let Some(ino) = self.by_path.get(&path) {
            return *ino;
        }
        let object = (!metadata.is_dir() && metadata.nlink() > 1).then_some(ObjectKey {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
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
        let path = self.node_path(ino)?;
        let aliases = self
            .nodes
            .get(&ino)
            .map(|node| node.paths.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![path.clone()]);
        for alias in aliases {
            self.core.copy_up(&alias)?;
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
                attr: self.attr(ino, &path)?,
            },
            DirectoryEntry {
                ino: parent_ino,
                kind: FileType::Directory,
                name: OsString::from(".."),
                attr: self
                    .attr(parent_ino, parent_path)
                    .or_else(|_| self.attr(FUSE_ROOT_ID, Path::new("")))?,
            },
        ];
        for name in self.core.list_names(&path)? {
            let child = OverlayCore::child(&path, &name)?;
            let metadata = self.core.metadata(&child)?;
            let child_ino = self.allocate_inode(child, &metadata);
            entries.push(DirectoryEntry {
                ino: child_ino,
                kind: file_type(&metadata),
                name,
                attr: self.attr_from_metadata(child_ino, &metadata),
            });
        }
        Ok(entries)
    }

    fn open_path(&self, path: &Path, flags: i32) -> io::Result<File> {
        if self.core.metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let real = if writing {
            self.core.copy_up(path)?
        } else {
            self.core.observe_read(path)?;
            self.core
                .resolve(path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?
                .path
        };
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
        options.open(real)
    }
}

impl Filesystem for OverlayFs {
    fn forget(&mut self, _request: &Request<'_>, ino: u64, nlookup: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.lookups = node.lookups.saturating_sub(nlookup);
        }
        self.reclaim_inode(ino);
    }
    fn lookup(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path, &metadata);
            Ok((ino, metadata))
        })();
        self.observe_result(observed_path.as_deref(), "lookup", &result, 0, false);
        match result {
            Ok((ino, metadata)) => {
                self.retain_lookup(ino);
                reply.entry(&TTL, &self.attr_from_metadata(ino, &metadata), 0)
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn getattr(&mut self, _request: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        let observed_path = self.node_path(ino).ok();
        let result = self
            .inode_metadata(ino, fh)
            .map(|metadata| self.attr_from_metadata(ino, &metadata));
        self.observe_result(observed_path.as_deref(), "getattr", &result, 0, false);
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn setattr(
        &mut self,
        _request: &Request<'_>,
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
        let observed_path = self.node_path(ino).ok();
        let mutating = setattr_requires_copy_up(mode, uid, gid, size, atime, mtime, flags);
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
                    sys::set_file_metadata(file,
sys::FileMetadataUpdate { mode, uid, gid, size, atime: atime.map(time_value), mtime: mtime.map(time_value), flags },)?;
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
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn readlink(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyData) {
        let observed_path = self.node_path(ino).ok();
        let result = self.node_path(ino).and_then(|path| {
            self.core.observe_read(&path)?;
            let resolved = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            fs::read_link(resolved.path)
        });
        self.observe_result(observed_path.as_deref(), "readlink", &result, 0, false);
        match result {
            Ok(target) => reply.data(target.as_os_str().as_encoded_bytes()),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mknod(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_node(&path, mode & !umask, rdev)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "mknod", &result, 0, true);
        match result {
            Ok(attr) => {
                self.retain_lookup(attr.ino);
                reply.entry(&TTL, &attr, 0)
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mkdir(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_dir(&path, mode & !umask)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "mkdir", &result, 0, true);
        match result {
            Ok(attr) => {
                self.retain_lookup(attr.ino);
                reply.entry(&TTL, &attr, 0)
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn unlink(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let observed_path = self.child_path(parent, name).ok();
        let result = self.child_path(parent, name).and_then(|path| {
            if let Some(ino) = self.by_path.get(&path).copied()
                && self.open_files.values().any(|file| file.ino == ino)
            {
                self.copy_up_inode(ino)?;
            }
            self.core.remove(&path, false).map(|()| path)
        });
        self.observe_result(observed_path.as_deref(), "unlink", &result, 0, true);
        match result {
            Ok(path) => {
                self.remove_inode_prefix(&path);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rmdir(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let observed_path = self.child_path(parent, name).ok();
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.core.remove(&path, true).map(|()| path));
        self.observe_result(observed_path.as_deref(), "rmdir", &result, 0, true);
        match result {
            Ok(path) => {
                self.remove_inode_prefix(&path);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn symlink(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_symlink(&path, target)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        self.observe_result(observed_path.as_deref(), "symlink", &result, 0, true);
        match result {
            Ok(attr) => {
                self.retain_lookup(attr.ino);
                reply.entry(&TTL, &attr, 0)
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rename(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
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
        match result {
            Ok((old, new, exchange)) => {
                if exchange {
                    self.exchange_inode_prefixes(&old, &new);
                } else {
                    self.remap_inode_prefix(&old, &new);
                }
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn link(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let observed_path = self.child_path(newparent, newname).ok();
        let result = (|| {
            let source = self.copy_up_inode(ino)?;
            let destination = self.child_path(newparent, newname)?;
            self.core.hard_link(&source, &destination)?;
            self.add_inode_alias(ino, destination.clone());
            self.attr(ino, &destination)
        })();
        self.observe_result(observed_path.as_deref(), "link", &result, 0, true);
        match result {
            Ok(attr) => {
                self.retain_lookup(attr.ino);
                reply.entry(&TTL, &attr, 0)
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn open(&mut self, _request: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let observed_path = self.node_path(ino).ok();
        let result = self.open_inode(ino, flags);
        self.observe_result(
            observed_path.as_deref(),
            "open",
            &result,
            0,
            flags & libc::O_TRUNC != 0,
        );
        match result {
            Ok(file) => {
                let handle = self.allocate_handle();
                self.open_files.insert(
                    handle,
                    OpenFile {
                        file,
                        ino,
                        path: observed_path.unwrap_or_default(),
                        flags,
                    },
                );
                reply.opened(handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn read(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
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
        let result = file.read_at(&mut data, offset as u64);
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

    fn write(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
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
        let result = file.write_at(data, offset as u64);
        self.observe_result(
            observed_path.as_deref(),
            "write",
            &result,
            result.as_ref().copied().unwrap_or(0) as u64,
            true,
        );
        match result {
            Ok(written) => reply.written(written as u32),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn flush(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        if self.open_files.contains_key(&fh) {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn release(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Some(file) = self.open_files.remove(&fh) {
            self.reclaim_inode(file.ino);
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn fsync(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.open_files.get(&fh) {
            Some(file) => match sys::fsync(file, datasync) {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            },
            None => reply.error(libc::EBADF),
        }
    }

    fn opendir(&mut self, _request: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
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

    fn readdir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
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

    fn readdirplus(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let mut delivered = Vec::new();
        for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(
                entry.ino,
                (index + 1) as i64,
                &entry.name,
                &TTL,
                &entry.attr,
                0,
            ) {
                break;
            }
            if index >= 2 {
                delivered.push(entry.ino);
            }
        }
        for ino in delivered {
            self.retain_lookup(ino);
        }
        reply.ok();
    }

    fn releasedir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        if let Some(entries) = self.open_directories.remove(&fh) {
            for entry in entries {
                self.reclaim_inode(entry.ino);
            }
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn fsyncdir(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        _fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = self.node_path(ino).and_then(|path| {
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

    fn statfs(&mut self, _request: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
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

    fn setxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        let observed_path = self.node_path(ino).ok();
        if position != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
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
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn getxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let observed_path = self.node_path(ino).ok();
        let result = self.node_path(ino).and_then(|path| {
            self.core.observe_read(&path)?;
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            sys::get_xattr(&real.path, name)
        });
        self.observe_result(observed_path.as_deref(), "getxattr", &result, 0, false);
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn listxattr(&mut self, _request: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let result = self.node_path(ino).and_then(|path| {
            self.core.observe_read(&path)?;
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            let names = sys::list_xattrs(&real.path)?;
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

    fn removexattr(&mut self, _request: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let observed_path = self.node_path(ino).ok();
        let result = pvisor_overlay_core::validate_guest_xattr(name)
            .and_then(|()| self.copy_up_inode(ino))
            .and_then(|path| self.core.prepare_metadata_change(&path))
            .and_then(|path| sys::remove_xattr(&path, name));
        self.observe_result(observed_path.as_deref(), "removexattr", &result, 0, true);
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn access(&mut self, _request: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
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

    fn create(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let observed_path = self.child_path(parent, name).ok();
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let file = self.core.create_file(&path, mode & !umask, flags)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            let attr = self.attr(ino, &path)?;
            Ok((file, attr))
        })();
        self.observe_result(observed_path.as_deref(), "create", &result, 0, true);
        match result {
            Ok((file, attr)) => {
                let handle = self.allocate_handle();
                self.retain_lookup(attr.ino);
                self.open_files.insert(
                    handle,
                    OpenFile {
                        file,
                        ino: attr.ino,
                        path: observed_path.unwrap_or_default(),
                        flags,
                    },
                );
                reply.created(&TTL, &attr, 0, handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn fallocate(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
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
        let result = sys::allocate(file, offset, length);
        self.observe_result(observed_path.as_deref(), "fallocate", &result, 0, true);
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn lseek(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        match self.open_files.get(&fh) {
            Some(file) => match sys::seek(file, offset, whence) {
                Ok(offset) => reply.offset(offset),
                Err(error) => reply.error(errno(&error)),
            },
            None => reply.error(libc::EBADF),
        }
    }

    fn copy_file_range(
        &mut self,
        _request: &Request<'_>,
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
        let mut copied = 0_u64;
        let mut buffer = vec![0_u8; (len.min(128 * 1024)) as usize];
        let result = (|| {
            while copied < len {
                let wanted = (len - copied).min(buffer.len() as u64) as usize;
                let read = input.read_at(
                    &mut buffer[..wanted],
                    (offset_in as u64).saturating_add(copied),
                )?;
                if read == 0 {
                    break;
                }
                let mut written = 0;
                while written < read {
                    let amount = output.write_at(
                        &buffer[written..read],
                        (offset_out as u64)
                            .saturating_add(copied)
                            .saturating_add(written as u64),
                    )?;
                    if amount == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "copy_file_range made no progress",
                        ));
                    }
                    written += amount;
                }
                copied += read as u64;
            }
            Ok::<(), io::Error>(())
        })();
        match result {
            Ok(()) => reply.written(copied.min(u32::MAX as u64) as u32),
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[cfg(target_os = "macos")]
    fn exchange(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            let first = self.child_path(parent, name)?;
            let second = self.child_path(newparent, newname)?;
            self.core.exchange(&first, &second)?;
            Ok((first, second))
        })();
        match result {
            Ok((first, second)) => {
                self.exchange_inode_prefixes(&first, &second);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[cfg(target_os = "macos")]
    fn getxtimes(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyXTimes) {
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
        let file = overlay.open_inode(ino, libc::O_RDONLY).unwrap();
        overlay.open_files.insert(
            1,
            OpenFile {
                file,
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
