//! Stable little-endian page format. No native Rust layout is serialized.
use super::*;
use sha2::{Digest, Sha256};
pub(super) const PAGE_BYTES: usize = 65536;
const MAGIC: &[u8; 8] = b"PVICB2\0\0";
const CHECK_MAGIC: &[u8; 8] = b"PVICH2\0\0";
const FILE_WIDTH: usize = 128;
const CONTENT_WIDTH: usize = 64;
const CHUNK_WIDTH: usize = 40;
const NODE_WIDTH: usize = 280;
const NODE_CAP: usize = (PAGE_BYTES - 16) / NODE_WIDTH;
const NONE: u64 = u64::MAX;

pub(super) struct SourceEntry {
    pub path: Vec<u8>,
    pub metadata: Response,
    pub content: Option<String>,
}
pub(super) struct SourceContent {
    pub size: u64,
    pub chunks: Vec<(String, u32)>,
}
#[derive(Debug)]
pub(super) struct FileEntry {
    pub id: u64,
    pub parent: u64,
    pub path: Vec<u8>,
    pub metadata: Response,
    pub content: Option<u64>,
}
pub(super) struct Content {
    pub size: u64,
    pub first: u64,
    pub count: u64,
}
#[derive(Clone)]
struct Header {
    count: u64,
    aux: u64,
    aux_count: u64,
    root: u64,
    bytes: u64,
}
type PageCache = Mutex<LruCache<(String, u64), Arc<Vec<u8>>>>;
struct Pages {
    storage: Storage,
    handle: Handle,
    metadata: BTreeMap<String, Descriptor>,
    hashes: BTreeMap<String, Vec<[u8; 32]>>,
    local: Option<PathBuf>,
    cache: PageCache,
}
pub(super) struct LoadedImage {
    pub commit: Commit,
    config: Configuration,
    handle: Handle,
    pages: Pages,
    files: Header,
    contents: Header,
    index: Header,
}
impl Pages {
    fn new(
        storage: Storage,
        local: Option<PathBuf>,
        handle: Handle,
        commit: &Commit,
        catalog: &[u8],
    ) -> anyhow::Result<Self> {
        ensure!(
            catalog.len() >= 80
                && &catalog[..8] == CHECK_MAGIC
                && u32_at(catalog, 8)? == PAGE_BYTES as u32
                && u32_at(catalog, 12)? == 4,
            "invalid checksum catalog"
        );
        let mut hashes = BTreeMap::new();
        let mut offset = 80usize;
        for (i, name) in BINARY_NAMES.iter().enumerate() {
            let len = commit.metadata[*name].bytes;
            let count = len.div_ceil(PAGE_BYTES as u64);
            ensure!(
                u64_at(catalog, 16 + i * 16)? == len && u64_at(catalog, 24 + i * 16)? == count,
                "checksum page count mismatch"
            );
            let mut entries = Vec::new();
            for _ in 0..count {
                entries.push(
                    catalog
                        .get(offset..offset + 32)
                        .context("truncated checksum catalog")?
                        .try_into()?,
                );
                offset += 32;
            }
            hashes.insert((*name).into(), entries);
        }
        ensure!(offset == catalog.len(), "trailing checksum data");
        Ok(Self {
            storage,
            handle,
            metadata: commit.metadata.clone(),
            hashes,
            local,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
        })
    }
    fn page(&self, name: &str, id: u64) -> anyhow::Result<Arc<Vec<u8>>> {
        let key = (name.to_string(), id);
        if let Some(bytes) = self.cache.lock().unwrap().get(&key).cloned() {
            return Ok(bytes);
        }
        let expected = self
            .hashes
            .get(name)
            .and_then(|h| h.get(id as usize))
            .context("page ID out of range")?;
        let start = id
            .checked_mul(PAGE_BYTES as u64)
            .context("page offset overflow")?;
        let end = (start + PAGE_BYTES as u64).min(self.metadata[name].bytes);
        ensure!(start < end, "empty page");
        let len = (end - start) as usize;
        let valid =
            |bytes: &[u8]| bytes.len() == len && Sha256::digest(bytes).as_slice() == expected;
        let local = self.local.as_ref().map(|root| {
            root.join("pages")
                .join(&self.handle.revision)
                .join(name)
                .join(id.to_string())
        });
        let bytes = if let Some(path) = &local
            && let Some(bytes) = read_local(path, PAGE_BYTES)?
            && valid(&bytes)
        {
            bytes
        } else {
            let bytes = self
                .storage
                .range(&format!("{}/{name}", self.handle.prefix()), start..end)?;
            ensure!(valid(&bytes), "cache metadata page digest mismatch");
            if let Some(path) = local {
                cache_local(&path, &bytes);
            }
            bytes
        };
        let bytes = Arc::new(bytes);
        self.cache.lock().unwrap().put(key, bytes.clone());
        Ok(bytes)
    }
    fn read(&self, name: &str, offset: u64, len: usize) -> anyhow::Result<Vec<u8>> {
        ensure!(
            len <= 16384
                && offset
                    .checked_add(len as u64)
                    .is_some_and(|end| end <= self.metadata[name].bytes),
            "metadata byte range out of bounds"
        );
        let mut bytes = Vec::with_capacity(len);
        let mut position = offset;
        while bytes.len() < len {
            let page = self.page(name, position / PAGE_BYTES as u64)?;
            let start = (position % PAGE_BYTES as u64) as usize;
            let count = (len - bytes.len()).min(page.len() - start);
            bytes.extend_from_slice(&page[start..start + count]);
            position += count as u64;
        }
        Ok(bytes)
    }
    fn header(&self, name: &str, kind: u32, width: usize, max: u64) -> anyhow::Result<Header> {
        let p = self.page(name, 0)?;
        ensure!(
            p.len() == PAGE_BYTES
                && &p[..8] == MAGIC
                && u32_at(&p, 8)? == 1
                && u32_at(&p, 12)? == kind
                && u32_at(&p, 16)? == PAGE_BYTES as u32
                && u32_at(&p, 20)? == width as u32,
            "unsupported binary metadata schema"
        );
        let h = Header {
            bytes: u64_at(&p, 24)?,
            count: u64_at(&p, 32)?,
            aux: u64_at(&p, 48)?,
            aux_count: u64_at(&p, 56)?,
            root: u64_at(&p, 72)?,
        };
        ensure!(
            h.bytes == self.metadata[name].bytes
                && h.bytes.is_multiple_of(PAGE_BYTES as u64)
                && h.count <= max
                && u64_at(&p, 40)? == PAGE_BYTES as u64,
            "invalid metadata table bounds"
        );
        ensure!(
            p[68..72].iter().all(|b| *b == 0) && p[80..].iter().all(|b| *b == 0),
            "unknown metadata feature flags"
        );
        match kind {
            1 => {
                ensure!(
                    h.count > 0
                        && h.aux == table_end(h.count, FILE_WIDTH)
                        && h.aux
                            .checked_add(h.aux_count)
                            .is_some_and(|end| end <= h.bytes)
                        && u32_at(&p, 64)? == 1
                        && h.root == 0,
                    "invalid file sections"
                );
            }
            2 => {
                ensure!(
                    h.aux == table_end(h.count, CONTENT_WIDTH)
                        && h.aux_count <= MAX_SPANS as u64
                        && h.bytes == h.aux + table_bytes(h.aux_count, CHUNK_WIDTH)
                        && u32_at(&p, 64)? == CHUNK_WIDTH as u32
                        && h.root == 0,
                    "invalid content sections"
                );
            }
            3 => {
                ensure!(
                    h.bytes >= 2 * PAGE_BYTES as u64
                        && h.root > 0
                        && h.root < h.bytes / PAGE_BYTES as u64
                        && h.aux == 0
                        && h.aux_count == 0
                        && u32_at(&p, 64)? == 0,
                    "invalid index root"
                );
            }
            _ => bail!("unsupported metadata type"),
        }
        Ok(h)
    }
}
impl LoadedImage {
    #[cfg(test)]
    pub(super) fn cached_page_count(&self) -> usize {
        self.pages.cache.lock().unwrap().len()
    }
    pub(super) fn new(
        storage: Storage,
        local: Option<PathBuf>,
        handle: Handle,
        commit: Commit,
        config: Configuration,
        catalog: &[u8],
    ) -> anyhow::Result<Self> {
        let pages = Pages::new(storage, local, handle.clone(), &commit, catalog)?;
        let (files, contents, index) = std::thread::scope(|scope| {
            let files =
                scope.spawn(|| pages.header("files.bin", 1, FILE_WIDTH, MAX_ENTRIES as u64));
            let contents =
                scope.spawn(|| pages.header("contents.bin", 2, CONTENT_WIDTH, MAX_ENTRIES as u64));
            let index =
                scope.spawn(|| pages.header("index.bin", 3, NODE_WIDTH, MAX_ENTRIES as u64));
            Ok::<_, anyhow::Error>((
                files
                    .join()
                    .map_err(|_| anyhow::anyhow!("file header reader panicked"))??,
                contents
                    .join()
                    .map_err(|_| anyhow::anyhow!("content header reader panicked"))??,
                index
                    .join()
                    .map_err(|_| anyhow::anyhow!("index header reader panicked"))??,
            ))
        })?;
        ensure!(
            index.count + 1 == files.count,
            "file/index entry count mismatch"
        );
        let image = Self {
            commit,
            config,
            handle,
            pages,
            files,
            contents,
            index,
        };
        let root = std::thread::scope(|scope| {
            let root = scope.spawn(|| image.file(0));
            let index = scope.spawn(|| image.node(image.index.root));
            let root = root
                .join()
                .map_err(|_| anyhow::anyhow!("root metadata reader panicked"))??;
            index
                .join()
                .map_err(|_| anyhow::anyhow!("root index reader panicked"))??;
            Ok::<_, anyhow::Error>(root)
        })?;
        ensure!(
            root.path.is_empty()
                && root.parent == NONE
                && matches!(root.metadata,Response::Metadata{ref kind,..} if kind=="directory"),
            "invalid root file record"
        );
        Ok(image)
    }
    pub(super) fn prepared(&self) -> Response {
        Response::Prepared {
            image_handle: Some(self.handle.encode()),
            metadata_generation: Some(format!("sha256:{}", self.handle.revision)),
            totals: Some(self.config.totals),
            digest: self.commit.manifest_digest.clone(),
            architecture: self.config.architecture.clone(),
            env: self.config.env.clone(),
            entrypoint: self.config.entrypoint.clone(),
            cmd: self.config.cmd.clone(),
        }
    }
    fn file(&self, id: u64) -> anyhow::Result<FileEntry> {
        ensure!(id < self.files.count, "file ID out of bounds");
        let bytes = self.pages.read(
            "files.bin",
            record_offset(PAGE_BYTES as u64, id, FILE_WIDTH),
            FILE_WIDTH,
        )?;
        ensure!(
            bytes[96..].iter().all(|b| *b == 0),
            "unknown file record flags"
        );
        let parent = u64_at(&bytes, 0)?;
        let inode = u64_at(&bytes, 8)?;
        let nlink = u64_at(&bytes, 16)?;
        let size = u64_at(&bytes, 24)?;
        let mtime = u64_at(&bytes, 32)? as i64;
        let mtime_nsec = u64_at(&bytes, 40)? as i64;
        let mode = u32_at(&bytes, 48)?;
        let uid = u32_at(&bytes, 52)?;
        let gid = u32_at(&bytes, 56)?;
        let kind = u32_at(&bytes, 60)?;
        let po = u64_at(&bytes, 64)?;
        let pl = u32_at(&bytes, 72)? as usize;
        let tl = u32_at(&bytes, 76)? as usize;
        let to = u64_at(&bytes, 80)?;
        let content = u64_at(&bytes, 88)?;
        let valid_arena = |offset: u64, len: usize| {
            offset >= self.files.aux
                && offset
                    .checked_add(len as u64)
                    .is_some_and(|end| end <= self.files.aux + self.files.aux_count)
        };
        ensure!(
            valid_arena(po, pl)
                && pl <= 16384
                && inode > 0
                && nlink > 0
                && (0..1_000_000_000).contains(&mtime_nsec)
                && (parent < self.files.count || id == 0 && parent == NONE),
            "invalid file attributes"
        );
        let path = self.pages.read("files.bin", po, pl)?;
        validate_path(&path)?;
        let kind = match kind {
            0 => "directory",
            1 => "file",
            2 => "symlink",
            3 => "special",
            _ => bail!("invalid file kind"),
        };
        let target = if kind == "symlink" {
            ensure!(
                tl <= 16384 && valid_arena(to, tl),
                "symlink target out of bounds"
            );
            Some(self.pages.read("files.bin", to, tl)?)
        } else {
            ensure!(tl == 0 && to == 0, "unexpected symlink target");
            None
        };
        let content = if kind == "file" {
            ensure!(content < self.contents.count, "content ID out of bounds");
            Some(content)
        } else {
            ensure!(content == NONE, "non-file content reference");
            None
        };
        Ok(FileEntry {
            id,
            parent,
            path,
            metadata: Response::Metadata {
                kind: kind.into(),
                size,
                mode,
                uid,
                gid,
                inode,
                nlink,
                mtime,
                mtime_nsec,
                target,
            },
            content,
        })
    }
    pub(super) fn content(&self, id: u64) -> anyhow::Result<Content> {
        ensure!(id < self.contents.count, "content ID out of bounds");
        let b = self.pages.read(
            "contents.bin",
            record_offset(PAGE_BYTES as u64, id, CONTENT_WIDTH),
            CONTENT_WIDTH,
        )?;
        let c = Content {
            size: u64_at(&b, 32)?,
            first: u64_at(&b, 40)?,
            count: u64_at(&b, 48)?,
        };
        ensure!(
            b[56..].iter().all(|v| *v == 0)
                && c.count == c.size.div_ceil(MAX_READ as u64)
                && c.first
                    .checked_add(c.count)
                    .is_some_and(|end| end <= self.contents.aux_count),
            "invalid chunk table reference"
        );
        Ok(c)
    }
    pub(super) fn chunk(&self, id: u64) -> anyhow::Result<(String, u32)> {
        ensure!(id < self.contents.aux_count, "chunk ID out of bounds");
        let b = self.pages.read(
            "contents.bin",
            record_offset(self.contents.aux, id, CHUNK_WIDTH),
            CHUNK_WIDTH,
        )?;
        let len = u32_at(&b, 32)?;
        ensure!(
            len > 0 && len <= MAX_READ && b[36..].iter().all(|v| *v == 0),
            "invalid chunk record"
        );
        Ok((format!("sha256:{}", crate::util::encode_hex(&b[..32])), len))
    }
    pub(super) fn entry(&self, path: &[u8]) -> anyhow::Result<FileEntry> {
        validate_path(path)?;
        let mut entry = self.file(0)?;
        let mut resolved = Vec::new();
        if path.is_empty() {
            return Ok(entry);
        }
        for component in path.split(|b| *b == b'/') {
            ensure!(
                matches!(&entry.metadata,Response::Metadata{kind,..} if kind=="directory"),
                "path parent is not a directory"
            );
            let key = Key {
                parent: entry.id,
                name: component.to_vec(),
            };
            let (page, slot) = self.seek(&key)?;
            let node = self.node(page)?;
            let Some(record) = node.entries.get(slot) else {
                return Err(not_found().into());
            };
            if record.key != key {
                return Err(not_found().into());
            }
            if !resolved.is_empty() {
                resolved.push(b'/');
            }
            resolved.extend_from_slice(component);
            let child = self.file(record.value)?;
            ensure!(
                child.parent == entry.id && child.path == resolved,
                "index/file relationship mismatch"
            );
            entry = child;
        }
        Ok(entry)
    }
    pub(super) fn list(&self, path: &[u8], offset: usize) -> anyhow::Result<Response> {
        let parent = self.entry(path)?;
        ensure!(
            matches!(&parent.metadata,Response::Metadata{kind,..} if kind=="directory"),
            "list requires a directory"
        );
        let (mut page, mut slot) = if offset == 0 {
            self.seek(&Key {
                parent: parent.id,
                name: Vec::new(),
            })?
        } else {
            ((offset / 256) as u64, offset % 256)
        };
        ensure!(page > 0, "invalid directory cookie");
        let mut names = Vec::new();
        let mut metadata = Vec::new();
        let mut frame_size = 128usize;
        let mut last = None;
        let mut visits = 0;
        loop {
            visits += 1;
            ensure!(visits <= 4, "directory page traversal limit");
            let node = self.node(page)?;
            ensure!(
                node.level == 0 && slot <= node.entries.len(),
                "invalid directory cookie"
            );
            if offset != 0 && names.is_empty() {
                ensure!(
                    node.entries
                        .get(slot)
                        .is_some_and(|r| r.key.parent == parent.id),
                    "cookie belongs to another directory"
                );
            }
            while let Some(record) = node.entries.get(slot) {
                if record.key.parent != parent.id {
                    return Ok(Response::Entries {
                        names,
                        metadata: Some(metadata),
                        next_offset: None,
                    });
                }
                if let Some(previous) = &last {
                    ensure!(previous < &record.key, "unordered directory leaf chain");
                }
                let child = self.file(record.value)?;
                let mut expected = path.to_vec();
                if !expected.is_empty() {
                    expected.push(b'/');
                }
                expected.extend_from_slice(&record.key.name);
                ensure!(
                    child.parent == parent.id && child.path == expected,
                    "directory index relationship mismatch"
                );
                let size = serde_json::to_vec(&record.key.name)?.len()
                    + serde_json::to_vec(&child.metadata)?.len()
                    + 2;
                if names.len() == 256 || frame_size + size > super::super::protocol::MAX_FRAME {
                    ensure!(!names.is_empty(), "directory entry exceeds protocol limit");
                    return Ok(Response::Entries {
                        names,
                        metadata: Some(metadata),
                        next_offset: Some(page as usize * 256 + slot),
                    });
                }
                frame_size += size;
                names.push(record.key.name.clone());
                metadata.push(child.metadata);
                last = Some(record.key.clone());
                slot += 1;
            }
            if node.next == 0 {
                return Ok(Response::Entries {
                    names,
                    metadata: Some(metadata),
                    next_offset: None,
                });
            }
            ensure!(node.next > page, "invalid leaf chain");
            page = node.next;
            slot = 0;
        }
    }
    fn node(&self, id: u64) -> anyhow::Result<Node> {
        ensure!(
            id > 0 && id < self.index.bytes / PAGE_BYTES as u64,
            "index page out of bounds"
        );
        let p = self.pages.page("index.bin", id)?;
        let level = u32_at(&p, 0)?;
        let count = u32_at(&p, 4)? as usize;
        let next = u64_at(&p, 8)?;
        ensure!(
            level <= 32
                && count <= NODE_CAP
                && (count > 0 || id == self.index.root && level == 0)
                && (level == 0 || next == 0)
                && (next == 0 || next < self.index.bytes / PAGE_BYTES as u64),
            "invalid B+tree page"
        );
        let mut entries = Vec::new();
        for slot in 0..count {
            let start = 16 + slot * NODE_WIDTH;
            let b = &p[start..start + NODE_WIDTH];
            let parent = u64_at(b, 0)?;
            let value = u64_at(b, 8)?;
            let len = u16_at(b, 16)? as usize;
            ensure!(
                parent < self.files.count
                    && len > 0
                    && len <= 255
                    && b[18 + len..].iter().all(|v| *v == 0),
                "invalid index key"
            );
            let name = b[18..18 + len].to_vec();
            ensure!(
                !name.contains(&0) && !name.contains(&b'/') && name != b"." && name != b"..",
                "invalid index basename"
            );
            ensure!(
                if level == 0 {
                    value > 0 && value < self.files.count
                } else {
                    value > 0 && value < self.index.bytes / PAGE_BYTES as u64
                },
                "invalid index child"
            );
            let key = Key { parent, name };
            if let Some(prev) = entries.last() {
                let prev: &NodeRecord = prev;
                ensure!(prev.key < key, "unordered index keys");
            }
            entries.push(NodeRecord { key, value });
        }
        ensure!(
            p[16 + count * NODE_WIDTH..].iter().all(|v| *v == 0),
            "unknown index flags"
        );
        Ok(Node {
            level,
            next,
            entries,
        })
    }
    fn seek(&self, key: &Key) -> anyhow::Result<(u64, usize)> {
        let mut page = self.index.root;
        let mut previous = None;
        for _ in 0..33 {
            let node = self.node(page)?;
            if let Some(level) = previous {
                ensure!(node.level + 1 == level, "invalid index tree depth");
            }
            if node.level == 0 {
                let slot = node.entries.partition_point(|record| record.key < *key);
                if slot == node.entries.len() && node.next != 0 {
                    ensure!(node.next > page, "invalid leaf chain");
                    let next = self.node(node.next)?;
                    ensure!(
                        next.level == 0 && next.entries.first().is_some_and(|r| r.key >= *key),
                        "invalid leaf boundary"
                    );
                    return Ok((node.next, 0));
                }
                return Ok((page, slot));
            }
            let slot = node
                .entries
                .partition_point(|record| record.key <= *key)
                .saturating_sub(1);
            previous = Some(node.level);
            page = node.entries[slot].value;
        }
        bail!("index traversal limit")
    }
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    parent: u64,
    name: Vec<u8>,
}
struct NodeRecord {
    key: Key,
    value: u64,
}
struct Node {
    level: u32,
    next: u64,
    entries: Vec<NodeRecord>,
}

pub(super) fn encode(
    entries: &[SourceEntry],
    contents: &BTreeMap<String, SourceContent>,
) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    ensure!(
        !entries.is_empty() && entries.len() <= MAX_ENTRIES && entries[0].path.is_empty(),
        "invalid source file table"
    );
    let paths: HashMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.path.clone(), i as u64))
        .collect();
    ensure!(
        paths.len() == entries.len() && entries.windows(2).all(|p| p[0].path < p[1].path),
        "duplicate or unordered source paths"
    );
    let content_ids: HashMap<_, _> = contents
        .keys()
        .enumerate()
        .map(|(i, d)| (d.clone(), i as u64))
        .collect();
    let aux = table_end(entries.len() as u64, FILE_WIDTH);
    let mut files = vec![0; aux as usize];
    let mut keys = Vec::new();
    for (id, e) in entries.iter().enumerate() {
        validate_path(&e.path)?;
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
        } = &e.metadata
        else {
            bail!("invalid source metadata")
        };
        let parent = if id == 0 {
            NONE
        } else {
            let position = e.path.iter().rposition(|b| *b == b'/');
            let parent_path = position.map_or(&[][..], |p| &e.path[..p]);
            let parent = *paths.get(parent_path).context("missing parent")?;
            ensure!(
                matches!(&entries[parent as usize].metadata,Response::Metadata{kind,..} if kind=="directory"),
                "source parent is not a directory"
            );
            let name = position.map_or(e.path.as_slice(), |p| &e.path[p + 1..]);
            keys.push(NodeRecord {
                key: Key {
                    parent,
                    name: name.into(),
                },
                value: id as u64,
            });
            parent
        };
        let po = files.len() as u64;
        files.extend_from_slice(&e.path);
        let to = if let Some(target) = target {
            ensure!(target.len() <= 16384, "symlink target too long");
            let offset = files.len() as u64;
            files.extend_from_slice(target);
            offset
        } else {
            0
        };
        ensure!(
            files.len() <= MAX_OBJECT,
            "file metadata exceeds size limit"
        );
        let start = record_offset(PAGE_BYTES as u64, id as u64, FILE_WIDTH) as usize;
        let b = &mut files[start..start + FILE_WIDTH];
        put64(b, 0, parent);
        put64(b, 8, *inode);
        put64(b, 16, *nlink);
        put64(b, 24, *size);
        put64(b, 32, *mtime as u64);
        put64(b, 40, *mtime_nsec as u64);
        put32(b, 48, *mode);
        put32(b, 52, *uid);
        put32(b, 56, *gid);
        put32(
            b,
            60,
            match kind.as_str() {
                "directory" => 0,
                "file" => 1,
                "symlink" => 2,
                "special" => 3,
                _ => bail!("invalid source kind"),
            },
        );
        put64(b, 64, po);
        put32(b, 72, e.path.len() as u32);
        put32(b, 76, target.as_ref().map_or(0, |t| t.len()) as u32);
        put64(b, 80, to);
        put64(
            b,
            88,
            e.content
                .as_ref()
                .map(|d| content_ids.get(d).copied().context("missing content"))
                .transpose()?
                .unwrap_or(NONE),
        );
    }
    let arena = files.len() as u64 - aux;
    pad(&mut files)?;
    header(
        &mut files,
        1,
        FILE_WIDTH,
        entries.len() as u64,
        (aux, arena, 1),
        0,
    );
    let aux = table_end(contents.len() as u64, CONTENT_WIDTH);
    let chunk_count: usize = contents.values().map(|c| c.chunks.len()).sum();
    ensure!(chunk_count <= MAX_SPANS, "too many content chunks");
    let mut body = vec![0; (aux + table_bytes(chunk_count as u64, CHUNK_WIDTH)) as usize];
    let mut chunk_id = 0u64;
    let mut objects = BTreeMap::new();
    for (id, (digest, content)) in contents.iter().enumerate() {
        ensure!(
            content.chunks.len() as u64 == content.size.div_ceil(MAX_READ as u64),
            "source chunk count mismatch"
        );
        let start = record_offset(PAGE_BYTES as u64, id as u64, CONTENT_WIDTH) as usize;
        put_digest(&mut body[start..start + 32], digest)?;
        put64(&mut body, start + 32, content.size);
        put64(&mut body, start + 40, chunk_id);
        put64(&mut body, start + 48, content.chunks.len() as u64);
        for (ordinal, (digest, len)) in content.chunks.iter().enumerate() {
            ensure!(
                *len as u64
                    == (content.size - ordinal as u64 * MAX_READ as u64).min(MAX_READ as u64),
                "source chunk length mismatch"
            );
            if let Some(old) = objects.insert(digest.clone(), *len) {
                ensure!(old == *len, "inconsistent object lengths");
            }
            let start = record_offset(aux, chunk_id, CHUNK_WIDTH) as usize;
            put_digest(&mut body[start..start + 32], digest)?;
            put32(&mut body, start + 32, *len);
            chunk_id += 1;
        }
    }
    header(
        &mut body,
        2,
        CONTENT_WIDTH,
        contents.len() as u64,
        (aux, chunk_count as u64, CHUNK_WIDTH),
        0,
    );
    let mut inventory = vec![0; table_end(objects.len() as u64, CHUNK_WIDTH) as usize];
    for (id, (digest, len)) in objects.iter().enumerate() {
        let start = record_offset(PAGE_BYTES as u64, id as u64, CHUNK_WIDTH) as usize;
        put_digest(&mut inventory[start..start + 32], digest)?;
        put32(&mut inventory, start + 32, *len);
    }
    header(
        &mut inventory,
        4,
        CHUNK_WIDTH,
        objects.len() as u64,
        (0, 0, 0),
        0,
    );
    keys.sort_by(|a, b| a.key.cmp(&b.key));
    let mut index = vec![0; PAGE_BYTES];
    let mut layer = Vec::new();
    let leaves = keys.len().div_ceil(NODE_CAP).max(1);
    for (i, group) in keys.chunks(NODE_CAP).enumerate() {
        let page = (index.len() / PAGE_BYTES) as u64;
        let next = if i + 1 < leaves { page + 1 } else { 0 };
        write_node(&mut index, 0, next, group)?;
        layer.push(NodeRecord {
            key: group[0].key.clone(),
            value: page,
        });
    }
    let root = if layer.is_empty() {
        write_node(&mut index, 0, 0, &[])?;
        1
    } else {
        let mut level = 1;
        while layer.len() > 1 {
            let mut upper = Vec::new();
            for group in layer.chunks(NODE_CAP) {
                let page = (index.len() / PAGE_BYTES) as u64;
                write_node(&mut index, level, 0, group)?;
                upper.push(NodeRecord {
                    key: group[0].key.clone(),
                    value: page,
                });
            }
            layer = upper;
            level += 1;
        }
        layer[0].value
    };
    header(
        &mut index,
        3,
        NODE_WIDTH,
        keys.len() as u64,
        (0, 0, 0),
        root,
    );
    let mut binaries = BTreeMap::from([
        ("files.bin".into(), files),
        ("contents.bin".into(), body),
        ("index.bin".into(), index),
        ("objects.bin".into(), inventory),
    ]);
    let mut checksums = vec![0; 80];
    checksums[..8].copy_from_slice(CHECK_MAGIC);
    put32(&mut checksums, 8, PAGE_BYTES as u32);
    put32(&mut checksums, 12, 4);
    for (id, name) in BINARY_NAMES.iter().enumerate() {
        let bytes = &binaries[*name];
        ensure!(
            bytes.len() <= MAX_OBJECT,
            "binary metadata exceeds size limit"
        );
        put64(&mut checksums, 16 + id * 16, bytes.len() as u64);
        put64(
            &mut checksums,
            24 + id * 16,
            bytes.len().div_ceil(PAGE_BYTES) as u64,
        );
        for page in bytes.chunks(PAGE_BYTES) {
            checksums.extend_from_slice(&Sha256::digest(page));
        }
    }
    ensure!(
        checksums.len() <= MAX_CONTROL,
        "checksum catalog exceeds limit"
    );
    binaries.insert("checksums.bin".into(), checksums);
    Ok(binaries)
}
fn write_node(
    index: &mut Vec<u8>,
    level: u32,
    next: u64,
    records: &[NodeRecord],
) -> anyhow::Result<()> {
    ensure!(
        index.len() + PAGE_BYTES <= MAX_OBJECT,
        "index exceeds size limit"
    );
    let start = index.len();
    index.resize(start + PAGE_BYTES, 0);
    let p = &mut index[start..];
    put32(p, 0, level);
    put32(p, 4, records.len() as u32);
    put64(p, 8, next);
    for (id, record) in records.iter().enumerate() {
        let b = &mut p[16 + id * NODE_WIDTH..16 + (id + 1) * NODE_WIDTH];
        put64(b, 0, record.key.parent);
        put64(b, 8, record.value);
        b[16..18].copy_from_slice(&(record.key.name.len() as u16).to_le_bytes());
        b[18..18 + record.key.name.len()].copy_from_slice(&record.key.name);
    }
    Ok(())
}
fn header(
    bytes: &mut [u8],
    kind: u32,
    width: usize,
    count: u64,
    auxiliary: (u64, u64, usize),
    root: u64,
) {
    let (aux, aux_count, aux_width) = auxiliary;
    bytes[..8].copy_from_slice(MAGIC);
    put32(bytes, 8, 1);
    put32(bytes, 12, kind);
    put32(bytes, 16, PAGE_BYTES as u32);
    put32(bytes, 20, width as u32);
    put64(bytes, 24, bytes.len() as u64);
    put64(bytes, 32, count);
    put64(bytes, 40, PAGE_BYTES as u64);
    put64(bytes, 48, aux);
    put64(bytes, 56, aux_count);
    put32(bytes, 64, aux_width as u32);
    put64(bytes, 72, root);
}
fn table_bytes(count: u64, width: usize) -> u64 {
    count.div_ceil((PAGE_BYTES / width) as u64) * PAGE_BYTES as u64
}
fn table_end(count: u64, width: usize) -> u64 {
    PAGE_BYTES as u64 + table_bytes(count, width)
}
fn record_offset(base: u64, id: u64, width: usize) -> u64 {
    let slots = (PAGE_BYTES / width) as u64;
    base + (id / slots) * PAGE_BYTES as u64 + (id % slots) * width as u64
}
fn pad(bytes: &mut Vec<u8>) -> anyhow::Result<()> {
    let len = bytes.len().div_ceil(PAGE_BYTES) * PAGE_BYTES;
    ensure!(len <= MAX_OBJECT, "metadata exceeds size limit");
    bytes.resize(len, 0);
    Ok(())
}
fn put_digest(target: &mut [u8], digest: &str) -> anyhow::Result<()> {
    let hex = crate::image::oci::digest_hex(digest)?;
    for (i, byte) in target.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(())
}
fn u16_at(bytes: &[u8], offset: usize) -> anyhow::Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .context("truncated integer")?
            .try_into()?,
    ))
}
fn u32_at(bytes: &[u8], offset: usize) -> anyhow::Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("truncated integer")?
            .try_into()?,
    ))
}
fn u64_at(bytes: &[u8], offset: usize) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .context("truncated integer")?
            .try_into()?,
    ))
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod malformed_tests {
    use super::*;
    fn image(
        mutate: impl FnOnce(&mut BTreeMap<String, Vec<u8>>),
    ) -> (tempfile::TempDir, LoadedImage) {
        let temp = tempfile::tempdir().unwrap();
        let root = Response::Metadata {
            kind: "directory".into(),
            size: 0,
            mode: 0o40755,
            uid: 0,
            gid: 0,
            inode: 1,
            nlink: 2,
            mtime: 0,
            mtime_nsec: 0,
            target: None,
        };
        let file = Response::Metadata {
            kind: "file".into(),
            size: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            inode: 2,
            nlink: 1,
            mtime: 0,
            mtime_nsec: 0,
            target: None,
        };
        let digest = hash(&[]);
        let entries = [
            SourceEntry {
                path: Vec::new(),
                metadata: root,
                content: None,
            },
            SourceEntry {
                path: b"a".to_vec(),
                metadata: file,
                content: Some(digest.clone()),
            },
        ];
        let contents = BTreeMap::from([(
            digest,
            SourceContent {
                size: 0,
                chunks: vec![],
            },
        )]);
        let mut objects = encode(&entries, &contents).unwrap();
        mutate(&mut objects);
        let mut catalog = vec![0; 80];
        catalog[..8].copy_from_slice(CHECK_MAGIC);
        put32(&mut catalog, 8, PAGE_BYTES as u32);
        put32(&mut catalog, 12, 4);
        for (i, name) in BINARY_NAMES.iter().enumerate() {
            let bytes = &objects[*name];
            put64(&mut catalog, 16 + i * 16, bytes.len() as u64);
            put64(
                &mut catalog,
                24 + i * 16,
                bytes.len().div_ceil(PAGE_BYTES) as u64,
            );
            for page in bytes.chunks(PAGE_BYTES) {
                catalog.extend_from_slice(&Sha256::digest(page));
            }
        }
        let storage = Storage::filesystem(temp.path().join("objects"), true).unwrap();
        let handle = Handle {
            image_key: "a".repeat(64),
            platform: "linux-amd64".into(),
            revision: "b".repeat(64),
        };
        let mut metadata = BTreeMap::new();
        for (name, bytes) in objects {
            metadata.insert(
                name.clone(),
                Descriptor {
                    sha256: hash(&bytes),
                    bytes: bytes.len() as u64,
                },
            );
            storage
                .put(&format!("{}/{name}", handle.prefix()), bytes, true)
                .unwrap();
        }
        let commit = Commit {
            format_version: 2,
            image_key: handle.image_key.clone(),
            platform: handle.platform.clone(),
            manifest_digest: format!("sha256:{}", "c".repeat(64)),
            metadata,
        };
        let config = Configuration {
            format_version: 2,
            architecture: "amd64".into(),
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
            totals: ImageTotals { files: 1, bytes: 0 },
        };
        let image = LoadedImage::new(storage, None, handle, commit, config, &catalog).unwrap();
        (temp, image)
    }
    #[test]
    fn authenticated_but_invalid_content_references_are_rejected() {
        let (_temp, image) = image(|objects| {
            put64(
                objects.get_mut("files.bin").unwrap(),
                PAGE_BYTES + FILE_WIDTH + 88,
                NONE,
            );
        });
        assert!(
            image
                .entry(b"a")
                .unwrap_err()
                .to_string()
                .contains("content ID")
        );
    }
    #[test]
    fn authenticated_leaf_cycles_cannot_hang_a_missing_path_lookup() {
        let (_temp, image) = image(|objects| {
            put64(objects.get_mut("index.bin").unwrap(), PAGE_BYTES + 8, 1);
        });
        assert!(
            image
                .entry(b"z")
                .unwrap_err()
                .to_string()
                .contains("leaf chain")
        );
    }
}
