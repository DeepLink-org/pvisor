//! V2 directory-local hash radix index. Attributes and content references are
//! inline in independently authenticated/compressed leaves, never a global table.
pub(super) use super::frames::PAGE_BYTES;
use super::frames::{self, Digest, FrameRef};
use super::*;
use bincode::{Decode, Encode};

pub(super) struct SourceEntry {
    pub path: Vec<u8>,
    pub metadata: Response,
    pub content: Option<String>,
}
pub(super) struct SourceContent {
    pub size: u64,
    pub chunks: Vec<(String, u32)>,
}
#[derive(Clone, Debug, Encode, Decode)]
pub(super) enum Content {
    Empty,
    Packed(super::content::Packed),
    Chunks(FrameRef),
    PackedFrame(super::aggregate::Packed),
}

pub(super) enum LocatedChunk {
    Legacy(Digest, u32),
    V1(super::aggregate::Chunk),
}

impl LocatedChunk {
    pub(super) fn len(&self) -> u32 {
        match self {
            Self::Legacy(_, len) => *len,
            Self::V1(chunk) => chunk.bytes,
        }
    }
}
#[derive(Clone, Debug, Encode, Decode)]
struct Attributes {
    kind: u8,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    inode: u64,
    nlink: u64,
    mtime: i64,
    mtime_nsec: i64,
    target: Option<Vec<u8>>,
}
impl Attributes {
    fn from_response(value: &Response) -> anyhow::Result<Self> {
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
        } = value
        else {
            bail!("invalid source metadata")
        };
        let kind = match kind.as_str() {
            "directory" => 0,
            "file" => 1,
            "symlink" => 2,
            "special" => 3,
            _ => bail!("unknown source file kind"),
        };
        Ok(Self {
            kind,
            size: *size,
            mode: *mode,
            uid: *uid,
            gid: *gid,
            inode: *inode,
            nlink: *nlink,
            mtime: *mtime,
            mtime_nsec: *mtime_nsec,
            target: target.clone(),
        })
    }
    fn response(&self) -> Response {
        Response::Metadata {
            kind: ["directory", "file", "symlink", "special"][self.kind as usize].into(),
            size: self.size,
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            inode: self.inode,
            nlink: self.nlink,
            mtime: self.mtime,
            mtime_nsec: self.mtime_nsec,
            target: self.target.clone(),
        }
    }
}
#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct Record {
    pub name: Vec<u8>,
    attrs: Attributes,
    pub directory: Option<FrameRef>,
    pub content: Option<Content>,
    pub digest: Option<Digest>,
}
impl Record {
    fn validate(&self, root: bool) -> anyhow::Result<()> {
        ensure!(
            self.attrs.kind <= 3
                && self.attrs.inode > 0
                && self.attrs.nlink > 0
                && self.attrs.inode <= MAX_ENTRIES as u64
                && (0..1_000_000_000).contains(&self.attrs.mtime_nsec),
            "invalid file attributes"
        );
        if root {
            ensure!(
                self.name.is_empty() && self.attrs.kind == 0 && self.attrs.inode == 1,
                "invalid root record"
            );
        } else {
            validate_basename(&self.name)?;
        }
        ensure!(
            self.attrs
                .target
                .as_ref()
                .is_none_or(|v| v.len() <= 16384 && !v.contains(&0))
                && (self.attrs.target.is_some() == (self.attrs.kind == 2)),
            "invalid symlink target"
        );
        ensure!(
            self.attrs.kind == 0 || self.directory.is_none(),
            "non-directory child index"
        );
        ensure!(
            (self.attrs.kind == 1) == self.content.is_some()
                && self.content.is_some() == self.digest.is_some(),
            "invalid content ID"
        );
        if let Some(content) = &self.content {
            ensure!(
                matches!(content, Content::Empty) == (self.attrs.size == 0),
                "empty content size mismatch"
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub(super) struct FileEntry {
    #[cfg(test)]
    pub path: Vec<u8>,
    pub metadata: Response,
    pub content: Option<Content>,
    pub digest: Option<Digest>,
    record: Record,
}
impl FileEntry {
    fn new(_path: Vec<u8>, record: Record) -> Self {
        Self {
            #[cfg(test)]
            path: _path,
            metadata: record.attrs.response(),
            content: record.content.clone(),
            digest: record.digest,
            record,
        }
    }
}
#[derive(Clone, Debug, Encode, Decode)]
pub(super) struct RangeChild {
    first: Vec<u8>,
    reference: FrameRef,
}
#[derive(Clone, Debug, Encode, Decode)]
pub(super) enum Body {
    Leaf {
        directory: u64,
        depth: u8,
        prefix: u64,
        entries: Vec<Record>,
    },
    Branch {
        directory: u64,
        depth: u8,
        prefix: u64,
        children: Box<[Option<FrameRef>; 16]>,
    },
    Range {
        directory: u64,
        depth: u8,
        prefix: u64,
        children: Vec<RangeChild>,
    },
    Chunks {
        first: u64,
        chunks: Vec<(Digest, u32)>,
    },
    ChunkBranch {
        first: u64,
        children: Vec<FrameRef>,
    },
    BlobChunks {
        first: u64,
        chunks: Vec<super::aggregate::Chunk>,
    },
}
pub(super) struct Node {
    body: Body,
    reference: FrameRef,
    retained: usize,
}
impl Node {
    fn retained_bytes(&self) -> usize {
        self.retained
    }
    fn allocation_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match &self.body {
                Body::Leaf { entries, .. } => {
                    entries.capacity() * std::mem::size_of::<Record>()
                        + entries
                            .iter()
                            .map(|r| {
                                r.name.capacity() + r.attrs.target.as_ref().map_or(0, Vec::capacity)
                            })
                            .sum::<usize>()
                }
                Body::Range { children, .. } => {
                    children.capacity() * std::mem::size_of::<RangeChild>()
                        + children.iter().map(|r| r.first.capacity()).sum::<usize>()
                }
                Body::Chunks { chunks, .. } => {
                    chunks.capacity() * std::mem::size_of::<(Digest, u32)>()
                }
                Body::BlobChunks { chunks, .. } => {
                    chunks.capacity() * super::aggregate::Chunk::allocation_bytes()
                }
                Body::ChunkBranch { children, .. } => {
                    children.capacity() * std::mem::size_of::<FrameRef>()
                }
                Body::Branch { .. } => std::mem::size_of::<[Option<FrameRef>; 16]>(),
            }
    }
}
struct NodeCache {
    entries: LruCache<u64, Arc<Node>>,
    bytes: usize,
}
impl NodeCache {
    fn new() -> Self {
        Self {
            entries: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            bytes: 0,
        }
    }
    fn get(&mut self, key: &u64) -> Option<&Arc<Node>> {
        self.entries.get(key)
    }
    fn pop_lru(&mut self) -> Option<(u64, Arc<Node>)> {
        let item = self.entries.pop_lru()?;
        self.bytes -= item.1.retained_bytes();
        Some(item)
    }
    fn put(&mut self, key: u64, node: Arc<Node>) {
        if let Some(old) = self.entries.pop(&key) {
            self.bytes -= old.retained_bytes();
        }
        while self.entries.len() >= self.entries.cap().get() {
            self.pop_lru();
        }
        self.bytes += node.retained_bytes();
        self.entries.put(key, node);
    }
}
pub(super) struct LoadedImage {
    pub commit: Commit,
    config: Configuration,
    handle: Handle,
    storage: Storage,
    local: Option<PathBuf>,
    root: Record,
    cache: Mutex<NodeCache>,
    flights: frames::Flights,
}
impl LoadedImage {
    pub(super) fn new(
        storage: Storage,
        local: Option<PathBuf>,
        handle: Handle,
        commit: Commit,
        config: Configuration,
        root: &[u8],
    ) -> anyhow::Result<Self> {
        let root: Record = frames::decode(root)?;
        root.validate(true)?;
        let image = Self {
            commit,
            config,
            handle,
            storage,
            local,
            root,
            cache: Mutex::new(NodeCache::new()),
            flights: frames::Flights::default(),
        };
        if let Some(reference) = &image.root.directory {
            image.directory_node(reference, image.root.attrs.inode, 0, 0)?;
        }
        Ok(image)
    }
    pub(super) fn prepared(&self) -> Response {
        Response::Prepared {
            image_handle: self.handle.encode(),
            metadata_generation: format!("sha256:{}", self.handle.revision),
            metadata_pages: true,
            totals: Some(self.config.totals),
            digest: self.commit.manifest_digest.clone(),
            architecture: self.config.architecture.clone(),
            env: self.config.env.clone(),
            entrypoint: self.config.entrypoint.clone(),
            cmd: self.config.cmd.clone(),
        }
    }
    fn node(&self, reference: &FrameRef) -> anyhow::Result<Arc<Node>> {
        frames::validate(
            reference,
            self.commit.metadata["index.bin"].bytes,
            PAGE_BYTES,
        )?;
        if let Some(node) = self.cache.lock().unwrap().get(&reference.offset).cloned() {
            ensure!(
                node.reference == *reference,
                "inconsistent cached index reference"
            );
            return Ok(node);
        }
        let owner = self.flights.acquire(&reference.offset.to_string())?;
        let _miss = owner.lock().unwrap();
        if let Some(node) = self.cache.lock().unwrap().get(&reference.offset).cloned() {
            ensure!(
                node.reference == *reference,
                "inconsistent cached index reference"
            );
            return Ok(node);
        }
        let local = self.local.as_ref().map(|root| {
            root.join("pages")
                .join(&self.handle.revision)
                .join("index.bin")
                .join(reference.offset.to_string())
        });
        let valid = |v: &[u8]| {
            v.len() == reference.encoded as usize && frames::digest(v) == reference.digest
        };
        let encoded = if let Some(bytes) = local
            .as_ref()
            .map(|p| read_local(p, frames::MAX_FRAME_ENCODED))
            .transpose()?
            .flatten()
            .filter(|v| valid(v))
        {
            bytes
        } else {
            let bytes = self.storage.verified(
                &format!("{}/index.bin", self.handle.prefix()),
                Some(reference.offset..reference.offset + reference.encoded as u64),
                reference.encoded as usize,
                |bytes| {
                    ensure!(valid(bytes), "cache metadata page digest mismatch");
                    Ok(())
                },
            )?;
            if let Some(path) = local {
                cache_local(&path, &bytes);
            }
            bytes
        };
        let body: Body = frames::unpack_verified(reference, &encoded)?;
        let mut node = Node {
            body,
            reference: reference.clone(),
            retained: 0,
        };
        node.retained = node.allocation_bytes();
        self.validate_body(reference, &node.body)?;
        let mut cache = self.cache.lock().unwrap();
        if let Some(existing) = cache.get(&reference.offset) {
            ensure!(
                existing.reference == *reference,
                "inconsistent concurrent index reference"
            );
            return Ok(existing.clone());
        }
        // Decoded records can occupy more memory than their compact wire form.
        // Bound retained bytes as well as the number of frames.
        let retained = node.retained_bytes();
        while cache.bytes + retained > 16 * 1024 * 1024 {
            if cache.pop_lru().is_none() {
                return Ok(Arc::new(node));
            }
        }
        let node = Arc::new(node);
        cache.put(reference.offset, node.clone());
        Ok(node)
    }
    fn validate_body(&self, reference: &FrameRef, body: &Body) -> anyhow::Result<()> {
        let child = |r: &FrameRef| -> anyhow::Result<()> {
            frames::validate(r, reference.offset, PAGE_BYTES)?;
            ensure!(r.count > 0, "empty index child");
            Ok(())
        };
        match body {
            Body::Leaf {
                directory,
                depth,
                prefix,
                entries,
            } => {
                validate_route(*directory, *depth, *prefix)?;
                ensure!(
                    entries.len() as u64 == reference.count,
                    "leaf entry count mismatch"
                );
                let mut previous: Option<&[u8]> = None;
                for entry in entries {
                    entry.validate(false)?;
                    match entry.content.as_ref() {
                        Some(Content::Packed(_)) => {
                            ensure!(self.commit.format_version == 2, "V2 pack in Image Pack V1");
                        }
                        Some(Content::PackedFrame(_)) => {
                            ensure!(self.commit.format_version == 1, "Image Pack frame in V2");
                        }
                        _ => {}
                    }
                    ensure!(
                        previous.is_none_or(|p| name_order(p, &entry.name).is_lt()),
                        "unordered or duplicate index names"
                    );
                    ensure!(
                        hash_prefix(route_hash(&entry.name), *depth) == *prefix,
                        "index/file relationship mismatch"
                    );
                    if let Some(r) = &entry.directory {
                        child(r)?;
                    }
                    if let Some(Content::Chunks(r)) = &entry.content {
                        child(r)?;
                    }
                    previous = Some(&entry.name);
                }
            }
            Body::Branch {
                directory,
                depth,
                prefix,
                children,
            } => {
                validate_route(*directory, *depth, *prefix)?;
                ensure!(
                    *depth < 16 && children.iter().flatten().count() > 0,
                    "invalid index tree depth"
                );
                let mut count = 0u64;
                for r in children.iter().flatten() {
                    child(r)?;
                    count = count.checked_add(r.count).context("entry count overflow")?;
                }
                ensure!(count == reference.count, "branch entry count mismatch");
            }
            Body::Range {
                directory,
                depth,
                prefix,
                children,
            } => {
                validate_route(*directory, *depth, *prefix)?;
                ensure!(
                    children.len() >= 2 && children.len() <= 16,
                    "invalid overflow fanout"
                );
                let mut previous: Option<&[u8]> = None;
                let mut count = 0u64;
                for item in children {
                    validate_basename(&item.first)?;
                    ensure!(
                        previous.is_none_or(|p| p < item.first.as_slice()),
                        "unordered overflow names"
                    );
                    previous = Some(&item.first);
                    child(&item.reference)?;
                    count = count
                        .checked_add(item.reference.count)
                        .context("entry count overflow")?;
                }
                ensure!(count == reference.count, "overflow entry count mismatch");
            }
            Body::Chunks { chunks, .. } => {
                ensure!(
                    self.commit.format_version == 2
                        && chunks.len() as u64 == reference.count
                        && !chunks.is_empty(),
                    "invalid chunk leaf"
                );
                ensure!(
                    chunks
                        .iter()
                        .all(|(_, length)| (1..=CHUNK_BYTES).contains(length)),
                    "invalid chunk length"
                );
            }
            Body::BlobChunks { chunks, .. } => {
                ensure!(
                    self.commit.format_version == 1
                        && chunks.len() as u64 == reference.count
                        && (1..=256).contains(&chunks.len())
                        && chunks
                            .iter()
                            .all(|chunk| (1..=CHUNK_BYTES).contains(&chunk.bytes)),
                    "invalid Image Pack chunk leaf"
                );
            }
            Body::ChunkBranch { children, .. } => {
                ensure!(
                    children.len() >= 2 && children.len() <= 16,
                    "invalid chunk fanout"
                );
                let mut count = 0u64;
                for r in children {
                    child(r)?;
                    count = count.checked_add(r.count).context("chunk count overflow")?;
                }
                ensure!(count == reference.count, "chunk branch count mismatch");
            }
        }
        Ok(())
    }
    fn directory_node(
        &self,
        reference: &FrameRef,
        directory: u64,
        depth: u8,
        prefix: u64,
    ) -> anyhow::Result<Arc<Node>> {
        let node = self.node(reference)?;
        let (actual, d, p) = match &node.body {
            Body::Leaf {
                directory,
                depth,
                prefix,
                ..
            }
            | Body::Branch {
                directory,
                depth,
                prefix,
                ..
            }
            | Body::Range {
                directory,
                depth,
                prefix,
                ..
            } => (*directory, *depth, *prefix),
            _ => bail!("non-directory index node"),
        };
        ensure!(
            (actual, d, p) == (directory, depth, prefix),
            "directory index relationship mismatch"
        );
        Ok(node)
    }
    fn lookup(&self, parent: &Record, name: &[u8]) -> anyhow::Result<Record> {
        let mut reference = parent.directory.clone().ok_or_else(not_found)?;
        let hash = route_hash(name);
        let (mut depth, mut prefix) = (0, 0);
        for _ in 0..48 {
            let node = self.directory_node(&reference, parent.attrs.inode, depth, prefix)?;
            match &node.body {
                Body::Leaf { entries, .. } => {
                    return entries
                        .binary_search_by(|r| name_order(&r.name, name))
                        .map(|i| entries[i].clone())
                        .map_err(|_| not_found().into());
                }
                Body::Branch { children, .. } => {
                    let slot = nibble(hash, depth);
                    reference = children[slot].clone().ok_or_else(not_found)?;
                    depth += 1;
                    prefix = hash_prefix(hash, depth);
                }
                Body::Range { children, .. } => {
                    let index = children
                        .partition_point(|v| v.first.as_slice() <= name)
                        .saturating_sub(1);
                    reference = children[index].reference.clone();
                }
                _ => unreachable!(),
            }
        }
        bail!("index traversal limit")
    }
    pub(super) fn entry(&self, path: &[u8]) -> anyhow::Result<FileEntry> {
        validate_path(path)?;
        let mut record = self.root.clone();
        if !path.is_empty() {
            for name in path.split(|b| *b == b'/') {
                ensure!(record.attrs.kind == 0, "path parent is not a directory");
                record = self.lookup(&record, name)?;
            }
        }
        Ok(FileEntry::new(path.to_vec(), record))
    }
    fn collect(
        &self,
        reference: &FrameRef,
        route: (u64, u8, u64),
        skip: &mut u64,
        output: &mut Vec<Record>,
        visits: &mut usize,
    ) -> anyhow::Result<()> {
        let (directory, depth, prefix) = route;
        if output.len() >= 256 {
            return Ok(());
        }
        if *skip >= reference.count {
            *skip -= reference.count;
            return Ok(());
        }
        *visits += 1;
        ensure!(*visits <= 4096, "directory page traversal limit");
        let node = self.directory_node(reference, directory, depth, prefix)?;
        match &node.body {
            Body::Leaf { entries, .. } => {
                output.extend(
                    entries
                        .iter()
                        .skip(*skip as usize)
                        .take(256 - output.len())
                        .cloned(),
                );
                *skip = 0;
            }
            Body::Branch { children, .. } => {
                for (slot, child) in children.iter().enumerate() {
                    if let Some(child) = child {
                        self.collect(
                            child,
                            (directory, depth + 1, prefix << 4 | slot as u64),
                            skip,
                            output,
                            visits,
                        )?;
                    }
                }
            }
            Body::Range { children, .. } => {
                for child in children {
                    self.collect(
                        &child.reference,
                        (directory, depth, prefix),
                        skip,
                        output,
                        visits,
                    )?;
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    pub(super) fn list(&self, path: &[u8], cookie: usize) -> anyhow::Result<Response> {
        let parent = self.entry(path)?.record;
        ensure!(parent.attrs.kind == 0, "list requires a directory");
        let start = if cookie == 0 {
            0
        } else {
            ensure!(
                (cookie as u64 >> 32) == parent.attrs.inode && cookie as u32 > 0,
                "cookie belongs to another directory"
            );
            (cookie as u32 - 1) as u64
        };
        let mut records = Vec::new();
        let total = parent.directory.as_ref().map_or(0, |v| v.count);
        ensure!(
            start <= total && (cookie == 0 || start < total),
            "invalid directory cookie"
        );
        if let Some(root) = &parent.directory {
            let mut skip = start;
            self.collect(
                root,
                (parent.attrs.inode, 0, 0),
                &mut skip,
                &mut records,
                &mut 0,
            )?;
        }
        let mut names = Vec::new();
        let mut metadata = Vec::new();
        let mut bytes = 128;
        for record in records {
            let response = record.attrs.response();
            let length =
                serde_json::to_vec(&record.name)?.len() + serde_json::to_vec(&response)?.len() + 2;
            if bytes + length > super::super::protocol::MAX_FRAME {
                ensure!(!names.is_empty(), "directory entry exceeds protocol limit");
                break;
            }
            bytes += length;
            names.push(record.name);
            metadata.push(response);
        }
        let end = start + names.len() as u64;
        let next_offset =
            (end < total).then_some(((parent.attrs.inode << 32) | (end + 1)) as usize);
        Ok(Response::Entries {
            names,
            metadata,
            next_offset,
        })
    }
    pub(super) fn chunk(&self, root: &FrameRef, ordinal: u64) -> anyhow::Result<LocatedChunk> {
        ensure!(ordinal < root.count, "file chunk index out of range");
        let mut reference = root.clone();
        let mut first = 0;
        for _ in 0..32 {
            let node = self.node(&reference)?;
            match &node.body {
                Body::Chunks {
                    first: actual,
                    chunks,
                } => {
                    ensure!(*actual == first, "chunk range mismatch");
                    let (digest, bytes) = chunks[(ordinal - first) as usize];
                    return Ok(LocatedChunk::Legacy(digest, bytes));
                }
                Body::BlobChunks {
                    first: actual,
                    chunks,
                } => {
                    ensure!(*actual == first, "chunk range mismatch");
                    return Ok(LocatedChunk::V1(chunks[(ordinal - first) as usize].clone()));
                }
                Body::ChunkBranch {
                    first: actual,
                    children,
                } => {
                    ensure!(*actual == first, "chunk branch range mismatch");
                    let mut selected = None;
                    for child in children {
                        if ordinal < first + child.count {
                            selected = Some(child.clone());
                            break;
                        }
                        first += child.count;
                    }
                    reference = selected.context("missing chunk child")?;
                }
                _ => bail!("invalid content chunk index"),
            }
        }
        bail!("content traversal limit")
    }
    pub(super) fn legacy_dependencies(
        &self,
        cache: &PortableCache,
    ) -> anyhow::Result<BTreeMap<String, super::content::LegacyObject>> {
        let mut packs = Vec::new();
        let mut objects = BTreeMap::new();
        let mut trees = BTreeMap::new();
        let mut spans = 0;
        self.walk_files(|entry| {
            match entry.content {
                Some(Content::Packed(packed)) => packs.push(packed),
                Some(Content::Chunks(root)) => {
                    let size = entry.record.attrs.size;
                    if !visit_chunk_tree(&mut trees, &root, size, &mut spans)? {
                        return Ok(());
                    }
                    for ordinal in 0..root.count {
                        let LocatedChunk::Legacy(digest, length) = self.chunk(&root, ordinal)?
                        else {
                            bail!("Image Pack chunk in prerelease V2");
                        };
                        check_chunk_len(length, size, ordinal)?;
                        let key = data_key(&frames::digest_string(&digest))?;
                        let object = super::content::LegacyObject::Chunk(digest, length);
                        ensure!(
                            *objects.entry(key).or_insert(object) == object,
                            "inconsistent legacy chunk reference"
                        );
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        objects.extend(super::content::validate_public_packs(cache, packs)?);
        Ok(objects)
    }

    pub(super) fn reachable_blobs(&self) -> anyhow::Result<super::aggregate::Reachable> {
        let mut output = super::aggregate::Reachable::default();
        let mut trees = BTreeMap::new();
        let mut spans = 0;
        self.walk_files(|entry| {
            match entry.content {
                Some(Content::PackedFrame(packed)) => {
                    output.packed(
                        packed,
                        entry.digest.context("missing file digest")?,
                        entry.record.attrs.size,
                    )?;
                }
                Some(Content::Chunks(root)) => {
                    let size = entry.record.attrs.size;
                    if !visit_chunk_tree(&mut trees, &root, size, &mut spans)? {
                        return Ok(());
                    }
                    for ordinal in 0..root.count {
                        let LocatedChunk::V1(chunk) = self.chunk(&root, ordinal)? else {
                            bail!("legacy chunk in Image Pack V1");
                        };
                        check_chunk_len(chunk.bytes, size, ordinal)?;
                        output.chunk(chunk)?;
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(output)
    }

    fn walk_files(
        &self,
        mut visit: impl FnMut(FileEntry) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let mut pending = vec![Vec::new()];
        let mut visited = 0;
        while let Some(path) = pending.pop() {
            let mut cursor = 0;
            loop {
                let Response::Entries {
                    names,
                    metadata,
                    next_offset,
                } = self.list(&path, cursor)?
                else {
                    unreachable!()
                };
                for (name, metadata) in names.into_iter().zip(metadata) {
                    visited += 1;
                    ensure!(visited <= MAX_ENTRIES, "image entry limit");
                    let mut child = path.clone();
                    if !child.is_empty() {
                        child.push(b'/');
                    }
                    child.extend_from_slice(&name);
                    if matches!(metadata, Response::Metadata { kind, .. } if kind == "directory") {
                        pending.push(child);
                    } else {
                        visit(self.entry(&child)?)?;
                    }
                }
                if let Some(next) = next_offset {
                    cursor = next;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn cached_page_count(&self) -> usize {
        self.cache.lock().unwrap().entries.len()
    }
    #[cfg(test)]
    pub(super) fn resize_metadata_cache(&self, entries: usize) {
        let mut cache = self.cache.lock().unwrap();
        while cache.entries.len() > entries {
            cache.pop_lru();
        }
        cache.entries.resize(NonZeroUsize::new(entries).unwrap());
    }
    #[cfg(test)]
    pub(super) fn clear_metadata_cache(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.entries.clear();
        cache.bytes = 0;
    }
    #[cfg(test)]
    pub(super) fn cached_nodes(&self) -> Vec<(u64, Arc<Node>, usize)> {
        self.cache
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|(id, node)| (*id, node.clone(), node.retained_bytes()))
            .collect()
    }
}

fn visit_chunk_tree(
    seen: &mut BTreeMap<u64, (FrameRef, u64)>,
    root: &FrameRef,
    size: u64,
    spans: &mut u64,
) -> anyhow::Result<bool> {
    ensure!(
        root.count == size.div_ceil(u64::from(CHUNK_BYTES)),
        "file chunk count mismatch"
    );
    if let Some((previous, previous_size)) = seen.get(&root.offset) {
        ensure!(
            previous == root && *previous_size == size,
            "inconsistent shared chunk tree"
        );
        return Ok(false);
    }
    *spans += root.count;
    ensure!(*spans <= MAX_SPANS as u64, "image span limit");
    seen.insert(root.offset, (root.clone(), size));
    Ok(true)
}

pub(super) fn check_chunk_len(length: u32, size: u64, ordinal: u64) -> anyhow::Result<()> {
    let width = u64::from(CHUNK_BYTES);
    ensure!(
        u64::from(length) == (size - ordinal * width).min(width),
        "invalid file chunk length"
    );
    Ok(())
}

pub(in crate::image::cache) fn name_order(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    route_hash(a).cmp(&route_hash(b)).then_with(|| a.cmp(b))
}
pub(super) fn route_hash(name: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64_with_seed(name, 0x7076_6973_6f72_7632)
}
fn hash_prefix(hash: u64, depth: u8) -> u64 {
    if depth == 0 {
        0
    } else {
        hash >> (64 - depth as u32 * 4)
    }
}
fn nibble(hash: u64, depth: u8) -> usize {
    ((hash >> (60 - depth as u32 * 4)) & 15) as usize
}
fn validate_route(directory: u64, depth: u8, prefix: u64) -> anyhow::Result<()> {
    ensure!(
        directory > 0
            && directory <= MAX_ENTRIES as u64
            && depth <= 16
            && (depth == 16 || prefix < (1u64 << (depth as u32 * 4))),
        "invalid hash route"
    );
    Ok(())
}
fn validate_basename(name: &[u8]) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 255
            && !name.contains(&0)
            && !name.contains(&b'/')
            && name != b"."
            && name != b"..",
        "invalid index basename"
    );
    Ok(())
}
fn tree(
    writer: &mut frames::Writer,
    directory: u64,
    depth: u8,
    prefix: u64,
    mut records: Vec<Record>,
) -> anyhow::Result<FrameRef> {
    records.sort_by(|a, b| name_order(&a.name, &b.name));
    let count = records.len() as u64;
    let body = Body::Leaf {
        directory,
        depth,
        prefix,
        entries: records,
    };
    if frames::encode(&body)?.len() <= PAGE_BYTES {
        return writer.push(&body, count);
    }
    let Body::Leaf { entries, .. } = body else {
        unreachable!()
    };
    ensure!(entries.len() > 1, "single index record exceeds frame limit");
    if depth == 16 {
        return overflow(writer, directory, depth, prefix, entries);
    }
    let mut groups: [Vec<Record>; 16] = std::array::from_fn(|_| Vec::new());
    for entry in entries {
        groups[nibble(route_hash(&entry.name), depth)].push(entry);
    }
    let mut children = std::array::from_fn(|_| None);
    for (slot, group) in groups.into_iter().enumerate() {
        if !group.is_empty() {
            children[slot] = Some(tree(
                writer,
                directory,
                depth + 1,
                prefix << 4 | slot as u64,
                group,
            )?);
        }
    }
    writer.push(
        &Body::Branch {
            directory,
            depth,
            prefix,
            children: Box::new(children),
        },
        count,
    )
}
fn overflow(
    writer: &mut frames::Writer,
    directory: u64,
    depth: u8,
    prefix: u64,
    entries: Vec<Record>,
) -> anyhow::Result<FrameRef> {
    let count = entries.len() as u64;
    let mut children = Vec::new();
    let size = entries.len().div_ceil(16).max(1);
    for group in entries.chunks(size) {
        let body = Body::Leaf {
            directory,
            depth,
            prefix,
            entries: group.to_vec(),
        };
        let reference = if frames::encode(&body)?.len() <= PAGE_BYTES {
            writer.push(&body, group.len() as u64)?
        } else {
            ensure!(group.len() > 1, "single index record exceeds limit");
            overflow(writer, directory, depth, prefix, group.to_vec())?
        };
        children.push(RangeChild {
            first: group[0].name.clone(),
            reference,
        });
    }
    writer.push(
        &Body::Range {
            directory,
            depth,
            prefix,
            children,
        },
        count,
    )
}
fn chunks(
    writer: &mut frames::Writer,
    values: &[super::aggregate::Chunk],
    first: u64,
) -> anyhow::Result<FrameRef> {
    let count = values.len() as u64;
    if values.len() <= 256 {
        return writer.push(
            &Body::BlobChunks {
                first,
                chunks: values.to_vec(),
            },
            count,
        );
    }
    let size = values.len().div_ceil(16);
    let mut children = Vec::new();
    for (i, group) in values.chunks(size).enumerate() {
        children.push(chunks(writer, group, first + (i * size) as u64)?);
    }
    writer.push(&Body::ChunkBranch { first, children }, count)
}
pub(super) fn encode(
    entries: &[SourceEntry],
    contents: &BTreeMap<String, SourceContent>,
    placement: &super::aggregate::Plan,
) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    ensure!(
        !entries.is_empty() && entries[0].path.is_empty(),
        "missing source root"
    );
    let mut writer = frames::Writer::default();
    let mut locations = BTreeMap::new();
    for (digest, file_chunks) in &placement.large {
        locations.insert(
            digest,
            Content::Chunks(chunks(&mut writer, file_chunks, 0)?),
        );
    }
    let mut directories: BTreeMap<Vec<u8>, Vec<Record>> = BTreeMap::new();
    for entry in entries {
        validate_path(&entry.path)?;
        if matches!(&entry.metadata, Response::Metadata { kind, .. } if kind == "directory") {
            directories.insert(entry.path.clone(), Vec::new());
        }
    }
    for entry in entries.iter().rev() {
        let attrs = Attributes::from_response(&entry.metadata)?;
        let directory = if attrs.kind == 0 {
            let children = directories
                .remove(&entry.path)
                .context("missing directory")?;
            if children.is_empty() {
                None
            } else {
                let mut children = children;
                children.sort_by(|a, b| name_order(&a.name, &b.name));
                Some(tree(&mut writer, attrs.inode, 0, 0, children)?)
            }
        } else {
            None
        };
        let name = entry.path.rsplit(|b| *b == b'/').next().unwrap().to_vec();
        let record = Record {
            name,
            attrs,
            directory,
            content: entry.content.as_ref().map(|v| {
                if contents[v].size == 0 {
                    Content::Empty
                } else if let Some(chunked) = locations.get(v) {
                    chunked.clone()
                } else {
                    Content::PackedFrame(placement.small(&entry.path, v).clone())
                }
            }),
            digest: entry
                .content
                .as_deref()
                .map(frames::parse_digest)
                .transpose()?,
        };
        record.validate(entry.path.is_empty())?;
        if entry.path.is_empty() {
            let mut output = BTreeMap::new();
            output.insert("root.bin".into(), frames::encode(&record)?);
            output.insert("index.bin".into(), writer.finish()?);
            return Ok(output);
        }
        let parent = entry
            .path
            .iter()
            .rposition(|b| *b == b'/')
            .map_or(&[][..], |i| &entry.path[..i]);
        directories
            .get_mut(parent)
            .context("missing source parent")?
            .push(record);
    }
    bail!("missing root")
}

#[cfg(test)]
pub(super) mod malformed_tests {
    use super::*;
    #[test]
    fn small_directory_frames_share_the_byte_budget_without_early_count_eviction() {
        let mut cache = NodeCache::new();
        for offset in 0..300 {
            let mut node = Node {
                body: Body::Leaf {
                    directory: offset + 1,
                    depth: 0,
                    prefix: 0,
                    entries: Vec::new(),
                },
                reference: FrameRef {
                    offset,
                    encoded: 1,
                    decoded: 1,
                    digest: [0; 32],
                    count: 1,
                },
                retained: 0,
            };
            node.retained = node.allocation_bytes();
            cache.put(offset, Arc::new(node));
        }
        assert_eq!(cache.entries.len(), 300);
        assert!(cache.bytes < 16 * 1024 * 1024);
        assert_eq!(
            cache.bytes,
            cache
                .entries
                .iter()
                .map(|(_, n)| n.retained_bytes())
                .sum::<usize>()
        );
        while cache.pop_lru().is_some() {}
        assert_eq!(cache.bytes, 0);
    }
    fn attrs(kind: u8, inode: u64) -> Attributes {
        Attributes {
            kind,
            size: 0,
            mode: if kind == 0 { 0o40755 } else { 0o100644 },
            uid: 0,
            gid: 0,
            inode,
            nlink: 1,
            mtime: 0,
            mtime_nsec: 0,
            target: None,
        }
    }
    fn load(writer: frames::Writer, root: Record) -> (tempfile::TempDir, LoadedImage) {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::filesystem(temp.path().into(), false).unwrap();
        let handle = Handle {
            version: Version::LegacyV2,
            image_key: "a".repeat(64),
            platform: "linux-amd64".into(),
            revision: "b".repeat(64),
        };
        let bytes = writer.finish().unwrap();
        storage
            .put(
                &format!("{}/index.bin", handle.prefix()),
                bytes.clone(),
                true,
            )
            .unwrap();
        let commit = Commit {
            format: None,
            format_version: 2,
            image_key: handle.image_key.clone(),
            platform: handle.platform.clone(),
            manifest_digest: format!("sha256:{}", "c".repeat(64)),
            packing: None,
            metadata: BTreeMap::from([(
                "index.bin".into(),
                Descriptor {
                    sha256: hash(&bytes),
                    bytes: bytes.len() as u64,
                },
            )]),
        };
        let config = Configuration {
            format_version: 2,
            architecture: "amd64".into(),
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
            totals: ImageTotals::default(),
        };
        let image = LoadedImage::new(
            storage,
            None,
            handle,
            commit,
            config,
            &frames::encode(&root).unwrap(),
        )
        .unwrap();
        (temp, image)
    }

    #[rstest::rstest]
    #[case::reachable_inventory(false)]
    #[case::unreachable_legacy_object(true)]
    fn prerelease_v2_chunk_remains_readable_without_a_v2_writer(#[case] unreferenced: bool) {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::filesystem(temp.path().into(), false).unwrap();
        let content = vec![17; 65537];
        let digest = frames::digest(&content);
        let len = content.len() as u32;
        let mut blob = frames::Writer::default();
        blob.push(
            &super::super::content::Data::Slots {
                offsets: vec![0, len],
                bytes: content,
            },
            1,
        )
        .unwrap();
        let blob = blob.finish().unwrap();
        let object_key = data_key(&frames::digest_string(&digest)).unwrap();
        storage.put(&object_key, blob.clone(), true).unwrap();

        let mut index = frames::Writer::default();
        let chunk_tree = index
            .push(
                &Body::Chunks {
                    first: 0,
                    chunks: vec![(digest, len)],
                },
                1,
            )
            .unwrap();
        let child = Record {
            name: b"file".into(),
            attrs: Attributes {
                size: u64::from(len),
                ..attrs(1, 2)
            },
            directory: None,
            content: Some(Content::Chunks(chunk_tree)),
            digest: Some(digest),
        };
        let directory = tree(&mut index, 1, 0, 0, vec![child]).unwrap();
        let root = Record {
            name: vec![],
            attrs: attrs(0, 1),
            directory: Some(directory),
            content: None,
            digest: None,
        };
        let canonical = "docker.io/library/legacy:test";
        let manifest_digest = format!("sha256:{}", "c".repeat(64));
        let mut inventory = vec![(object_key, blob.len() as u64)];
        if unreferenced {
            let extra_key = data_key(&hash(b"not reachable")).unwrap();
            storage
                .put(&extra_key, b"not reachable".to_vec(), true)
                .unwrap();
            inventory.push((extra_key, 13));
            inventory.sort_unstable();
        }
        let metadata = [
            (
                "manifest.json",
                serde_json::to_vec(&Manifest {
                    format_version: 2,
                    reference: canonical.into(),
                    platform: "linux-amd64".into(),
                    manifest_digest: manifest_digest.clone(),
                    config_digest: None,
                    layer_digests: None,
                })
                .unwrap(),
            ),
            (
                "config.json",
                serde_json::to_vec(&Configuration {
                    format_version: 2,
                    architecture: "amd64".into(),
                    env: BTreeMap::new(),
                    entrypoint: vec![],
                    cmd: vec![],
                    totals: ImageTotals::default(),
                })
                .unwrap(),
            ),
            ("root.bin", frames::encode(&root).unwrap()),
            ("index.bin", index.finish().unwrap()),
            ("objects.bin", frames::encode(&inventory).unwrap()),
        ];
        let commit = Commit {
            format: None,
            format_version: 2,
            packing: None,
            image_key: image_key(canonical),
            platform: "linux-amd64".into(),
            manifest_digest,
            metadata: metadata
                .iter()
                .map(|(key, bytes)| {
                    (
                        (*key).into(),
                        Descriptor {
                            sha256: hash(bytes),
                            bytes: bytes.len() as u64,
                        },
                    )
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&commit).unwrap();
        let handle = Handle {
            version: Version::LegacyV2,
            image_key: commit.image_key,
            platform: commit.platform,
            revision: hash(&bytes)[7..].into(),
        };
        storage
            .put("format.json", format::LEGACY.to_vec(), true)
            .unwrap();
        storage
            .put(&format!("{}/COMMIT.json", handle.prefix()), bytes, true)
            .unwrap();
        for (name, bytes) in metadata {
            storage
                .put(&format!("{}/{name}", handle.prefix()), bytes, true)
                .unwrap();
        }
        assert!(Format::for_write(&storage).is_err());
        let cache = PortableCache::new(storage, None, true);
        assert_eq!(
            cache
                .request(Request::Read {
                    digest: handle.encode(),
                    path: b"file".into(),
                    offset: u64::from(len) - 2,
                    length: 3,
                })
                .unwrap()
                .1,
            [17, 17]
        );
        let output = tempfile::tempdir().unwrap();
        let export =
            super::super::export_pool(temp.path(), &handle.encode(), &output.path().join("bundle"));
        if unreferenced {
            assert!(
                export
                    .unwrap_err()
                    .to_string()
                    .contains("export inventory disagrees")
            );
        } else {
            export.unwrap();
        }
    }

    #[test]
    fn root_requires_the_reserved_inode() {
        let root = Record {
            name: vec![],
            attrs: attrs(0, 2),
            directory: None,
            content: None,
            digest: None,
        };
        assert!(root.validate(true).is_err());
    }

    pub(in crate::image::cache::portable) fn cached_foreign_directory_reference_is_rejected() {
        let mut writer = frames::Writer::default();
        let child = Record {
            name: b"x".to_vec(),
            attrs: attrs(1, 4),
            directory: None,
            content: Some(Content::Empty),
            digest: Some(frames::digest(&[])),
        };
        let reference = tree(&mut writer, 2, 0, 0, vec![child]).unwrap();
        let a = Record {
            name: b"a".to_vec(),
            attrs: attrs(0, 3),
            directory: Some(reference.clone()),
            content: None,
            digest: None,
        };
        let b = Record {
            name: b"b".to_vec(),
            attrs: attrs(0, 2),
            directory: Some(reference),
            content: None,
            digest: None,
        };
        let root_ref = tree(&mut writer, 1, 0, 0, vec![a, b]).unwrap();
        let root = Record {
            name: vec![],
            attrs: attrs(0, 1),
            directory: Some(root_ref),
            content: None,
            digest: None,
        };
        let (_temp, image) = load(writer, root);
        image.entry(b"b/x").unwrap(); // cache the referenced child before the attack
        for _ in 0..2 {
            assert!(
                image
                    .entry(b"a/x")
                    .unwrap_err()
                    .to_string()
                    .contains("directory index relationship")
            );
            assert!(
                image
                    .list(b"a", 0)
                    .unwrap_err()
                    .to_string()
                    .contains("directory index relationship")
            );
        }
    }
    #[test]
    fn non_utf8_names_roundtrip_without_host_filesystem_support() {
        let name = b"entry-\xff";
        let mut writer = frames::Writer::default();
        let entry = Record {
            name: name.to_vec(),
            attrs: attrs(1, 2),
            directory: None,
            content: Some(Content::Empty),
            digest: Some(frames::digest(&[])),
        };
        let reference = tree(&mut writer, 1, 0, 0, vec![entry]).unwrap();
        let root = Record {
            name: vec![],
            attrs: attrs(0, 1),
            directory: Some(reference),
            content: None,
            digest: None,
        };
        let (_temp, image) = load(writer, root);
        assert_eq!(image.entry(name).unwrap().path, name);
        assert!(
            matches!(image.list(b"", 0).unwrap(), Response::Entries { names, .. } if names == [name.to_vec()])
        );
    }
    #[test]
    fn authenticated_but_invalid_content_references_are_rejected() {
        let entry = Record {
            name: b"a".to_vec(),
            attrs: attrs(1, 2),
            directory: None,
            content: None,
            digest: None,
        };
        assert!(
            entry
                .validate(false)
                .unwrap_err()
                .to_string()
                .contains("content ID")
        );
    }
    #[test]
    fn authenticated_child_cycles_are_rejected_before_traversal() {
        let mut writer = frames::Writer::default();
        let dummy = Body::Leaf {
            directory: 1,
            depth: 0,
            prefix: 0,
            entries: Vec::new(),
        };
        let mut reference = writer.push(&dummy, 1).unwrap();
        reference.offset = 0;
        let body = Body::Branch {
            directory: 1,
            depth: 0,
            prefix: 0,
            children: Box::new(std::array::from_fn(|i| (i == 0).then(|| reference.clone()))),
        };
        // An authenticated self reference cannot be made strictly earlier than itself.
        let frame = FrameRef {
            offset: 0,
            ..reference
        };
        let root = Record {
            name: vec![],
            attrs: attrs(0, 1),
            directory: None,
            content: None,
            digest: None,
        };
        let (_temp, image) = load(frames::Writer::default(), root);
        assert!(image.validate_body(&frame, &body).is_err());
    }
    #[test]
    fn frame_codec_rejects_trailing_bytes_and_oversized_allocations() {
        let mut bytes = frames::encode(&vec![1u8, 2, 3]).unwrap();
        bytes.push(0);
        assert!(frames::decode::<Vec<u8>>(&bytes).is_err());
        let bytes = frames::encode(&u64::MAX).unwrap();
        assert!(frames::decode::<Vec<u8>>(&bytes).is_err());
    }
}

#[cfg(test)]
mod format_vectors {
    use super::*;
    #[test]
    fn routing_matches_reference_xxh3_seeded_vectors() {
        // Cross-checked against libxxhash XXH3_64bits_withSeed, not this crate.
        assert_eq!(route_hash(b""), 0xcdc5_f08f_372c_546d);
        assert_eq!(route_hash(b"config.py"), 0x3b4d_79b0_5e89_fb2a);
        assert_eq!(route_hash(b"entry-\xff"), 0x3650_710c_30da_2f24);
    }
    #[test]
    fn inline_empty_file_has_a_stable_little_endian_varint_encoding() {
        let record = Record {
            name: b"f".to_vec(),
            attrs: Attributes {
                kind: 1,
                size: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                inode: 2,
                nlink: 1,
                mtime: 0,
                mtime_nsec: 0,
                target: None,
            },
            directory: None,
            content: Some(Content::Empty),
            digest: Some(frames::digest(&[])),
        };
        let bytes = frames::encode(&record).unwrap();
        assert_eq!(
            &bytes[..18],
            &[1, 102, 1, 0, 251, 164, 129, 0, 0, 2, 1, 0, 0, 0, 0, 1, 0, 1]
        );
        assert_eq!(
            crate::util::encode_hex(&bytes[18..]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        frames::decode::<Record>(&bytes)
            .unwrap()
            .validate(false)
            .unwrap();
    }
}
