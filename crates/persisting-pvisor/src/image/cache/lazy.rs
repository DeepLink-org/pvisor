//! A read-only, demand-filled FUSE lower for the existing VM/OCI overlays.
use super::{CacheClient, MAX_READ, Request as CacheRequest, Response, architecture, hash};
use crate::image::oci::{ImageStore, PreparedImage};
use anyhow::{Context, ensure};
use fs2::FileExt;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, ReplyOpen, ReplyStatfs, Request, Session,
};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(3600);

pub(crate) struct LazyMount {
    session: Option<BackgroundSession>,
    path: PathBuf,
}
impl Drop for LazyMount {
    fn drop(&mut self) {
        if let Some(session) = self.session.take()
            && let Err(error) = session.unmount()
        {
            crate::diagnostics::diagnostic(format_args!(
                "unmount lazy image {}: {error}",
                self.path.display()
            ));
        }
        #[cfg(target_os = "linux")]
        let _ = fs::remove_dir(&self.path);
    }
}

pub(crate) fn prepare_image(
    image: &str,
    store: Option<PathBuf>,
) -> anyhow::Result<(PreparedImage, Option<LazyMount>)> {
    let client = CacheClient::discover()?;
    let store = ImageStore::new(store)?;
    let Some(client) = client else {
        return Ok((
            super::progress::loading("resolving and preparing local image", || {
                store.prepare(image)
            })?,
            None,
        ));
    };
    let downloads = super::progress::Downloads::new(image);
    crate::diagnostics::diagnostic(format_args!(
        "pVisor image: lazy loading from {}",
        client.endpoint
    ));
    let (response, _) =
        super::progress::loading("waiting for cache to resolve and prepare image", || {
            client.request(CacheRequest::Prepare {
                image: image.into(),
                architecture: architecture().into(),
                refresh: false,
            })
        })?;
    let Response::Prepared {
        metadata_generation,
        totals,
        digest,
        architecture: platform,
        env,
        entrypoint,
        cmd,
    } = response
    else {
        anyhow::bail!("expected cache prepared response");
    };
    ensure!(
        platform == architecture(),
        "cache returned the wrong image architecture"
    );
    crate::image::oci::digest_hex(&digest)?;
    let cache = dirs::cache_dir()
        .context("cannot find user cache directory")?
        .join("persisting/pvisor/blocks")
        .join(&hash(client.endpoint.as_bytes())[7..])
        .join(&digest[7..]);
    fs::create_dir_all(&cache)?;
    downloads.totals(totals);
    if let Some(totals) = totals {
        crate::diagnostics::diagnostic(format_args!(
            "pVisor image: prepared {digest}; {} files, {:.1} MiB (contents fetched on demand)",
            totals.files,
            totals.bytes as f64 / (1024.0 * 1024.0)
        ));
    }
    let metadata_cache = metadata_generation.map(|generation| {
        dirs::cache_dir()
            .expect("cache directory already resolved")
            .join("persisting/pvisor/metadata/v1")
            .join(&hash(client.endpoint.as_bytes())[7..])
            .join(&digest[7..])
            .join(&hash(generation.as_bytes())[7..])
    });
    let mut filesystem = super::progress::loading("loading root metadata", || {
        RemoteFs::new(client, digest.clone(), cache, metadata_cache)
    })?;
    filesystem.downloads = downloads;
    let mount =
        super::progress::loading("mounting lazy rootfs", || mount(filesystem, &store.root))?;
    Ok((
        PreparedImage {
            rootfs: mount.path.clone(),
            digest,
            env,
            entrypoint,
            cmd,
        },
        Some(mount),
    ))
}

fn mount(filesystem: RemoteFs, _store: &Path) -> anyhow::Result<LazyMount> {
    #[cfg(target_os = "macos")]
    let mountpoint =
        PathBuf::from("/Volumes").join(format!("pvisor-image-{}", uuid::Uuid::new_v4()));
    #[cfg(target_os = "linux")]
    let mountpoint = {
        let path = _store.join(format!(".lazy-mount-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        path
    };
    #[allow(unused_mut)]
    let mut options = vec![
        MountOption::FSName("pvisor-image".into()),
        MountOption::RO,
        MountOption::NoAtime,
        MountOption::DefaultPermissions,
    ];
    #[cfg(target_os = "macos")]
    options.push(MountOption::CUSTOM("backend=fskit".into()));
    let session = Session::new(filesystem, &mountpoint, &options)
        .context("mount lazy image lower (FUSE is required); set PERSISTING_PVISOR_CACHE_SERVER=off to use local OCI extraction")?;
    let mount = LazyMount {
        session: Some(BackgroundSession::new(session)?),
        path: mountpoint.clone(),
    };
    // FSKit attaches asynchronously after its request loop starts.
    #[cfg(target_os = "macos")]
    for _ in 0..250 {
        if persisting_overlayfs::is_mountpoint(&mountpoint) {
            break;
        }
        if mount
            .session
            .as_ref()
            .is_some_and(|s| s.guard.is_finished())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        mountpoint.is_dir(),
        "lazy image mount did not become available"
    );
    Ok(mount)
}

#[derive(Clone)]
struct Node {
    path: Vec<u8>,
    attr: FileAttr,
    target: Option<Vec<u8>>,
    cache: PathBuf,
}

// File identity is the outer key; 1 MiB block indices only have meaning within it.
#[derive(Default)]
struct HotBlocks {
    files: HashMap<u64, HashMap<u64, Arc<[u8]>>>,
    order: VecDeque<(u64, u64)>,
    bytes: usize,
}
impl HotBlocks {
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    const MAX_ENTRIES: usize = 4096;

    fn get(&self, file: u64, block: u64) -> Option<Arc<[u8]>> {
        self.files.get(&file)?.get(&block).cloned()
    }

    fn insert(&mut self, file: u64, block: u64, bytes: Arc<[u8]>) {
        if self.get(file, block).is_some() {
            return;
        }
        // ponytail: bounded FIFO avoids per-read LRU maintenance; use LRU if
        // eviction of frequently reused blocks becomes a measured bottleneck.
        while self.bytes + bytes.len() > Self::MAX_BYTES || self.order.len() >= Self::MAX_ENTRIES {
            let (file, block) = self.order.pop_front().unwrap();
            let blocks = self.files.get_mut(&file).unwrap();
            self.bytes -= blocks.remove(&block).unwrap().len();
            if blocks.is_empty() {
                self.files.remove(&file);
            }
        }
        self.bytes += bytes.len();
        self.files.entry(file).or_default().insert(block, bytes);
        self.order.push_back((file, block));
    }
}

struct RemoteFs {
    downloads: super::progress::Downloads,
    hot: RefCell<HotBlocks>,
    client: CacheClient,
    digest: String,
    cache: PathBuf,
    metadata_cache: Option<PathBuf>,
    // ponytail: metadata is retained for one immutable image (bounded by its
    // entries); add reference-counted eviction if large images exceed memory.
    nodes: HashMap<u64, Node>,
    paths: HashMap<Vec<u8>, u64>,
    objects: HashMap<u64, u64>,
    directories: HashMap<u64, Vec<(u64, FileType, OsString)>>,
    next_inode: u64,
}
impl RemoteFs {
    fn new(
        client: CacheClient,
        digest: String,
        cache: PathBuf,
        metadata_cache: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut fs = Self {
            downloads: super::progress::Downloads::default(),
            hot: RefCell::default(),
            client,
            digest,
            cache,
            metadata_cache,
            nodes: HashMap::new(),
            paths: HashMap::new(),
            objects: HashMap::new(),
            directories: HashMap::new(),
            next_inode: 1,
        };
        let root = fs.lookup_path(Vec::new())?;
        ensure!(
            root.attr.ino == 1 && root.attr.kind == FileType::Directory,
            "invalid remote image root"
        );
        Ok(fs)
    }

    fn metadata_request(&self, request: CacheRequest) -> anyhow::Result<Response> {
        let Some(directory) = &self.metadata_cache else {
            return self.client.request(request).map(|(response, _)| response);
        };
        let path = directory.join(&hash(&serde_json::to_vec(&request)?)[7..]);
        let cached = fs::read(&path).ok().and_then(|bytes| {
            (bytes.len() >= 32 && Sha256::digest(&bytes[32..]).as_slice() == &bytes[..32])
                .then(|| serde_json::from_slice::<Response>(&bytes[32..]).ok())
                .flatten()
        });
        let response = if let Some(response) = cached {
            response
        } else {
            let response = match self.client.request(request) {
                Ok((response, _)) => response,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    Response::Error {
                        code: "not_found".into(),
                        message: "path absent from immutable image".into(),
                    }
                }
                Err(error) => return Err(error),
            };
            fs::create_dir_all(directory)?;
            let bytes = serde_json::to_vec(&response)?;
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            temporary.write_all(&Sha256::digest(&bytes))?;
            temporary.write_all(&bytes)?;
            temporary.persist(&path)?;
            response
        };
        match response {
            Response::Error { code, message } if code == "not_found" => {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, message).into())
            }
            response => Ok(response),
        }
    }

    fn lookup_path(&mut self, path: Vec<u8>) -> anyhow::Result<Node> {
        if let Some(ino) = self.paths.get(&path) {
            return self.node(*ino).cloned();
        }
        let response = self.metadata_request(CacheRequest::Stat {
            digest: self.digest.clone(),
            path: path.clone(),
        })?;
        self.insert_node(path, response)
    }

    fn insert_node(&mut self, path: Vec<u8>, response: Response) -> anyhow::Result<Node> {
        if let Some(ino) = self.paths.get(&path) {
            return self.node(*ino).cloned();
        }
        let Response::Metadata {
            kind,
            size,
            mode,
            inode,
            nlink,
            mtime,
            mtime_nsec,
            target,
            ..
        } = response
        else {
            anyhow::bail!("expected cache metadata response");
        };
        if let Some(ino) = self.objects.get(&inode) {
            self.paths.insert(path, *ino);
            return self.node(*ino).cloned();
        }
        let ino = self.next_inode;
        self.next_inode += 1;
        // libc mode constants are u16 on macOS and u32 on Linux.
        #[allow(clippy::unnecessary_cast)]
        let kind = match kind.as_str() {
            "directory" => FileType::Directory,
            "file" => FileType::RegularFile,
            "symlink" => FileType::Symlink,
            _ => match mode & libc::S_IFMT as u32 {
                x if x == libc::S_IFIFO as u32 => FileType::NamedPipe,
                x if x == libc::S_IFSOCK as u32 => FileType::Socket,
                _ => anyhow::bail!("unsupported special file in remote image"),
            },
        };
        ensure!(
            (0..1_000_000_000).contains(&mtime_nsec),
            "invalid remote timestamp"
        );
        let time = if mtime >= 0 {
            UNIX_EPOCH.checked_add(Duration::new(mtime as u64, mtime_nsec as u32))
        } else {
            UNIX_EPOCH.checked_sub(Duration::from_secs(mtime.unsigned_abs()))
        }
        .context("remote timestamp overflow")?;
        let node = Node {
            cache: self.cache.join(&hash(&path)[7..]),
            path: path.clone(),
            target,
            attr: FileAttr {
                ino,
                size,
                blocks: size.div_ceil(512),
                atime: time,
                mtime: time,
                ctime: time,
                crtime: time,
                kind,
                // Host permission checks use the mounting user; the guest receives its
                // normal permission semantics through the existing virtio-fs overlay.
                perm: if ino == 1 {
                    0o700
                } else {
                    (mode & 0o7777) as u16
                },
                uid: unsafe { libc::geteuid() },
                gid: unsafe { libc::getegid() },
                nlink: nlink.min(u32::MAX as u64) as u32,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            },
        };
        self.paths.insert(path, ino);
        self.objects.insert(inode, ino);
        self.nodes.insert(ino, node.clone());
        Ok(node)
    }

    fn node(&self, ino: u64) -> anyhow::Result<&Node> {
        self.nodes
            .get(&ino)
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT).into())
    }

    fn child(&mut self, parent: u64, name: &OsStr) -> anyhow::Result<Node> {
        let parent = self.node(parent)?;
        ensure!(parent.attr.kind == FileType::Directory, "not a directory");
        let name = name.as_bytes();
        ensure!(
            !name.is_empty()
                && name != b"."
                && name != b".."
                && !name.contains(&b'/')
                && !name.contains(&0),
            "invalid remote filename"
        );
        let mut path = parent.path.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(name);
        if self.directories.contains_key(&parent.attr.ino) && !self.paths.contains_key(&path) {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
        }
        self.lookup_path(path)
    }

    fn entries(&mut self, ino: u64) -> anyhow::Result<&Vec<(u64, FileType, OsString)>> {
        if !self.directories.contains_key(&ino) {
            let node = self.node(ino)?.clone();
            ensure!(node.attr.kind == FileType::Directory, "not a directory");
            let parent = Path::new(OsStr::from_bytes(&node.path))
                .parent()
                .unwrap_or(Path::new(""));
            let parent = self.lookup_path(parent.as_os_str().as_bytes().to_vec())?;
            let mut entries = vec![
                (ino, FileType::Directory, ".".into()),
                (parent.attr.ino, FileType::Directory, "..".into()),
            ];
            let mut offset = 0;
            loop {
                let response = self.metadata_request(CacheRequest::List {
                    digest: self.digest.clone(),
                    path: node.path.clone(),
                    offset,
                })?;
                let Response::Entries {
                    names,
                    metadata,
                    next_offset,
                } = response
                else {
                    anyhow::bail!("expected cache directory response");
                };
                if let Some(attributes) = &metadata {
                    ensure!(
                        attributes.len() == names.len(),
                        "directory metadata count mismatch"
                    );
                }
                let mut attributes = metadata.map(Vec::into_iter);
                for name in names {
                    // Validate untrusted directory names before using them as paths.
                    ensure!(
                        !name.is_empty()
                            && name != b"."
                            && name != b".."
                            && !name.contains(&b'/')
                            && !name.contains(&0),
                        "invalid remote filename"
                    );
                    let child = if let Some(attributes) = &mut attributes {
                        let mut path = node.path.clone();
                        if !path.is_empty() {
                            path.push(b'/');
                        }
                        path.extend_from_slice(&name);
                        self.insert_node(path, attributes.next().unwrap())?
                    } else {
                        // Older servers and persisted v1 pages contain names only.
                        self.child(ino, OsStr::from_bytes(&name))?
                    };
                    entries.push((child.attr.ino, child.attr.kind, OsString::from_vec(name)));
                }
                match next_offset {
                    Some(next) => {
                        ensure!(next > offset, "invalid cache directory continuation");
                        offset = next;
                    }
                    None => break,
                }
            }
            self.directories.insert(ino, entries);
        }
        Ok(self.directories.get(&ino).unwrap())
    }

    fn block(&self, node: &Node, index: u64) -> anyhow::Result<(Arc<[u8]>, bool)> {
        if let Some(bytes) = self.hot.borrow().get(node.attr.ino, index) {
            return Ok((bytes, true));
        }
        let directory = &node.cache;
        fs::create_dir_all(directory)?;
        let path = directory.join(index.to_string());
        // Persistent lock inodes prevent unlink/reopen from splitting flock
        // ownership. Remove them only when evicting an unused cache directory.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join(format!("{index}.lock")))?;
        lock.lock_exclusive()?;
        let offset = index
            .checked_mul(MAX_READ as u64)
            .context("cache block offset overflow")?;
        let length = node.attr.size.saturating_sub(offset).min(MAX_READ as u64) as usize;
        match fs::read(&path) {
            Ok(bytes)
                if bytes.len() == length + 32
                    && Sha256::digest(&bytes[32..]).as_slice() == &bytes[..32] =>
            {
                let body: Arc<[u8]> = bytes[32..].into();
                self.hot
                    .borrow_mut()
                    .insert(node.attr.ino, index, body.clone());
                return Ok((body, true));
            }
            Ok(_) => {
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let (_, body) = self.client.request(CacheRequest::Read {
            digest: self.digest.clone(),
            path: node.path.clone(),
            offset,
            length: length as u32,
        })?;
        ensure!(
            body.len() == length,
            "remote file returned a short block before EOF"
        );
        self.downloads.received(&node.path, body.len());
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&Sha256::digest(&body))?;
        temporary.write_all(&body)?;
        temporary.persist(&path)?;
        let body: Arc<[u8]> = body.into();
        self.hot
            .borrow_mut()
            .insert(node.attr.ino, index, body.clone());
        Ok((body, false))
    }

    fn read_range(&self, ino: u64, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
        let node = self.node(ino)?;
        ensure!(
            node.attr.kind == FileType::RegularFile,
            "read requires a regular file"
        );
        let end = offset.saturating_add(size as u64).min(node.attr.size);
        let mut result = Vec::with_capacity(end.saturating_sub(offset) as usize);
        let mut cursor = offset;
        while cursor < end {
            let (bytes, cached) = self.block(node, cursor / MAX_READ as u64)?;
            let begin = (cursor % MAX_READ as u64) as usize;
            let count = (end - cursor).min((bytes.len() - begin) as u64) as usize;
            result.extend_from_slice(&bytes[begin..begin + count]);
            if cached {
                self.downloads.cached(&node.path, count);
            }
            cursor += count as u64;
        }
        Ok(result)
    }
}

fn errno(error: anyhow::Error) -> i32 {
    let code = error
        .chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .find_map(|e| {
            e.raw_os_error().or(match e.kind() {
                std::io::ErrorKind::NotFound => Some(libc::ENOENT),
                std::io::ErrorKind::PermissionDenied => Some(libc::EACCES),
                _ => None,
            })
        })
        .unwrap_or(libc::EIO);
    if code != libc::ENOENT {
        crate::diagnostics::diagnostic(format_args!("lazy image I/O: {error:#}"));
    }
    code
}
// ponytail: synchronous FUSE reads preserve the existing serialized virtio-fs
// behavior. Introduce queued completions when cache-miss latency warrants it.
impl Filesystem for RemoteFs {
    fn lookup(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.child(parent, name) {
            Ok(node) => reply.entry(&TTL, &node.attr, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn getattr(&mut self, _: &Request<'_>, ino: u64, _: Option<u64>, reply: ReplyAttr) {
        match self.node(ino) {
            Ok(node) => reply.attr(&TTL, &node.attr),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readlink(&mut self, _: &Request<'_>, ino: u64, reply: ReplyData) {
        match self
            .node(ino)
            .and_then(|node| node.target.as_deref().context("not a symlink"))
        {
            Ok(target) => reply.data(target),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn open(&mut self, _: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        // FSKit can request O_RDWR for a read on a read-only volume.
        if flags & (libc::O_TRUNC | libc::O_APPEND) != 0 {
            reply.error(libc::EROFS);
            return;
        }
        match self.node(ino) {
            Ok(node) if matches!(node.attr.kind, FileType::RegularFile | FileType::Symlink) => {
                reply.opened(ino, 0)
            }
            _ => reply.error(libc::EINVAL),
        }
    }
    fn read(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        size: u32,
        _: i32,
        _: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        match self.read_range(ino, offset as u64, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn opendir(&mut self, _: &Request<'_>, ino: u64, _: i32, reply: ReplyOpen) {
        match self.node(ino) {
            Ok(node) if node.attr.kind == FileType::Directory => reply.opened(ino, 0),
            _ => reply.error(libc::ENOTDIR),
        }
    }
    fn readdir(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        match self.entries(ino) {
            Ok(entries) => {
                for (index, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                    if reply.add(*ino, (index + 1) as i64, *kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn statfs(&mut self, _: &Request<'_>, _: u64, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096);
    }
}

#[cfg(test)]
mod tests;
