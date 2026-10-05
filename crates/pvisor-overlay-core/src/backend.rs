//! Transport-independent read-only backends and scoped namespace attachments.
//!
//! Attachments are private metadata projections, never mounts. Native backing
//! descriptors remain owned by the adapters; remote bytes are read through the
//! backend and materialized only for copy-up or an owned snapshot export.
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock, RwLock, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Directory,
    RegularFile,
    Symlink,
    NamedPipe,
    Socket,
    BlockDevice,
    CharDevice,
}

/// Unix object metadata, independent of fuser replies and virtqueue buffers.
#[derive(Clone, Debug)]
pub struct FileAttr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub crtime: SystemTime,
    pub kind: FileType,
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub rdev: u32,
    pub blksize: u32,
    pub flags: u32,
}

/// The existing containers override contract survives owned tree copies.
#[derive(Clone, Copy, Debug)]
pub struct UnixIdentity {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}
impl UnixIdentity {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::from_raw_os_error(libc::EIO);
        let mut fields = std::str::from_utf8(bytes)
            .map_err(|_| invalid())?
            .split(':');
        let uid = fields
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        let gid = fields
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        let mode = fields
            .next()
            .and_then(|s| u32::from_str_radix(s, 8).ok())
            .ok_or_else(invalid)?;
        if fields.next().is_some() {
            return Err(invalid());
        }
        Ok(Self { uid, gid, mode })
    }
}

pub trait ReadOnlyBackend: Send + Sync {
    /// Populate only native metadata for a checked relative path.
    fn prepare_metadata(&self, relative: &Path) -> io::Result<()>;
    fn prepare_directory(&self, relative: &Path) -> io::Result<()>;
    fn attributes(&self, relative: &Path) -> io::Result<FileAttr>;
    fn read_at(&self, relative: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>>;
    /// Fill native bytes before code opens a backing file directly.
    fn materialize_file(&self, relative: &Path) -> io::Result<()>;
    /// An owned tree copy must include unvisited image objects too.
    fn materialize_tree(&self, relative: &Path) -> io::Result<()>;
}

struct Attachment {
    root: PathBuf,
    backend: Arc<dyn ReadOnlyBackend>,
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
impl Drop for Attachment {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Release);
    }
}

fn attachments() -> &'static RwLock<Vec<Weak<Attachment>>> {
    static ATTACHMENTS: OnceLock<RwLock<Vec<Weak<Attachment>>>> = OnceLock::new();
    ATTACHMENTS.get_or_init(RwLock::default)
}

/// Hold through execution and teardown. Dropping the owner retires the entry;
/// no global lock is held while a backend performs metadata or content I/O.
pub struct BackendAttachment(Arc<Attachment>);
impl BackendAttachment {
    pub fn new(root: &Path, backend: Arc<dyn ReadOnlyBackend>) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !fs::symlink_metadata(&root)?.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let mut entries = attachments()
            .write()
            .map_err(|_| io::Error::other("backend registry poisoned"))?;
        entries.retain(|entry| entry.strong_count() != 0);
        if entries
            .iter()
            .filter_map(Weak::upgrade)
            .any(|entry| entry.root == root)
        {
            return Err(io::Error::from_raw_os_error(libc::EEXIST));
        }
        let attachment = Arc::new(Attachment { root, backend });
        ACTIVE.fetch_add(1, Ordering::Release);
        entries.push(Arc::downgrade(&attachment));
        Ok(Self(attachment))
    }

    pub fn root(&self) -> &Path {
        &self.0.root
    }
}

fn find(path: &Path) -> io::Result<Option<(Arc<Attachment>, PathBuf)>> {
    if ACTIVE.load(Ordering::Acquire) == 0 {
        return Ok(None);
    }
    let entries = attachments()
        .read()
        .map_err(|_| io::Error::other("backend registry poisoned"))?;
    let selected = entries
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|entry| path.starts_with(&entry.root))
        .max_by_key(|entry| entry.root.components().count());
    drop(entries);
    selected
        .map(|entry| {
            let relative = path.strip_prefix(&entry.root).unwrap().to_path_buf();
            crate::OverlayCore::validate_rel(&relative)?;
            Ok((entry, relative))
        })
        .transpose()
}

pub fn prepare_metadata(path: &Path) -> io::Result<()> {
    if let Some((entry, relative)) = find(path)? {
        entry.backend.prepare_metadata(&relative)?;
    }
    Ok(())
}

pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<fs::Metadata> {
    let path = path.as_ref();
    prepare_metadata(path)?;
    fs::symlink_metadata(path)
}

pub fn read_dir(path: impl AsRef<Path>) -> io::Result<fs::ReadDir> {
    let path = path.as_ref();
    if let Some((entry, relative)) = find(path)? {
        entry.backend.prepare_directory(&relative)?;
    }
    fs::read_dir(path)
}

pub fn read_link(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = path.as_ref();
    prepare_metadata(path)?;
    fs::read_link(path)
}

pub fn attributes(path: &Path) -> io::Result<Option<FileAttr>> {
    find(path)?
        .map(|(entry, relative)| entry.backend.attributes(&relative))
        .transpose()
}

pub fn read_at(path: &Path, offset: u64, size: u32) -> io::Result<Option<Vec<u8>>> {
    find(path)?
        .map(|(entry, relative)| entry.backend.read_at(&relative, offset, size))
        .transpose()
}

pub fn materialize_file(path: &Path) -> io::Result<()> {
    if let Some((entry, relative)) = find(path)? {
        entry.backend.materialize_file(&relative)?;
    }
    Ok(())
}

pub fn materialize_tree(path: &Path) -> io::Result<()> {
    if let Some((entry, relative)) = find(path)? {
        entry.backend.materialize_tree(&relative)?;
    }
    Ok(())
}

pub fn link_count(path: &Path, native: u64) -> io::Result<u64> {
    Ok(attributes(path)?.map_or(native, |attr| attr.nlink as u64))
}

impl FileType {
    #[allow(clippy::unnecessary_cast)]
    pub fn mode(self) -> u32 {
        match self {
            Self::Directory => libc::S_IFDIR as u32,
            Self::RegularFile => libc::S_IFREG as u32,
            Self::Symlink => libc::S_IFLNK as u32,
            Self::NamedPipe => libc::S_IFIFO as u32,
            Self::Socket => libc::S_IFSOCK as u32,
            Self::BlockDevice => libc::S_IFBLK as u32,
            Self::CharDevice => libc::S_IFCHR as u32,
        }
    }
}
