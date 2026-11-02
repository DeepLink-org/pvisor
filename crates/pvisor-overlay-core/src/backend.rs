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
    /// Deliver at most `size` verified bytes to this call's borrowed sink.
    /// EOF is a successful short result. On error a verified prefix may already
    /// be present; callers must not publish it as a successful reply. The sink
    /// and its buffers must never be retained after this synchronous call.
    /// Default behavior preserves existing backends' owned-buffer reads.
    fn read_into(
        &self,
        relative: &Path,
        offset: u64,
        size: u32,
        sink: &mut dyn io::Write,
    ) -> io::Result<usize> {
        let bytes = self.read_at(relative, offset, size)?;
        if bytes.len() > size as usize {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        sink.write_all(&bytes)?;
        Ok(bytes.len())
    }
    /// Whether this producer can populate an exclusively owned, empty private
    /// destination directly from verified immutable content. Default is false.
    fn supports_direct_copy(&self) -> bool {
        false
    }
    /// Populate the destination without changing native lower placeholders.
    /// Only used with a stable namespace registration and positive capability.
    /// Must return the complete logical length; error may leave private partial
    /// bytes, which the adapter must remove rather than publish. Never retain FD.
    fn copy_file_direct(&self, _relative: &Path, _destination: &fs::File) -> io::Result<u64> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    /// Fill native bytes before code opens a backing file directly.
    fn materialize_file(&self, relative: &Path) -> io::Result<()>;
    /// An owned tree copy must include unvisited image objects too.
    fn materialize_tree(&self, relative: &Path) -> io::Result<()>;
}

struct Attachment {
    root: PathBuf,
    backend: Arc<dyn ReadOnlyBackend>,
    stable_namespace: bool,
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
        Self::register(root, backend, false)
    }

    /// Register a fixed logical namespace, independently of native projection mutability.
    ///
    /// The trusted producer must pin one authenticated revision and keep names,
    /// types, logical attributes, permissions, aliases and content stable until
    /// all attachment leases and consumers are gone. Native projection may add
    /// placeholders or fill contents, but must not replace published object
    /// identities or expose names absent from that revision. The caller retains
    /// the private projection directory and prevents external writers.
    ///
    /// Canonicalizes `root` and requires a directory, as `new` does. Rejects any
    /// ancestor/descendant attachment overlap with EEXIST, including future
    /// registrations while a lease exists. Failure installs nothing. This is
    /// not a physical `LayerMutability::Immutable` promise or authorization.
    pub fn new_stable_namespace(
        root: &Path,
        backend: Arc<dyn ReadOnlyBackend>,
    ) -> io::Result<Self> {
        Self::register(root, backend, true)
    }

    fn register(
        root: &Path,
        backend: Arc<dyn ReadOnlyBackend>,
        stable_namespace: bool,
    ) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !fs::symlink_metadata(&root)?.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let mut entries = attachments()
            .write()
            .map_err(|_| io::Error::other("backend registry poisoned"))?;
        entries.retain(|entry| entry.strong_count() != 0);
        if entries.iter().filter_map(Weak::upgrade).any(|entry| {
            entry.root == root
                || ((stable_namespace || entry.stable_namespace)
                    && (root.starts_with(&entry.root) || entry.root.starts_with(&root)))
        }) {
            return Err(io::Error::from_raw_os_error(libc::EEXIST));
        }
        let attachment = Arc::new(Attachment {
            root,
            backend,
            stable_namespace,
        });
        ACTIVE.fetch_add(1, Ordering::Release);
        entries.push(Arc::downgrade(&attachment));
        Ok(Self(attachment))
    }

    pub fn root(&self) -> &Path {
        &self.0.root
    }
}

/// Pins a trusted stable namespace registration, not the caller's directory owner.
///
/// Dropping the original attachment does not revoke this lease or permit a
/// replacement registration. The producer must still retain projection storage;
/// the lease never turns native placeholders into directly readable content.
/// Leases are process-local and cannot be serialized into a VM snapshot.
pub struct StableNamespaceLease {
    attachment: Arc<Attachment>,
}

/// Content identity within one pinned stable namespace, not a native inode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableContentIdentity {
    /// Nonzero backend object identifier; aliases of one object must share it.
    pub object: u64,
    /// Immutable logical byte length of the regular file.
    pub size: u64,
}

impl StableNamespaceLease {
    /// Resolve a regular file's immutable content identity within this revision.
    /// Validates a relative no-parent path, propagates backend/traversal errors,
    /// and returns None for nonregular files. Object IDs are meaningful only
    /// with this lease; no native placeholder bytes or authorization are granted.
    pub fn content_identity(&self, relative: &Path) -> io::Result<Option<StableContentIdentity>> {
        crate::OverlayCore::validate_rel(relative)?;
        let attr = self.attachment.backend.attributes(relative)?;
        if attr.kind != FileType::RegularFile {
            return Ok(None);
        }
        if attr.ino == 0 || attr.size > i64::MAX as u64 {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(Some(StableContentIdentity {
            object: attr.ino,
            size: attr.size,
        }))
    }
}

/// Obtain a lease only for an exact, explicitly stable attachment root.
///
/// Canonicalizes an existing root, propagating filesystem/registry errors.
/// Returns None for ordinary native directories, ordinary attachments and
/// subdirectories of stable attachments. Does not call backend I/O or infer
/// stability from read-only permissions. Future overlapping attachments are
/// refused until every attachment/lease is dropped.
pub fn stable_namespace(root: &Path) -> io::Result<Option<StableNamespaceLease>> {
    let root = root.canonicalize()?;
    let entries = attachments()
        .read()
        .map_err(|_| io::Error::other("backend registry poisoned"))?;
    Ok(entries.iter().filter_map(Weak::upgrade).find_map(|entry| {
        (entry.root == root && entry.stable_namespace)
            .then_some(StableNamespaceLease { attachment: entry })
    }))
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

/// Query whether an active metadata projection overlaps a canonical root.
///
/// `root` must be an absolute, lexically normalized, already canonicalized path;
/// relative/parent components are `InvalidInput`. This reads only the attachment
/// registry and never calls a backend or queries file metadata, including for
/// missing paths. Either ancestor or descendant overlap counts. A poisoned
/// registry returns an error. The result is instantaneous: callers requiring
/// stable absence must independently forbid new overlapping attachments for
/// their entire cache/owner lifetime. This function reserves no namespace.
pub fn attachment_overlaps(root: &Path) -> io::Result<bool> {
    if !root.is_absolute()
        || root.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend overlap query requires a canonical absolute root",
        ));
    }
    if ACTIVE.load(Ordering::Acquire) == 0 {
        return Ok(false);
    }
    let entries = attachments()
        .read()
        .map_err(|_| io::Error::other("backend registry poisoned"))?;
    Ok(entries
        .iter()
        .filter_map(Weak::upgrade)
        .any(|entry| root.starts_with(&entry.root) || entry.root.starts_with(root)))
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

/// Synchronously deliver verified backend bytes into a caller-owned sink.
/// None means no backend is attached and leaves the sink untouched. On error
/// the sink may contain a verified prefix; no backend may retain its borrow.
pub fn read_into(
    path: &Path,
    offset: u64,
    size: u32,
    sink: &mut dyn io::Write,
) -> io::Result<Option<usize>> {
    find(path)?
        .map(|(entry, relative)| entry.backend.read_into(&relative, offset, size, sink))
        .transpose()
}

/// Retained direct-copy capability for one fixed backend object path.
/// This is not an authorization token and cannot outlive its producer storage.
pub struct DirectFileSource {
    attachment: Arc<Attachment>,
    relative: PathBuf,
    size: u64,
}

impl DirectFileSource {
    /// Fill a new, empty, private destination; failures may leave partial bytes.
    /// The caller owns preimage ordering, validation, cleanup and publication.
    pub fn copy_into(&self, destination: &fs::File) -> io::Result<u64> {
        let copied = self
            .attachment
            .backend
            .copy_file_direct(&self.relative, destination)?;
        if copied != self.size || destination.metadata()?.len() != self.size {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(copied)
    }
}

/// Return a retained capability only for an explicitly stable producer which
/// supports direct copying. Native, legacy and ordinary attachments return None.
/// Does not fill placeholder contents or bypass path/policy checks.
pub fn direct_file_source(path: &Path) -> io::Result<Option<DirectFileSource>> {
    let Some((attachment, relative)) = find(path)? else {
        return Ok(None);
    };
    if !attachment.stable_namespace || !attachment.backend.supports_direct_copy() {
        return Ok(None);
    }
    let attr = attachment.backend.attributes(&relative)?;
    if attr.kind != FileType::RegularFile {
        return Ok(None);
    }
    Ok(Some(DirectFileSource {
        attachment,
        relative,
        size: attr.size,
    }))
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
