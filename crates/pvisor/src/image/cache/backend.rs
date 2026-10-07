//! Shared lazy image metadata and verified content, independent of transport.
use super::{CacheClient, MAX_READ, Request as CacheRequest, Response, hash};
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_overlay_core::backend::{FileAttr, FileType};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
#[derive(Clone)]
pub(super) struct Node {
    pub(super) path: Vec<u8>,
    pub(super) object_id: u64,
    pub(super) attr: FileAttr,
    pub(super) target: Option<Vec<u8>>,
    pub(super) override_stat: Vec<u8>,
    pub(super) cache: PathBuf,
}

// File identity is the outer key; 1 MiB block indices only have meaning within it.
#[derive(Default)]
struct HotBlocks {
    files: HashMap<u64, HashMap<u64, HotBlock>>,
    order: VecDeque<(u64, u64)>,
    bytes: usize,
}
struct HotBlock {
    bytes: Arc<[u8]>,
    _charge: Option<crate::cache_budget::Charge>,
}
impl HotBlocks {
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    const MAX_ENTRIES: usize = 4096;

    fn get(&self, file: u64, block: u64) -> Option<Arc<[u8]>> {
        self.files
            .get(&file)?
            .get(&block)
            .map(|entry| entry.bytes.clone())
    }

    fn insert(&mut self, file: u64, block: u64, bytes: Arc<[u8]>) {
        if self.get(file, block).is_some() {
            return;
        }
        // Bounded FIFO avoids per-read LRU maintenance; hits do not refresh eviction order.
        while self.bytes + bytes.len() > Self::MAX_BYTES || self.order.len() >= Self::MAX_ENTRIES {
            assert!(self.evict());
        }
        let Ok(charge) = crate::cache_budget::reserve_replacing(bytes.len(), || self.evict())
        else {
            return;
        };
        self.bytes += bytes.len();
        self.files.entry(file).or_default().insert(
            block,
            HotBlock {
                bytes,
                _charge: charge,
            },
        );
        self.order.push_back((file, block));
    }
    fn evict(&mut self) -> bool {
        let Some((file, block)) = self.order.pop_front() else {
            return false;
        };
        let blocks = self.files.get_mut(&file).unwrap();
        self.bytes -= blocks.remove(&block).unwrap().bytes.len();
        if blocks.is_empty() {
            self.files.remove(&file);
        }
        true
    }
}

pub(super) struct RemoteFs {
    pub(super) downloads: Arc<Mutex<super::progress::Downloads>>,
    pub(super) reader: Arc<ContentReader>,
    pub(super) client: Arc<CacheClient>,
    pub(super) digest: String,
    pub(super) cache: PathBuf,
    pub(super) metadata_cache: Option<PathBuf>,
    // Looked-up nodes remain addressable by inode: the backend has no forget
    // contract. Listings retain only bounded pages, not every child's Node.
    // Lightweight object identities survive eviction to preserve hard links.
    nodes: HashMap<u64, Node>,
    paths: HashMap<Vec<u8>, u64>,
    objects: HashMap<u64, u64>,
    directories: HashMap<(u64, usize), Response>,
    directory_order: VecDeque<(u64, usize)>,
    next_inode: u64,
}
impl RemoteFs {
    pub(super) fn new(
        client: CacheClient,
        digest: String,
        cache: PathBuf,
        metadata_cache: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let client = Arc::new(client);
        let downloads = Arc::new(Mutex::new(super::progress::Downloads::default()));
        let reader = Arc::new(ContentReader {
            client: client.clone(),
            digest: digest.clone(),
            downloads: downloads.clone(),
            hot: Mutex::default(),
        });
        let mut fs = Self {
            downloads,
            reader,
            client,
            digest,
            cache,
            metadata_cache,
            nodes: HashMap::new(),
            paths: HashMap::new(),
            objects: HashMap::new(),
            directories: HashMap::new(),
            directory_order: VecDeque::new(),
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

    pub(super) fn lookup_path(&mut self, path: Vec<u8>) -> anyhow::Result<Node> {
        if let Some(ino) = self.paths.get(&path) {
            return self.node(*ino).cloned();
        }
        let listed = self.directories.iter().find_map(|((ino, _), response)| {
            let parent = self.nodes.get(ino)?;
            let Response::Entries {
                names, metadata, ..
            } = response
            else {
                return None;
            };
            names.iter().zip(metadata).find_map(|(name, attr)| {
                let mut child = parent.path.clone();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(name);
                (child == path).then(|| attr.clone())
            })
        });
        let response = match listed {
            Some(response) => response,
            None => self.metadata_request(CacheRequest::Stat {
                digest: self.digest.clone(),
                path: path.clone(),
            })?,
        };
        self.insert_node(path, response)
    }

    fn insert_node(&mut self, path: Vec<u8>, response: Response) -> anyhow::Result<Node> {
        self.make_node(path, response, true)
    }

    fn make_node(
        &mut self,
        path: Vec<u8>,
        response: Response,
        retain: bool,
    ) -> anyhow::Result<Node> {
        if let Some(ino) = self.paths.get(&path) {
            return self.node(*ino).cloned();
        }
        let Response::Metadata {
            kind,
            size,
            mode,
            uid,
            gid,
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
        let ino = if let Some(ino) = self.objects.get(&inode).copied() {
            if let Some(node) = self.nodes.get(&ino) {
                let node = node.clone();
                if retain {
                    self.paths.insert(path, ino);
                }
                return Ok(node);
            }
            ino
        } else {
            let ino = self.next_inode;
            self.next_inode = self
                .next_inode
                .checked_add(1)
                .context("remote inode overflow")?;
            ino
        };
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
            UNIX_EPOCH
                .checked_sub(Duration::from_secs(mtime.unsigned_abs()))
                .and_then(|time| time.checked_add(Duration::from_nanos(mtime_nsec as u64)))
        }
        .context("remote timestamp overflow")?;
        let node = Node {
            object_id: inode,
            cache: self.cache.join(&hash(&path)[7..]),
            path: path.clone(),
            target,
            override_stat: format!("{uid}:{gid}:0{mode:o}").into_bytes(),
            attr: FileAttr {
                ino,
                size,
                blocks: size.div_ceil(512),
                atime: time,
                mtime: time,
                ctime: time,
                crtime: time,
                kind,
                perm: (mode & 0o7777) as u16,
                uid,
                gid,
                nlink: nlink.min(u32::MAX as u64) as u32,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            },
        };
        self.objects.insert(inode, ino);
        if retain {
            self.paths.insert(path, ino);
            self.nodes.insert(ino, node.clone());
        }
        Ok(node)
    }

    pub(super) fn node(&self, ino: u64) -> anyhow::Result<&Node> {
        self.nodes
            .get(&ino)
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT).into())
    }

    pub(super) fn child(&mut self, parent: u64, name: &OsStr) -> anyhow::Result<Node> {
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
        // Absence is provable only while all contiguous pages are resident.
        let mut offset = 0;
        while let Some(Response::Entries {
            names, next_offset, ..
        }) = self.directories.get(&(parent.attr.ino, offset))
        {
            if names.iter().any(|entry| entry == name) {
                break;
            }
            match next_offset {
                Some(next) => offset = *next,
                None => return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into()),
            }
        }
        self.lookup_path(path)
    }

    const DIRECTORY_PAGES: usize = 8;

    /// Cookies are compact list offsets plus two synthetic dot entries. Fetch
    /// at the cookie itself, so eviction never requires replaying earlier pages.
    pub(super) fn entries_page(
        &mut self,
        ino: u64,
        cookie: usize,
    ) -> anyhow::Result<Vec<(u64, FileType, OsString)>> {
        let node = self.node(ino)?.clone();
        ensure!(node.attr.kind == FileType::Directory, "not a directory");
        let mut entries = Vec::new();
        if cookie < 2 {
            let parent = Path::new(OsStr::from_bytes(&node.path))
                .parent()
                .unwrap_or(Path::new(""));
            let parent = self.lookup_path(parent.as_os_str().as_bytes().to_vec())?;
            if cookie == 0 {
                entries.push((ino, FileType::Directory, ".".into()));
            }
            entries.push((parent.attr.ino, FileType::Directory, "..".into()));
        }
        let offset = cookie.saturating_sub(2);
        let key = (ino, offset);
        // A resumed cookie can lie inside an already cached page.
        let cached = self
            .directories
            .iter()
            .find_map(|((directory, start), response)| {
                let Response::Entries {
                    names, next_offset, ..
                } = response
                else {
                    return None;
                };
                (*directory == ino
                    && offset >= *start
                    && (offset < start + names.len()
                        || (offset == start + names.len() && next_offset.is_none())))
                .then(|| (*start, response.clone()))
            });
        let (start, response) = match cached {
            Some(page) => page,
            None => (
                offset,
                self.metadata_request(CacheRequest::List {
                    digest: self.digest.clone(),
                    path: node.path.clone(),
                    offset,
                })?,
            ),
        };
        let Response::Entries {
            names,
            metadata,
            next_offset,
        } = &response
        else {
            anyhow::bail!("expected cache directory response");
        };
        ensure!(
            names.len() == metadata.len(),
            "directory metadata count mismatch"
        );
        let end = start
            .checked_add(names.len())
            .context("directory offset overflow")?;
        ensure!(end <= i64::MAX as usize - 2, "directory cookie overflow");
        ensure!(
            next_offset.is_none() || (*next_offset == Some(end) && end > start),
            "invalid cache directory continuation"
        );
        ensure!(
            serde_json::to_vec(&response)?.len() <= super::protocol::MAX_FRAME,
            "cache directory page exceeds frame limit"
        );
        for (name, attr) in names.iter().zip(metadata) {
            ensure!(
                !name.is_empty()
                    && name != b"."
                    && name != b".."
                    && !name.contains(&b'/')
                    && !name.contains(&0),
                "invalid remote filename"
            );
            ensure!(
                matches!(attr, Response::Metadata { .. }),
                "invalid directory metadata"
            );
        }
        for (name, attr) in names.iter().zip(metadata).skip(offset - start) {
            let mut path = node.path.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name);
            let child = self.make_node(path, attr.clone(), false)?;
            entries.push((
                child.attr.ino,
                child.attr.kind,
                OsString::from_vec(name.clone()),
            ));
        }
        if !self.directories.contains_key(&(ino, start)) {
            while self.directory_order.len() >= Self::DIRECTORY_PAGES {
                self.directories
                    .remove(&self.directory_order.pop_front().unwrap());
            }
            self.directory_order.push_back(key);
            self.directories.insert(key, response);
        }
        Ok(entries)
    }

    /// Compatibility for direct projection: its trait requires a complete host
    /// directory. This transient result is not a retained metadata cache.
    pub(super) fn entries(&mut self, ino: u64) -> anyhow::Result<Vec<(u64, FileType, OsString)>> {
        let mut entries = Vec::new();
        loop {
            let page = self.entries_page(ino, entries.len())?;
            if page.is_empty() {
                break;
            }
            entries.extend(page);
        }
        Ok(entries)
    }

    pub(super) fn read_range(&self, ino: u64, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
        self.reader.read(self.node(ino)?, offset, size)
    }
}

/// Content fetches never hold the metadata map or a whole-service mutex.
/// Only the requested persistent cache block is locked during network I/O.
pub(super) struct ContentReader {
    client: Arc<CacheClient>,
    digest: String,
    downloads: Arc<Mutex<super::progress::Downloads>>,
    hot: Mutex<HotBlocks>,
}
impl ContentReader {
    fn block(&self, node: &Node, index: u64) -> anyhow::Result<(Arc<[u8]>, bool)> {
        if let Some(bytes) = self.hot.lock().unwrap().get(node.attr.ino, index) {
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
                    .lock()
                    .unwrap()
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
        self.downloads
            .lock()
            .unwrap()
            .received(&node.path, body.len());
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&Sha256::digest(&body))?;
        temporary.write_all(&body)?;
        temporary.persist(&path)?;
        let body: Arc<[u8]> = body.into();
        self.hot
            .lock()
            .unwrap()
            .insert(node.attr.ino, index, body.clone());
        Ok((body, false))
    }

    pub(super) fn read(&self, node: &Node, offset: u64, size: u32) -> anyhow::Result<Vec<u8>> {
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
                self.downloads.lock().unwrap().cached(&node.path, count);
            }
            cursor += count as u64;
        }
        Ok(result)
    }
}

#[cfg(test)]
pub(crate) mod tests;
