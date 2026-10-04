use crate::sys;
use pvisor_core::overlay::{PathFingerprint, PathPreimage, XattrFingerprint};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

pub const WHITEOUT_PREFIX: &str = ".wh.";
pub const OPAQUE_NAME: &str = ".wh..wh..opq";
pub const ROOT_METADATA_NAME: &str = ".wh..pvisor-root-metadata";
const TEMP_PREFIX: &str = ".wh..pvisor-copyup-";
const PREIMAGE_COMPLETE_MARKER: &str = "complete-v1";
pub(crate) const OPAQUE_XATTRS: [&str; 3] = [
    "trusted.overlay.opaque",
    "user.overlay.opaque",
    "user.fuseoverlayfs.opaque",
];

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct Resolved {
    pub path: PathBuf,
    pub is_upper: bool,
}

/// One checked namespace resolution and its already-read backing metadata.
/// Layer 0 is the writable upper; layers 1.. follow `OverlayLayout::lowers`.
/// This is a request-local observation, not a cache or an immutable capability.
#[derive(Debug)]
pub struct ResolvedMetadata {
    pub resolved: Resolved,
    pub metadata: Metadata,
    pub layer: usize,
}

/// Physical identity observed while checking a backing parent directory.
/// Request-local evidence, not a permission capability or cached attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackingIdentity {
    pub device: u64,
    pub inode: u64,
    /// Linux mount identity distinguishes bind/idmapped mount contexts.
    /// Absent on systems without an observable mount ID.
    pub mount_id: Option<u64>,
}

/// Checked metadata plus identities of the selected layer's physical parents,
/// in component order, excluding the layer root and the final object.
#[derive(Debug)]
pub struct BackingResolution {
    pub entry: ResolvedMetadata,
    pub parents: Vec<BackingIdentity>,
}

#[derive(Debug)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub backing: ResolvedMetadata,
}

/// Validated lower ordering and its explicit apply baseline.
#[derive(Debug)]
pub struct OverlayLayout {
    lowers: Vec<PathBuf>,
    target: PathBuf,
    baseline: PathBuf,
    frozen_baseline: bool,
}
impl OverlayLayout {
    pub fn new(lowers: Vec<PathBuf>, target: PathBuf) -> io::Result<Self> {
        Self::with_baseline(lowers, target, None)
    }
    pub fn with_baseline(
        lowers: Vec<PathBuf>,
        target: PathBuf,
        snapshot: Option<&Path>,
    ) -> io::Result<Self> {
        let last = lowers.last().ok_or_else(|| error(libc::EINVAL))?;
        let baseline = fs::canonicalize(last)?;
        if baseline != fs::canonicalize(snapshot.unwrap_or(&target))? {
            return Err(error(libc::EINVAL));
        }
        if !target.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        for lower in &lowers {
            if !lower.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
        }
        let frozen_baseline = baseline != fs::canonicalize(&target)?;
        let baseline = last.clone();
        Ok(Self {
            lowers,
            target,
            baseline,
            frozen_baseline,
        })
    }
    pub fn lowers(&self) -> &[PathBuf] {
        &self.lowers
    }
    pub fn target(&self) -> &Path {
        &self.target
    }
    /// Target-corresponding baseline, never the higher-precedence extra layer.
    pub fn baseline(&self) -> &Path {
        &self.baseline
    }
}

#[derive(Debug)]
pub struct OverlayCore {
    profile: crate::profile::Profile,
    layout: OverlayLayout,
    upper: PathBuf,
    work: Option<PathBuf>,
    excluded: BTreeSet<PathBuf>,
    access: crate::FileAccessPolicy,
    // Keep every upper alias so unlink/replacement can retire a path without
    // losing the copied inode while another alias still carries its changes.
    copied_hard_links: Mutex<HashMap<(u64, u64), Vec<PathBuf>>>,
    hard_link_sources: Mutex<HashMap<(u64, u64), PathBuf>>,
    preimage_dir: Option<PathBuf>,
    // Read observations are published without fsync. Before the first upper
    // mutation, their file and directory are synced under this lock.
    preimage_lock: Mutex<BTreeSet<PathBuf>>,
}

fn error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A candidate layer must not follow symlinks in any relative ancestor,
/// even when a different layer supplied the merged directory prefix.
pub(crate) fn layer_path(root: &Path, rel: &Path) -> io::Result<Option<PathBuf>> {
    Ok(layer_metadata(root, rel)?.map(|(path, _)| path))
}

fn layer_metadata(root: &Path, rel: &Path) -> io::Result<Option<(PathBuf, Metadata)>> {
    layer_metadata_with_parents(root, rel, None, &crate::profile::Profile::default(), None)
}

fn layer_metadata_with_parents(
    root: &Path,
    rel: &Path,
    mut parents: Option<&mut Vec<BackingIdentity>>,
    profile: &crate::profile::Profile,
    mut checked_directories: Option<&mut HashSet<PathBuf>>,
) -> io::Result<Option<(PathBuf, Metadata)>> {
    OverlayCore::validate_rel(rel)?;
    if let Some(parents) = &mut parents {
        parents.clear();
    }
    let mut path = root.to_path_buf();
    if let Some(parent) = rel.parent().filter(|parent| {
        !checked_directories
            .as_ref()
            .is_some_and(|checked| checked.contains(&root.join(parent)))
    }) {
        for component in parent.components() {
            path.push(component.as_os_str());
            profile.add("layer_parent_stats", 1);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    if let Some(checked) = &mut checked_directories {
                        checked.insert(path.clone());
                    }
                    if let Some(parents) = &mut parents {
                        if cfg!(target_os = "linux") {
                            profile.add("mount_identity_attempts", 1);
                        }
                        parents.push(BackingIdentity {
                            device: metadata.dev(),
                            inode: metadata.ino(),
                            mount_id: sys::metadata_mount_id(&path, &metadata),
                        });
                    }
                }
                Ok(_) => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }
    let path = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    profile.add("layer_leaf_stats", 1);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.is_dir()
                && let Some(checked) = &mut checked_directories
            {
                checked.insert(path.clone());
            }
            Ok(Some((path, metadata)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The same opaque interpretation is used for the merged view and apply plan.
pub fn is_opaque_directory(path: &Path) -> bool {
    exists(&path.join(OPAQUE_NAME))
        || OPAQUE_XATTRS
            .iter()
            .any(|name| sys::get_xattr(path, OsStr::new(name)).is_ok_and(|value| value == b"y"))
}

pub fn validate_guest_xattr(name: &OsStr) -> io::Result<()> {
    if OPAQUE_XATTRS
        .iter()
        .any(|reserved| name == OsStr::new(reserved))
    {
        return Err(error(libc::EPERM));
    }
    Ok(())
}

fn ignorable_metadata_error(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
    )
}

fn ignorable_ownership_error(err: &io::Error) -> bool {
    // A uid or gid outside the current user namespace is reported as EINVAL.
    // Ownership is best-effort for an unprivileged overlay, just like EPERM.
    ignorable_metadata_error(err) || err.raw_os_error() == Some(libc::EINVAL)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

/// Fingerprint one path without following its final symlink.
pub fn fingerprint_at(root: &Path, rel: &Path) -> io::Result<PathFingerprint> {
    fingerprint_profiled(root, rel, &crate::profile::Profile::default())
}

fn fingerprint_profiled(
    root: &Path,
    rel: &Path,
    profile: &crate::profile::Profile,
) -> io::Result<PathFingerprint> {
    OverlayCore::validate_rel(rel)?;
    let Some(path) = layer_path(root, rel)? else {
        return Ok(PathFingerprint::Absent);
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(PathFingerprint::Absent);
        }
        Err(error) => return Err(error),
    };
    let xattrs = Some(fingerprint_xattrs(&path)?);
    let kind = metadata.file_type();
    if kind.is_file() {
        let mut digest = Sha256::new();
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            profile.add("fingerprint_bytes", read as u64);
            digest.update(&buffer[..read]);
        }
        return Ok(PathFingerprint::File {
            sha256: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            xattrs,
        });
    }
    if kind.is_dir() {
        return Ok(PathFingerprint::Directory {
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: metadata.mtime_nsec(),
            xattrs,
        });
    }
    if kind.is_symlink() {
        return Ok(PathFingerprint::Symlink {
            target: fs::read_link(&path)?.into_os_string().into_vec(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            xattrs,
        });
    }
    Ok(PathFingerprint::Other {
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev(),
        xattrs,
    })
}

fn fingerprint_xattrs(path: &Path) -> io::Result<XattrFingerprint> {
    let names = match sys::list_xattrs(path) {
        Ok(names) => names,
        Err(error) if error.raw_os_error() == Some(libc::ENOTSUP) => {
            return Ok(XattrFingerprint::Unsupported);
        }
        Err(error) => return Err(error),
    };
    let mut entries = Vec::new();
    for name in names {
        let name = OsStr::from_bytes(&name);
        if OPAQUE_XATTRS
            .iter()
            .any(|reserved| name == OsStr::new(reserved))
        {
            continue;
        }
        let value = sys::get_xattr(path, name)?;
        entries.push((name.as_bytes().to_vec(), sha256_hex(&value)));
    }
    entries.sort();
    Ok(XattrFingerprint::Values { entries })
}

/// Load first observations. Entries for mutated paths are synced before mutation.
pub fn load_preimages(directory: &Path) -> io::Result<Vec<PathPreimage>> {
    let entries = directory.join("entries");
    let iterator = match fs::read_dir(&entries) {
        Ok(iterator) => iterator,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut preimages = Vec::new();
    for entry in iterator {
        let entry = entry?;
        if entry.path().extension() != Some(OsStr::new("json")) {
            continue;
        }
        let preimage = serde_json::from_slice::<PathPreimage>(&fs::read(entry.path())?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        OverlayCore::validate_rel(&preimage.relative_path())?;
        preimages.push(preimage);
    }
    preimages.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(preimages)
}

pub fn preimage_journal_is_complete(directory: &Path) -> bool {
    directory.join(PREIMAGE_COMPLETE_MARKER).is_file()
}

/// Consume journal entries after their corresponding target paths commit.
pub fn remove_preimages(directory: &Path, paths: &[PathBuf]) -> io::Result<()> {
    let entries = directory.join("entries");
    for path in paths {
        OverlayCore::validate_rel(path)?;
        let journal_path =
            entries.join(format!("{}.json", sha256_hex(path.as_os_str().as_bytes())));
        match fs::remove_file(journal_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match File::open(entries) {
        Ok(directory) => directory.sync_all(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl OverlayCore {
    pub fn new(lowers: Vec<PathBuf>, upper: PathBuf, work: Option<PathBuf>) -> io::Result<Self> {
        Self::new_with_exclusions(lowers, upper, work, Vec::new())
    }

    pub fn new_with_exclusions(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
    ) -> io::Result<Self> {
        Self::new_with_exclusions_and_preimages(lowers, upper, work, excluded, None)
    }

    pub fn new_with_exclusions_and_preimages(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let target = lowers.last().ok_or_else(|| error(libc::EINVAL))?.clone();
        Self::new_for_target(lowers, target, upper, work, excluded, preimage_dir)
    }

    pub fn new_for_target(
        lowers: Vec<PathBuf>,
        target: PathBuf,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let layout = OverlayLayout::new(lowers, target)?;
        Self::new_for_layout(layout, upper, work, excluded, preimage_dir)
    }

    pub fn new_for_layout(
        layout: OverlayLayout,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        Self::build_for_layout(layout, upper, work, excluded, preimage_dir, true)
    }

    /// Open a snapshot's existing backing without initialization writes, root
    /// metadata copies or temporary-file recovery. The caller owns its lease.
    pub fn open_existing(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let target = lowers.last().ok_or_else(|| error(libc::EINVAL))?.clone();
        let layout = OverlayLayout::new(lowers, target)?;
        Self::open_existing_for_layout(layout, upper, work, excluded, preimage_dir)
    }

    /// Reopen backing while retaining its explicit target and frozen baseline.
    pub fn open_existing_for_layout(
        layout: OverlayLayout,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        Self::build_for_layout(layout, upper, work, excluded, preimage_dir, false)
    }

    fn build_for_layout(
        layout: OverlayLayout,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
        initialize: bool,
    ) -> io::Result<Self> {
        if initialize {
            fs::create_dir_all(&upper)?;
        }
        let upper_was_empty = fs::read_dir(&upper)?.next().is_none();
        if let Some(work) = &work {
            if initialize {
                fs::create_dir_all(work)?;
            }
            let actual_work = fs::canonicalize(work)?;
            let actual_upper = fs::canonicalize(&upper)?;
            if actual_work.starts_with(&actual_upper) || actual_upper.starts_with(&actual_work) {
                return Err(error(libc::EINVAL));
            }
            if fs::metadata(work)?.dev() != fs::metadata(&upper)?.dev() {
                return Err(error(libc::EXDEV));
            }
        }
        let excluded = excluded
            .into_iter()
            .map(|path| {
                Self::validate_rel(&path)?;
                if path.as_os_str().is_empty() {
                    return Err(error(libc::EINVAL));
                }
                Ok(path)
            })
            .collect::<io::Result<BTreeSet<_>>>()?;
        // Compare physical paths, so symlink aliases cannot bypass backing isolation.
        let backing = std::iter::once(&upper)
            .chain(work.iter())
            .map(fs::canonicalize)
            .collect::<io::Result<Vec<_>>>()?;
        for root in layout.lowers.iter().chain(std::iter::once(&layout.target)) {
            let root = fs::canonicalize(root)?;
            for path in &backing {
                if root.starts_with(path) {
                    return Err(error(libc::EINVAL));
                }
                if let Ok(relative) = path.strip_prefix(&root)
                    && !excluded
                        .iter()
                        .any(|excluded| relative.starts_with(excluded))
                {
                    return Err(error(libc::EINVAL));
                }
            }
        }
        if let Some(work) = &work {
            for entry in fs::read_dir(work)? {
                let entry = entry?;
                if initialize
                    && entry
                        .file_name()
                        .as_bytes()
                        .starts_with(TEMP_PREFIX.as_bytes())
                {
                    let path = entry.path();
                    if fs::symlink_metadata(&path)?.is_dir() {
                        fs::remove_dir_all(path)?;
                    } else {
                        fs::remove_file(path)?;
                    }
                }
            }
        }
        if let Some(directory) = &preimage_dir {
            if initialize {
                fs::create_dir_all(directory.join("entries"))?;
            } else {
                fs::read_dir(directory.join("entries"))?;
            }
            if initialize && upper_was_empty && !preimage_journal_is_complete(directory) {
                let marker = directory.join(PREIMAGE_COMPLETE_MARKER);
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(marker)?;
                file.write_all(b"pvisor-overlay-preimage-journal-v1\n")?;
                file.sync_all()?;
                File::open(directory)?.sync_all()?;
            }
        }
        let core = Self {
            profile: crate::profile::Profile::from_env("overlay-core"),
            layout,
            upper,
            work,
            excluded,
            copied_hard_links: Mutex::new(HashMap::new()),
            hard_link_sources: Mutex::new(HashMap::new()),
            access: crate::FileAccessPolicy::default(),
            preimage_dir,
            preimage_lock: Mutex::new(BTreeSet::new()),
        };
        if initialize
            && fs::read_dir(&core.upper)?.next().is_none()
            && let Some(root) = core.layout.lowers.first()
        {
            let metadata = fs::symlink_metadata(root)?;
            core.copy_metadata(root, &core.upper, &metadata)?;
        }
        Ok(core)
    }

    /// Preserve copy-up hard-link groups across a VM runner replacement.
    /// Paths are relative to the existing upper; no host descriptors are saved.
    pub fn capture_hard_links(&self) -> io::Result<Vec<(u64, u64, Vec<PathBuf>)>> {
        self.copied_hard_links
            .lock()
            .map_err(|_| error(libc::EIO))?
            .iter()
            .map(|(&(dev, ino), paths)| {
                let paths = paths
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&self.upper)
                            .map(Path::to_path_buf)
                            .map_err(|_| error(libc::EINVAL))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                Ok((dev, ino, paths))
            })
            .collect()
    }

    /// Original lower path recorded at copy-up, independent of FUSE inode
    /// cache eviction and later upper renames. Capturing these hints never
    /// scans lower trees. Restore coordinators validate hints against identity.
    pub fn capture_hard_link_sources(&self) -> io::Result<Vec<(u64, u64, PathBuf)>> {
        Ok(self
            .hard_link_sources
            .lock()
            .map_err(|_| error(libc::EIO))?
            .iter()
            .map(|(&(dev, ino), path)| (dev, ino, path.clone()))
            .collect())
    }

    pub fn restore_hard_link_sources(&self, sources: &[(u64, u64, PathBuf)]) -> io::Result<()> {
        let mut restored = HashMap::new();
        for (dev, ino, path) in sources {
            let owned = self.layout.lowers.iter().any(|root| {
                path.strip_prefix(root).ok().is_some_and(|relative| {
                    !relative.as_os_str().is_empty()
                        && Self::validate_rel(relative).is_ok()
                        && layer_path(root, relative).ok().flatten().as_ref() == Some(path)
                })
            });
            let meta = fs::symlink_metadata(path)?;
            if !owned
                || !meta.is_file()
                || meta.nlink() < 2
                || (meta.dev(), meta.ino()) != (*dev, *ino)
                || restored.insert((*dev, *ino), path.clone()).is_some()
            {
                return Err(error(libc::EINVAL));
            }
        }
        *self
            .hard_link_sources
            .lock()
            .map_err(|_| error(libc::EIO))? = restored;
        Ok(())
    }

    pub fn restore_hard_links(&self, saved: &[(u64, u64, Vec<PathBuf>)]) -> io::Result<()> {
        let mut groups = HashMap::new();
        let mut seen = BTreeSet::new();
        for (dev, ino, paths) in saved {
            if *ino == 0 || paths.is_empty() || groups.contains_key(&(*dev, *ino)) {
                return Err(error(libc::EINVAL));
            }
            let mut restored = Vec::new();
            let mut identity = None;
            for relative in paths {
                Self::validate_rel(relative)?;
                if relative.as_os_str().is_empty() || !seen.insert(relative.clone()) {
                    return Err(error(libc::EINVAL));
                }
                let path = layer_path(&self.upper, relative)?.ok_or_else(|| error(libc::ENOENT))?;
                let metadata = fs::symlink_metadata(&path)?;
                let current = (metadata.dev(), metadata.ino());
                if !metadata.is_file() || identity.is_some_and(|expected| expected != current) {
                    return Err(error(libc::EINVAL));
                }
                identity = Some(current);
                restored.push(path);
            }
            groups.insert((*dev, *ino), restored);
        }
        *self
            .copied_hard_links
            .lock()
            .map_err(|_| error(libc::EIO))? = groups;
        Ok(())
    }

    /// Remember the target's first content observation before exposing a lower
    /// file, symlink or xattr to the workload. Positive stat/lookup alone does
    /// not read/hash ordinary file contents. Frozen layouts need no read-time
    /// journal: their immutable target baseline supplies the mutation preimage.
    ///
    /// Read-only observations are atomic files but are not fsynced per read.
    /// Mutation promotes the same entry to durable before changing the upper.
    /// A normal stage copy/reopen preserves observations; this is not a durable
    /// read-set transaction or a live lower snapshot across power loss.
    pub fn observe_read(&self, rel: &Path) -> io::Result<()> {
        let _span = self.profile.span("observe_read");
        // Do not turn a denied alias or an I/O failure into a negative lookup
        // and then read/hash the denied underlying file as its "preimage".
        let resolved = self.resolve_checked(rel)?;
        if self.layout.frozen_baseline {
            return Ok(());
        }
        if resolved.is_none() {
            self.observe_absence(rel)
        } else {
            self.capture_preimage(rel, false)
        }
    }

    fn observe_absence(&self, rel: &Path) -> io::Result<()> {
        if self.layout.frozen_baseline {
            return Ok(());
        }
        // Absence was already observed. Do not re-read a live target that may
        // have appeared between lookup and journal publication.
        self.capture_preimage_after_check(rel, false, true, || Ok(()))
    }

    fn record_preimage(&self, rel: &Path) -> io::Result<()> {
        self.capture_preimage(rel, true)
    }

    fn capture_preimage(&self, rel: &Path, durable: bool) -> io::Result<()> {
        self.capture_preimage_after_check(rel, durable, false, || Ok(()))
    }

    // The hook makes the missing-entry / publication interleaving deterministic
    // in tests without changing the public API or relying on thread timing.
    fn capture_preimage_after_check(
        &self,
        rel: &Path,
        durable: bool,
        observed_absent: bool,
        after_missing: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let _span = self.profile.span("preimage");
        let Some(directory) = &self.preimage_dir else {
            return Ok(());
        };
        Self::validate_rel(rel)?;
        let lock_wait = self.profile.span("journal_lock_wait");
        let mut synced = self
            .preimage_lock
            .lock()
            .map_err(|_| io::Error::other("preimage journal lock poisoned"))?;
        drop(lock_wait);
        let path_bytes = rel.as_os_str().as_bytes();
        let destination = directory
            .join("entries")
            .join(format!("{}.json", sha256_hex(path_bytes)));
        let lookup = self.profile.span("journal_lookup");
        let existing = fs::symlink_metadata(&destination);
        drop(lookup);
        match existing {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(error(libc::EINVAL));
                }
                if durable && !synced.contains(rel) {
                    let file = Self::verified_preimage_file(&destination, rel)?;
                    self.sync_preimage(&file)?;
                    self.sync_preimage(&File::open(directory.join("entries"))?)?;
                    synced.insert(rel.to_path_buf());
                }
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        after_missing()?;
        // An apply may consume this entry while the core remains open. A new
        // mutation then starts a new observation, rather than reusing a cache.
        synced.remove(rel);
        let preimage = PathPreimage {
            path: path_bytes.to_vec(),
            state: if observed_absent {
                PathFingerprint::Absent
            } else {
                let _span = self.profile.span("fingerprint");
                let result = fingerprint_profiled(self.layout.baseline(), rel, &self.profile)?;
                self.profile.add("fingerprinted_paths", 1);
                result
            },
        };
        let serialize = self.profile.span("journal_serialize");
        let body = serde_json::to_vec_pretty(&preimage)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        drop(serialize);
        let temporary = directory.join("entries").join(format!(
            ".pending-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let create = self.profile.span("journal_create");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        drop(create);
        self.profile.add("journal_publications", 1);
        let result = (|| {
            {
                let _span = self.profile.span("journal_write");
                file.write_all(&body)?;
            }
            if durable {
                self.sync_preimage(&file)?;
            }
            // Publish without replacement even if another core instance uses
            // the same stage. A race must never replace its first observation.
            let publish = self.profile.span("journal_publish");
            // Rename reduces the bulk first-read cost on APFS. Keep absence
            // and direct mutation publication on the established link path:
            // paired repair/diff trials regressed when those also used rename.
            let publication = if !durable && !observed_absent {
                self.profile.add("journal_rename_attempts", 1);
                sys::publish_no_replace(&temporary, &destination)
            } else {
                self.profile.add("journal_link_attempts", 1);
                sys::publish_by_link(&temporary, &destination)
            };
            drop(publish);
            match publication {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    // Another core won after our initial check. Adopt its
                    // immutable first observation, and sync the actual winner
                    // before mutation rather than just syncing our loser.
                    let winner = Self::verified_preimage_file(&destination, rel)?;
                    if durable {
                        self.sync_preimage(&winner)?;
                    }
                    let _span = self.profile.span("journal_cleanup");
                    fs::remove_file(&temporary)?;
                }
                Err(error) => return Err(error),
            }
            if durable {
                self.sync_preimage(&File::open(directory.join("entries"))?)?;
                synced.insert(rel.to_path_buf());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn sync_preimage(&self, file: &File) -> io::Result<()> {
        let _span = self.profile.span("journal_fsync");
        file.sync_all()
    }

    fn verified_preimage_file(destination: &Path, rel: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(destination)?;
        if !file.metadata()?.is_file() {
            return Err(error(libc::EINVAL));
        }
        let preimage: PathPreimage = serde_json::from_reader(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if preimage.relative_path() != rel {
            return Err(error(libc::EINVAL));
        }
        Ok(file)
    }

    fn record_logical_tree_mapping(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.record_preimage(destination)?;
        if !self.metadata(source)?.is_dir() {
            return Ok(());
        }
        for name in self.list_names(source)? {
            let source_child = Self::child(source, &name)?;
            let destination_child = Self::child(destination, &name)?;
            self.record_logical_tree_mapping(&source_child, &destination_child)?;
        }
        Ok(())
    }

    pub fn upper(&self) -> &Path {
        &self.upper
    }

    fn is_excluded(&self, rel: &Path) -> bool {
        self.access.denied(rel)
            || self.excluded.iter().any(|prefix| {
                rel.starts_with(prefix)
                    || (cfg!(target_os = "macos")
                        && rel.components().count() >= prefix.components().count()
                        && rel.components().zip(prefix.components()).all(|(a, b)| {
                            a.as_os_str()
                                .as_bytes()
                                .eq_ignore_ascii_case(b.as_os_str().as_bytes())
                        }))
            })
    }

    fn require_visible(&self, rel: &Path) -> io::Result<()> {
        Self::validate_rel(rel)?;
        self.access.check(rel).map_err(|_| error(libc::EACCES))?;
        if self.is_excluded(rel) {
            return Err(error(libc::ENOENT));
        }
        Ok(())
    }

    pub fn with_access_policy(mut self, policy: &crate::FileAccessPolicy) -> Self {
        self.access = policy.clone();
        self
    }

    pub fn with_profile(mut self, profile: crate::profile::Profile) -> Self {
        self.profile = profile;
        self
    }

    pub fn emit_profile_checkpoint(&self) {
        self.profile.emit_checkpoint();
    }

    pub fn profile_report(&self) -> Option<crate::profile::ProfileReport> {
        self.profile.report()
    }

    // ponytail: reject multiply-linked files when denials exist; an inode index would
    // require scanning every lower and tracking external changes to avoid alias bypasses.
    fn require_unaliased(&self, path: &Path) -> io::Result<()> {
        if self.access.has_denials() {
            let metadata = fs::symlink_metadata(path)?;
            self.require_unaliased_metadata(&metadata)?;
        }
        Ok(())
    }

    fn require_unaliased_metadata(&self, metadata: &Metadata) -> io::Result<()> {
        if self.access.has_denials() && metadata.is_file() && metadata.nlink() > 1 {
            return Err(error(libc::EACCES));
        }
        Ok(())
    }

    /// Validate physical descendants, including names hidden by access rules, before
    /// moving/removing a directory. Never follow symlinks into another tree.
    fn require_tree_access(&self, old: &Path, new: &Path) -> io::Result<()> {
        if !self.access.has_denials() {
            return Ok(());
        }
        self.require_visible(old)?;
        self.require_visible(new)?;
        let mut names = BTreeSet::new();
        for root in std::iter::once(&self.upper).chain(&self.layout.lowers) {
            if old.ancestors().skip(1).any(|parent| {
                fs::symlink_metadata(root.join(parent)).is_ok_and(|meta| !meta.is_dir())
            }) {
                continue;
            }
            let path = root.join(old);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    for entry in fs::read_dir(path)? {
                        let name = entry?.file_name();
                        if !Self::is_whiteout_name(&name) {
                            names.insert(name);
                        }
                    }
                }
                Ok(_) => self.require_unaliased(&path)?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        for name in names {
            self.require_tree_access(&old.join(&name), &new.join(&name))?;
        }
        Ok(())
    }

    pub fn validate_rel(rel: &Path) -> io::Result<()> {
        if rel.is_absolute()
            || rel
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    pub fn validate_name(name: &OsStr) -> io::Result<()> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&0)
            || bytes.starts_with(WHITEOUT_PREFIX.as_bytes())
        {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    pub fn child(parent: &Path, name: &OsStr) -> io::Result<PathBuf> {
        Self::validate_rel(parent)?;
        Self::validate_name(name)?;
        Ok(if parent.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            parent.join(name)
        })
    }

    pub fn upper_path(&self, rel: &Path) -> PathBuf {
        if rel.as_os_str().is_empty() {
            self.upper.clone()
        } else {
            self.upper.join(rel)
        }
    }

    fn whiteout_path(&self, parent: &Path, name: &OsStr) -> PathBuf {
        let mut marker = OsString::from(WHITEOUT_PREFIX);
        marker.push(name);
        self.upper_path(parent).join(marker)
    }

    pub fn is_whiteout_name(name: &OsStr) -> bool {
        name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes())
    }

    fn is_whiteouted(&self, parent: &Path, name: &OsStr) -> bool {
        exists(&self.whiteout_path(parent, name))
    }

    pub fn is_opaque(&self, rel: &Path) -> bool {
        is_opaque_directory(&self.upper_path(rel))
    }

    fn resolve_component_metadata(&self, rel: &Path) -> io::Result<Option<ResolvedMetadata>> {
        self.resolve_component_metadata_with_parents(rel, None, None)
    }

    fn resolve_component_metadata_with_parents(
        &self,
        rel: &Path,
        mut parents: Option<&mut Vec<BackingIdentity>>,
        mut checked_directories: Option<&mut HashSet<PathBuf>>,
    ) -> io::Result<Option<ResolvedMetadata>> {
        if let Some((path, metadata)) = layer_metadata_with_parents(
            &self.upper,
            rel,
            parents.as_deref_mut(),
            &self.profile,
            checked_directories.as_deref_mut(),
        )? {
            return Ok(Some(ResolvedMetadata {
                resolved: Resolved {
                    path,
                    is_upper: true,
                },
                metadata,
                layer: 0,
            }));
        }
        let name = rel.file_name().ok_or_else(|| error(libc::EINVAL))?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        if self.is_whiteouted(parent, name) || self.is_opaque(parent) {
            return Ok(None);
        }
        for (index, lower) in self.layout.lowers.iter().enumerate() {
            if let Some((path, metadata)) = layer_metadata_with_parents(
                lower,
                rel,
                parents.as_deref_mut(),
                &self.profile,
                checked_directories.as_deref_mut(),
            )? {
                return Ok(Some(ResolvedMetadata {
                    resolved: Resolved {
                        path,
                        is_upper: false,
                    },
                    metadata,
                    layer: index + 1,
                }));
            }
        }
        Ok(None)
    }

    pub fn resolve(&self, rel: &Path) -> Option<Resolved> {
        self.resolve_checked(rel).ok().flatten()
    }

    /// Resolve while preserving permission and I/O failures. `None` means
    /// genuine absence from the merged view, not a denied alias or failed stat.
    pub fn resolve_checked(&self, rel: &Path) -> io::Result<Option<Resolved>> {
        Ok(self
            .resolve_metadata_checked(rel)?
            .map(|item| item.resolved))
    }

    fn resolve_metadata_checked(&self, rel: &Path) -> io::Result<Option<ResolvedMetadata>> {
        self.resolve_metadata_checked_with_parents(rel, None)
    }

    fn resolve_metadata_checked_with_parents(
        &self,
        rel: &Path,
        parents: Option<&mut Vec<BackingIdentity>>,
    ) -> io::Result<Option<ResolvedMetadata>> {
        self.resolve_metadata_walk::<true>(rel, parents, |_| Ok(()))
    }

    fn resolve_metadata_walk<const REUSE_DIRECTORIES: bool>(
        &self,
        rel: &Path,
        mut parents: Option<&mut Vec<BackingIdentity>>,
        mut after_prefix: impl FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<Option<ResolvedMetadata>> {
        let _span = self.profile.span("resolve");
        self.require_visible(rel)?;
        if rel.as_os_str().is_empty() {
            return Ok(Some(ResolvedMetadata {
                resolved: Resolved {
                    path: self.upper.clone(),
                    is_upper: true,
                },
                metadata: fs::symlink_metadata(&self.upper)?,
                layer: 0,
            }));
        }
        let mut current = PathBuf::new();
        let mut resolved = None;
        let count = rel.components().count();
        // Reuse only successful directory checks within this walk. Every
        // logical prefix still gets fresh leaf metadata and alias checks.
        // The final component rechecks ALL physical ancestors, including
        // those in losing layers; neither attributes nor absence are cached.
        // This retains the existing non-atomic host namespace contract.
        let mut checked = (REUSE_DIRECTORIES && count > 2).then(HashSet::new);
        for (index, component) in rel.components().enumerate() {
            self.profile.add("resolve_components", 1);
            current.push(component.as_os_str());
            let observed_parents = if index + 1 == count {
                parents.as_deref_mut()
            } else {
                None
            };
            let Some(item) = self.resolve_component_metadata_with_parents(
                &current,
                observed_parents,
                if index + 1 == count {
                    None
                } else {
                    checked.as_mut()
                },
            )?
            else {
                return Ok(None);
            };
            self.require_unaliased_metadata(&item.metadata)?;
            if index + 1 != count && !item.metadata.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
            resolved = Some(item);
            after_prefix(&current)?;
        }
        Ok(resolved)
    }

    /// Preserve absence observations and policy/alias checks while returning
    /// the backing identity and metadata from this same resolution.
    pub fn metadata_resolved(&self, rel: &Path) -> io::Result<ResolvedMetadata> {
        self.metadata_resolved_with_parents(rel, None)
    }

    /// Reuse identities already read by the selected layer's parent checks.
    /// Callers must still resolve policy and backing metadata on every request;
    /// these identities permit reuse of an inode reference, not its attributes.
    pub fn metadata_for_backing_lookup(&self, rel: &Path) -> io::Result<BackingResolution> {
        let mut parents = Vec::with_capacity(rel.components().count().saturating_sub(1));
        let entry = self.metadata_resolved_with_parents(rel, Some(&mut parents))?;
        Ok(BackingResolution { entry, parents })
    }

    /// Prepare a no-follow file read and record the baseline observation before
    /// returning its backing. Reject symlinks before reading their preimage.
    /// A live journal may hash/publish while the host changes the view, so the
    /// returned metadata is checked again after publication. Frozen/no-journal
    /// reads need only one resolution. This is request-local evidence, not an
    /// authorization capability for a future open or a content snapshot.
    pub fn prepare_file_read(&self, rel: &Path) -> io::Result<ResolvedMetadata> {
        self.prepare_file_read_with_parents(rel, None)
    }

    /// Like `prepare_file_read`, also return freshly checked physical parents
    /// for a native adapter's bounded directory-reference cache.
    pub fn prepare_file_read_for_backing_lookup(
        &self,
        rel: &Path,
    ) -> io::Result<BackingResolution> {
        let mut parents = Vec::with_capacity(rel.components().count().saturating_sub(1));
        let entry = self.prepare_file_read_with_parents(rel, Some(&mut parents))?;
        Ok(BackingResolution { entry, parents })
    }

    fn prepare_file_read_with_parents(
        &self,
        rel: &Path,
        parents: Option<&mut Vec<BackingIdentity>>,
    ) -> io::Result<ResolvedMetadata> {
        let _span = self.profile.span("observe_read");
        if !self.layout.frozen_baseline && self.preimage_dir.is_some() {
            let before = self.metadata_resolved(rel)?;
            if before.metadata.file_type().is_symlink() {
                return Err(error(libc::ELOOP));
            }
            self.capture_preimage(rel, false)?;
        }
        let backing = self.metadata_resolved_with_parents(rel, parents)?;
        if backing.metadata.file_type().is_symlink() {
            return Err(error(libc::ELOOP));
        }
        Ok(backing)
    }

    fn metadata_resolved_with_parents(
        &self,
        rel: &Path,
        parents: Option<&mut Vec<BackingIdentity>>,
    ) -> io::Result<ResolvedMetadata> {
        let _span = self.profile.span("metadata");
        let Some(resolved) = self.resolve_metadata_checked_with_parents(rel, parents)? else {
            self.observe_absence(rel)?;
            return Err(error(libc::ENOENT));
        };
        Ok(resolved)
    }

    pub fn metadata(&self, rel: &Path) -> io::Result<Metadata> {
        Ok(self.metadata_resolved(rel)?.metadata)
    }

    pub fn exists_in_lower(&self, rel: &Path) -> bool {
        if self.require_visible(rel).is_err() {
            return false;
        }
        self.layout
            .lowers
            .iter()
            .any(|lower| layer_path(lower, rel).ok().flatten().is_some())
    }

    fn copy_metadata(
        &self,
        source: &Path,
        destination: &Path,
        metadata: &Metadata,
    ) -> io::Result<()> {
        let nofollow = metadata.file_type().is_symlink();
        if let Err(err) = sys::chown(destination, metadata.uid(), metadata.gid(), nofollow)
            && !ignorable_ownership_error(&err)
        {
            return Err(err);
        }
        if !nofollow {
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode(metadata.mode() & 0o7777),
            )?;
        }
        if let Err(err) = sys::copy_xattrs(source, destination)
            && !ignorable_metadata_error(&err)
        {
            return Err(err);
        }
        let atime = sys::unix_time(metadata.atime(), metadata.atime_nsec());
        let mtime = sys::unix_time(metadata.mtime(), metadata.mtime_nsec());
        if let Err(err) = sys::set_times(destination, Some(atime), Some(mtime), nofollow)
            && !ignorable_metadata_error(&err)
        {
            return Err(err);
        }
        Ok(())
    }

    pub fn ensure_upper_parents(&self, rel: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        let Some(parent) = rel.parent() else {
            return Ok(());
        };
        let mut current = PathBuf::new();
        for component in parent.components() {
            current.push(component.as_os_str());
            let upper = self.upper_path(&current);
            if exists(&upper) {
                if !fs::symlink_metadata(&upper)?.is_dir() {
                    return Err(error(libc::ENOTDIR));
                }
                continue;
            }
            let resolved = self.resolve(&current).ok_or_else(|| error(libc::ENOENT))?;
            let metadata = fs::symlink_metadata(&resolved.path)?;
            if !metadata.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
            // Creating a child changes this copied-up directory's metadata,
            // and apply may promote that metadata even though the Agent did
            // not issue an explicit setattr on the parent.
            self.record_preimage(&current)?;
            fs::create_dir(&upper)?;
            self.copy_metadata(&resolved.path, &upper, &metadata)?;
        }
        Ok(())
    }

    fn temporary_path(&self, parent: &Path) -> PathBuf {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        self.work
            .as_deref()
            .unwrap_or(parent)
            .join(format!("{TEMP_PREFIX}{}-{id}", std::process::id()))
    }

    pub fn copy_up(&self, rel: &Path) -> io::Result<PathBuf> {
        let _span = self.profile.span("copy_up");
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        let upper = self.upper_path(rel);
        if exists(&upper) {
            self.require_unaliased(&upper)?;
            self.record_preimage(rel)?;
            return Ok(upper);
        }
        let resolved = self.resolve(rel).ok_or_else(|| error(libc::ENOENT))?;
        self.record_preimage(rel)?;
        if resolved.is_upper {
            return Ok(resolved.path);
        }
        self.ensure_upper_parents(rel)?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        let parent = upper.parent().ok_or_else(|| error(libc::EINVAL))?;
        let temporary = self.temporary_path(parent);
        let result = (|| {
            let kind = metadata.file_type();
            let mut reused_upper_inode = false;
            if kind.is_dir() {
                fs::create_dir(&temporary)?;
            } else if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&resolved.path)?, &temporary)?;
            } else if kind.is_file() {
                let identity = (metadata.dev(), metadata.ino());
                let existing = if metadata.nlink() > 1 {
                    self.copied_hard_links.lock().ok().and_then(|links| {
                        links
                            .get(&identity)?
                            .iter()
                            .find(|path| exists(path))
                            .cloned()
                    })
                } else {
                    None
                };
                if let Some(existing) = existing {
                    fs::hard_link(existing, &temporary)?;
                    reused_upper_inode = true;
                } else {
                    let mut options = OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(metadata.mode() & 0o7777);
                    let mut destination = options.open(&temporary)?;
                    let mut source = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&resolved.path)?;
                    let copied = io::copy(&mut source, &mut destination)?;
                    self.profile.add("copy_up_bytes", copied);
                }
            } else {
                sys::mknod(&temporary, metadata.mode(), metadata.rdev() as u32)?;
            }
            if !reused_upper_inode {
                self.copy_metadata(&resolved.path, &temporary, &metadata)?;
            }
            fs::rename(&temporary, &upper)?;
            if metadata.is_file()
                && metadata.nlink() > 1
                && let Ok(mut links) = self.copied_hard_links.lock()
            {
                links
                    .entry((metadata.dev(), metadata.ino()))
                    .or_default()
                    .push(upper.clone());
                self.hard_link_sources
                    .lock()
                    .map_err(|_| error(libc::EIO))?
                    .entry((metadata.dev(), metadata.ino()))
                    .or_insert_with(|| resolved.path.clone());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = if temporary.is_dir() {
                fs::remove_dir_all(&temporary)
            } else {
                fs::remove_file(&temporary)
            };
        }
        result.map(|()| upper)
    }

    fn directory_entries(
        &self,
        rel: &Path,
        check_children: bool,
    ) -> io::Result<Vec<DirectoryEntry>> {
        let _span = self.profile.span("list_entries");
        self.require_visible(rel)?;
        let metadata = self.metadata(rel)?;
        if !metadata.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        let mut names = BTreeSet::new();
        if !self.is_opaque(rel) {
            for lower in &self.layout.lowers {
                let Some(directory) = layer_path(lower, rel)? else {
                    continue;
                };
                if !fs::symlink_metadata(&directory)?.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(directory)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    if !Self::is_whiteout_name(&name) {
                        names.insert(name);
                    }
                }
            }
        }
        if let Some(directory) = layer_path(&self.upper, rel)? {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name();
                if !Self::is_whiteout_name(&name) {
                    names.insert(name);
                }
            }
        }
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            if self.is_whiteouted(rel, &name) {
                continue;
            }
            let child = Self::child(rel, &name)?;
            if self.is_excluded(&child) {
                continue;
            }
            // Match name filtering: denied/failed children are hidden. Never
            // convert an inaccessible child into an observed absence.
            if let Ok(Some(backing)) = self.resolve_component_metadata(&child)
                && self.require_unaliased_metadata(&backing.metadata).is_ok()
            {
                if check_children {
                    self.require_visible(&child)?;
                }
                entries.push(DirectoryEntry { name, backing });
            }
        }
        Ok(entries)
    }

    pub fn list_entries(&self, rel: &Path) -> io::Result<Vec<DirectoryEntry>> {
        self.directory_entries(rel, true)
    }

    pub fn list_names(&self, rel: &Path) -> io::Result<Vec<OsString>> {
        Ok(self
            .directory_entries(rel, false)?
            .into_iter()
            .map(|item| item.name)
            .collect())
    }

    /// Record an explicit metadata mutation separately from incidental root mtime.
    pub fn prepare_metadata_change(&self, rel: &Path) -> io::Result<PathBuf> {
        let upper = self.copy_up(rel)?;
        if rel.as_os_str().is_empty() {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(upper.join(ROOT_METADATA_NAME))?;
        }
        Ok(upper)
    }

    fn mark_opaque(&self, rel: &Path) -> io::Result<()> {
        let path = self.upper_path(rel);
        let marker = path.join(OPAQUE_NAME);
        if !exists(&marker) {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(marker)?;
        }
        for name in OPAQUE_XATTRS {
            match sys::set_xattr(&path, OsStr::new(name), b"y", 0) {
                Ok(()) => break,
                Err(error) if ignorable_metadata_error(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn create_whiteout(&self, rel: &Path) -> io::Result<()> {
        self.ensure_upper_parents(rel)?;
        let name = rel.file_name().ok_or_else(|| error(libc::EINVAL))?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let marker = self.whiteout_path(parent, name);
        if !exists(&marker) {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(marker)?;
        }
        Ok(())
    }

    pub fn clear_whiteout(&self, rel: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        let name = rel.file_name().ok_or_else(|| error(libc::EINVAL))?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let marker = self.whiteout_path(parent, name);
        match fs::remove_file(marker) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Prepare a destination for adapters that create nodes via passthrough.
    /// The destination preimage must be durable before clearing its whiteout.
    pub fn prepare_create(&self, rel: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        self.record_preimage(rel)?;
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)
    }

    pub fn create_file(&self, rel: &Path, mode: u32, flags: i32) -> io::Result<File> {
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(rel)?;
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        let access_mode = flags & libc::O_ACCMODE;
        let mut options = OpenOptions::new();
        options
            .read(access_mode != libc::O_WRONLY)
            .write(access_mode != libc::O_RDONLY)
            .append(flags & libc::O_APPEND != 0)
            .truncate(flags & libc::O_TRUNC != 0)
            .create_new(true)
            .mode(mode & 0o7777)
            .custom_flags(flags & !(libc::O_ACCMODE | libc::O_CREAT | libc::O_EXCL));
        options.open(self.upper_path(rel))
    }

    pub fn create_dir(&self, rel: &Path, mode: u32) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(rel)?;
        let shadows_lower = self.exists_in_lower(rel);
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        let path = self.upper_path(rel);
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o7777))?;
        if shadows_lower {
            self.mark_opaque(rel)?;
        }
        Ok(())
    }

    pub fn create_symlink(&self, rel: &Path, target: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(rel)?;
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        std::os::unix::fs::symlink(target, self.upper_path(rel))
    }

    pub fn create_node(&self, rel: &Path, mode: u32, rdev: u32) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(rel)?;
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        sys::mknod(&self.upper_path(rel), mode, rdev)
    }

    pub fn remove(&self, rel: &Path, directory: bool) -> io::Result<()> {
        self.require_visible(rel)?;
        if directory {
            self.require_tree_access(rel, rel)?;
        }
        let resolved = self.resolve(rel).ok_or_else(|| error(libc::ENOENT))?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        if directory {
            if !metadata.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
            if !self.list_names(rel)?.is_empty() {
                return Err(error(libc::ENOTEMPTY));
            }
        } else if metadata.is_dir() {
            return Err(error(libc::EISDIR));
        }
        self.record_logical_tree_mapping(rel, rel)?;

        if resolved.is_upper {
            if directory {
                fs::remove_dir_all(&resolved.path)?;
            } else {
                fs::remove_file(&resolved.path)?;
            }
            self.forget_copied_hard_links(&resolved.path);
        }
        if self.exists_in_lower(rel) {
            self.create_whiteout(rel)?;
        }
        Ok(())
    }

    /// Materialize the complete merged subtree and make its root opaque.
    ///
    /// This is required before renaming a lower-backed directory: a single-node
    /// copy-up would otherwise lose every child when the upper directory moves.
    pub fn materialize_tree(&self, rel: &Path) -> io::Result<PathBuf> {
        self.require_visible(rel)?;
        let metadata = self.metadata(rel)?;
        if !metadata.is_dir() {
            return self.copy_up(rel);
        }
        let names = self.list_names(rel)?;
        self.copy_up(rel)?;
        for name in names {
            let child = Self::child(rel, &name)?;
            if self.metadata(&child)?.is_dir() {
                self.materialize_tree(&child)?;
            } else {
                self.copy_up(&child)?;
            }
        }
        self.mark_opaque(rel)?;
        Ok(self.upper_path(rel))
    }

    fn validate_replacement(
        &self,
        old: &Path,
        new: &Path,
        no_replace: bool,
    ) -> io::Result<Option<PathBuf>> {
        let Some(destination) = self.resolve(new) else {
            return Ok(None);
        };
        if no_replace {
            return Err(error(libc::EEXIST));
        }
        let source_meta = self.metadata(old)?;
        let destination_meta = fs::symlink_metadata(&destination.path)?;
        match (source_meta.is_dir(), destination_meta.is_dir()) {
            (true, false) => return Err(error(libc::ENOTDIR)),
            (false, true) => return Err(error(libc::EISDIR)),
            (true, true) if !self.list_names(new)?.is_empty() => {
                return Err(error(libc::ENOTEMPTY));
            }
            _ => {}
        }
        Ok(destination.is_upper.then_some(destination.path))
    }

    fn remove_physical(path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path)?.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    fn forget_copied_hard_links(&self, removed: &Path) {
        let Ok(mut links) = self.copied_hard_links.lock() else {
            return;
        };
        links.retain(|_, paths| {
            paths.retain(|path| !path.starts_with(removed));
            !paths.is_empty()
        });
        if let Ok(mut sources) = self.hard_link_sources.lock() {
            sources.retain(|identity, _| links.contains_key(identity));
        }
    }

    fn remap_copied_hard_links(&self, old: &Path, new: &Path) {
        let Ok(mut links) = self.copied_hard_links.lock() else {
            return;
        };
        for path in links.values_mut().flatten() {
            if (path == old || path.starts_with(old))
                && let Ok(suffix) = path.strip_prefix(old)
            {
                *path = if suffix.as_os_str().is_empty() {
                    new.to_path_buf()
                } else {
                    new.join(suffix)
                };
            }
        }
    }

    fn exchange_copied_hard_links(&self, first: &Path, second: &Path) {
        let Ok(mut links) = self.copied_hard_links.lock() else {
            return;
        };
        for path in links.values_mut().flatten() {
            if path == first || path.starts_with(first) {
                if let Ok(suffix) = path.strip_prefix(first) {
                    *path = if suffix.as_os_str().is_empty() {
                        second.to_path_buf()
                    } else {
                        second.join(suffix)
                    };
                }
            } else if (path == second || path.starts_with(second))
                && let Ok(suffix) = path.strip_prefix(second)
            {
                *path = if suffix.as_os_str().is_empty() {
                    first.to_path_buf()
                } else {
                    first.join(suffix)
                };
            }
        }
    }

    pub fn rename(&self, old: &Path, new: &Path, no_replace: bool) -> io::Result<()> {
        self.require_visible(old)?;
        self.require_visible(new)?;
        Self::validate_rel(old)?;
        Self::validate_rel(new)?;
        if old == new {
            return Ok(());
        }
        let source_meta = self.metadata(old)?;
        if source_meta.is_dir() && new.starts_with(old) {
            return Err(error(libc::EINVAL));
        }
        let replaced_upper = self.validate_replacement(old, new, no_replace)?;
        self.require_tree_access(old, new)?;
        if self.resolve(new).is_some() {
            self.require_tree_access(new, new)?;
        }
        self.record_logical_tree_mapping(old, old)?;
        self.record_logical_tree_mapping(old, new)?;
        let source = if source_meta.is_dir() {
            self.materialize_tree(old)?
        } else {
            self.copy_up(old)?
        };
        self.ensure_upper_parents(new)?;
        self.clear_whiteout(new)?;
        let source_needs_whiteout = self.exists_in_lower(old);
        if source_needs_whiteout {
            self.create_whiteout(old)?;
        }
        let backup = replaced_upper
            .as_ref()
            .map(|_| self.temporary_path(self.upper()));
        if let (Some(destination), Some(backup)) = (&replaced_upper, &backup)
            && let Err(error) = fs::rename(destination, backup)
        {
            if source_needs_whiteout {
                let _ = self.clear_whiteout(old);
            }
            return Err(error);
        }
        if let Err(error) = fs::rename(&source, self.upper_path(new)) {
            if let (Some(destination), Some(backup)) = (&replaced_upper, &backup) {
                let _ = fs::rename(backup, destination);
            }
            if source_needs_whiteout {
                let _ = self.clear_whiteout(old);
            }
            return Err(error);
        }
        if let Some(backup) = backup
            && let Err(error) = Self::remove_physical(&backup)
        {
            log::warn!(
                "rename committed but cleanup of {} failed: {error}",
                backup.display()
            );
        }
        self.forget_copied_hard_links(&self.upper_path(new));
        self.remap_copied_hard_links(&self.upper_path(old), &self.upper_path(new));
        Ok(())
    }

    pub fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
        if self.access.has_denials() {
            return Err(error(libc::EACCES));
        }
        self.require_visible(source)?;
        self.require_visible(destination)?;
        let metadata = self.metadata(source)?;
        if metadata.is_dir() {
            return Err(error(libc::EPERM));
        }
        if self.resolve(destination).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(destination)?;
        let source = self.copy_up(source)?;
        self.ensure_upper_parents(destination)?;
        self.clear_whiteout(destination)?;
        let destination = self.upper_path(destination);
        fs::hard_link(&source, &destination)?;
        if let Ok(mut links) = self.copied_hard_links.lock()
            && let Some(paths) = links.values_mut().find(|paths| paths.contains(&source))
        {
            paths.push(destination);
        }
        Ok(())
    }

    pub fn exchange(&self, first: &Path, second: &Path) -> io::Result<()> {
        self.require_visible(first)?;
        self.require_visible(second)?;
        Self::validate_rel(first)?;
        Self::validate_rel(second)?;
        if first == second {
            return Ok(());
        }
        if first.starts_with(second) || second.starts_with(first) {
            return Err(error(libc::EINVAL));
        }
        let first_meta = self.metadata(first)?;
        let second_meta = self.metadata(second)?;
        self.require_tree_access(first, second)?;
        self.require_tree_access(second, first)?;
        self.record_logical_tree_mapping(first, first)?;
        self.record_logical_tree_mapping(second, second)?;
        self.record_logical_tree_mapping(first, second)?;
        self.record_logical_tree_mapping(second, first)?;
        let first_upper = if first_meta.is_dir() {
            self.materialize_tree(first)?
        } else {
            self.copy_up(first)?
        };
        let second_upper = if second_meta.is_dir() {
            self.materialize_tree(second)?
        } else {
            self.copy_up(second)?
        };
        let temporary = self.temporary_path(self.upper());
        fs::rename(&first_upper, &temporary)?;
        if let Err(error) = fs::rename(&second_upper, &first_upper) {
            let _ = fs::rename(&temporary, &first_upper);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &second_upper) {
            let _ = fs::rename(&first_upper, &second_upper);
            let _ = fs::rename(&temporary, &first_upper);
            return Err(error);
        }
        self.exchange_copied_hard_links(&first_upper, &second_upper);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn negative_observation_remains_absent_when_host_creates_before_publication() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let journal = temp.path().join("preimages");
        fs::create_dir(&target).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            temp.path().join("upper"),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        assert!(core.resolve_checked(Path::new("new")).unwrap().is_none());
        core.capture_preimage_after_check(Path::new("new"), false, true, || {
            fs::write(target.join("new"), b"host creation after negative lookup")
        })
        .unwrap();
        assert_eq!(
            load_preimages(&journal).unwrap()[0].state,
            PathFingerprint::Absent
        );
        core.copy_up(Path::new("new")).unwrap();
        assert_eq!(
            load_preimages(&journal).unwrap()[0].state,
            PathFingerprint::Absent
        );
        assert_ne!(
            fingerprint_at(&target, Path::new("new")).unwrap(),
            PathFingerprint::Absent
        );
    }
    #[test]
    fn two_cores_never_replace_the_first_observation_when_publication_races() {
        use super::*;
        use crate::apply::{OverlayRecord, OverlayState, OverlayUpper, apply_overlay};
        for durable_loser in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("target");
            let stage = temp.path().join("stage");
            let journal = stage.join("preimages");
            fs::create_dir(&target).unwrap();
            fs::write(target.join("value"), b"original").unwrap();
            let original = fingerprint_at(&target, Path::new("value")).unwrap();
            let loser = OverlayCore::new_with_exclusions_and_preimages(
                vec![target.clone()],
                stage.join("upper"),
                Some(stage.join("work")),
                vec![],
                Some(journal.clone()),
            )
            .unwrap();
            let winner = OverlayCore::new_with_exclusions_and_preimages(
                vec![target.clone()],
                temp.path().join("other-upper"),
                None,
                vec![],
                Some(journal.clone()),
            )
            .unwrap();
            // Both instances can see a missing journal destination. Schedule
            // the winner and host edit after the loser's missing-entry check,
            // before its fingerprint/publication; a replacing rename would
            // adopt "host edit" and permit the stale candidate to overwrite it.
            loser
                .capture_preimage_after_check(Path::new("value"), durable_loser, false, || {
                    winner.observe_read(Path::new("value"))?;
                    fs::write(target.join("value"), b"host edit")
                })
                .unwrap();
            let entries = load_preimages(&journal).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].state, original);
            assert_eq!(fs::read_dir(journal.join("entries")).unwrap().count(), 1);
            fs::write(
                loser.copy_up(Path::new("value")).unwrap(),
                b"stale agent edit",
            )
            .unwrap();
            drop(loser);
            drop(winner);
            let mut record = OverlayRecord {
                id: "shared-journal".into(),
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
    }

    #[test]
    fn opening_snapshot_backing_preserves_empty_root_and_copy_up_hard_links() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("lower");
        let upper = dir.path().join("upper");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("one"), b"before").unwrap();
        fs::hard_link(lower.join("one"), lower.join("two")).unwrap();
        let core = OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap();
        fs::set_permissions(&upper, fs::Permissions::from_mode(0o701)).unwrap();
        let before = fs::metadata(&upper).unwrap();
        drop(core);
        let open = || {
            OverlayCore::open_existing(vec![lower.clone()], upper.clone(), None, vec![], None)
                .unwrap()
        };
        let core = open();
        let after = fs::metadata(&upper).unwrap();
        assert_eq!(after.mode(), before.mode());
        assert_eq!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec())
        );
        core.copy_up(Path::new("one")).unwrap();
        core.rename(Path::new("one"), Path::new("renamed"), false)
            .unwrap();
        let state = core.capture_hard_links().unwrap();
        drop(core);
        let restored = open();
        restored.restore_hard_links(&state).unwrap();
        restored.copy_up(Path::new("two")).unwrap();
        assert_eq!(
            fs::metadata(upper.join("renamed")).unwrap().ino(),
            fs::metadata(upper.join("two")).unwrap().ino()
        );
        fs::write(upper.join("two"), b"continued").unwrap();
        assert_eq!(fs::read(upper.join("renamed")).unwrap(), b"continued");
        assert!(
            restored
                .restore_hard_links(&[(1, 2, vec![PathBuf::from("../outside")])])
                .is_err()
        );
    }

    #[test]
    fn explicit_root_metadata_is_reported_and_xattr_changes_conflict() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let upper = dir.path().join("upper");
        fs::create_dir(&base).unwrap();
        fs::write(base.join("file"), b"same contents").unwrap();
        let core = OverlayCore::new(vec![base.clone()], upper.clone(), None).unwrap();
        core.prepare_metadata_change(Path::new("")).unwrap();
        assert!(upper.join(ROOT_METADATA_NAME).is_file());
        assert_eq!(
            validate_guest_xattr(OsStr::new("user.overlay.opaque"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        let file = base.join("file");
        match sys::set_xattr(&file, OsStr::new("user.pvisor.conflict"), b"before", 0) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOTSUP) => return,
            Err(error) => panic!("xattr setup failed: {error}"),
        }
        let before = fingerprint_at(&base, Path::new("file")).unwrap();
        sys::set_xattr(&file, OsStr::new("user.pvisor.conflict"), b"after", 0).unwrap();
        assert!(!before.matches(&fingerprint_at(&base, Path::new("file")).unwrap()));
    }

    #[test]
    fn backing_symlink_alias_cannot_share_the_upper_and_work_directory() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let upper = dir.path().join("upper");
        fs::create_dir(&base).unwrap();
        fs::create_dir(&upper).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&upper, &alias).unwrap();
        assert!(OverlayCore::new(vec![base], upper, Some(alias)).is_err());
    }

    #[test]
    fn masked_lower_symlink_cannot_supply_children_outside_its_layer() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("top");
        let base = dir.path().join("base");
        let outside = dir.path().join("outside");
        fs::create_dir_all(top.join("masked")).unwrap();
        fs::create_dir(&base).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, base.join("masked")).unwrap();
        let core =
            OverlayCore::new(vec![top, base.clone()], dir.path().join("upper"), None).unwrap();
        assert!(core.resolve(Path::new("masked/secret")).is_none());
        assert!(core.list_names(Path::new("masked")).unwrap().is_empty());
        assert_eq!(
            fingerprint_at(&base, Path::new("masked/secret")).unwrap(),
            PathFingerprint::Absent
        );
    }

    #[test]
    fn copying_a_hardlink_alias_keeps_edited_upper_metadata() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        fs::create_dir(&base).unwrap();
        fs::write(base.join("a"), b"original").unwrap();
        fs::hard_link(base.join("a"), base.join("b")).unwrap();
        let core = OverlayCore::new(vec![base], dir.path().join("upper"), None).unwrap();
        let a = core.copy_up(Path::new("a")).unwrap();
        fs::set_permissions(&a, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&a, b"edited").unwrap();
        let b = core.copy_up(Path::new("b")).unwrap();
        assert_eq!(
            fs::metadata(&b).unwrap().ino(),
            fs::metadata(&a).unwrap().ino()
        );
        assert_eq!(fs::metadata(&a).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read(b).unwrap(), b"edited");
    }

    #[test]
    fn snapshot_layout_uses_its_target_corresponding_frozen_baseline() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let snapshot = dir.path().join("snapshot");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&snapshot).unwrap();
        fs::write(target.join("file"), b"original").unwrap();
        fs::write(snapshot.join("file"), b"snapshot").unwrap();
        let layout =
            OverlayLayout::with_baseline(vec![snapshot.clone()], target.clone(), Some(&snapshot))
                .unwrap();
        let journal = dir.path().join("preimages");
        let core = OverlayCore::new_for_layout(
            layout,
            dir.path().join("upper"),
            None,
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();
        let file = core.copy_up(Path::new("file")).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"snapshot");
        assert_eq!(
            load_preimages(&journal).unwrap()[0].state,
            fingerprint_at(&snapshot, Path::new("file")).unwrap()
        );
    }
    #[test]
    fn layout_rejects_a_different_apply_baseline_before_creating_upper() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("base");
        let layer = directory.path().join("layer");
        std::fs::create_dir(&base).unwrap();
        std::fs::create_dir(&layer).unwrap();
        let upper = directory.path().join("upper");
        assert!(
            super::OverlayCore::new_for_target(
                vec![layer.clone(), base.clone()],
                layer,
                upper.clone(),
                None,
                Vec::new(),
                None
            )
            .is_err()
        );
        assert!(!upper.exists());
        assert!(
            super::OverlayCore::new_for_target(
                vec![base.clone()],
                base,
                upper,
                None,
                Vec::new(),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn composed_lower_preimage_tracks_apply_target_not_visible_layer() {
        let temporary = tempfile::tempdir().unwrap();
        let top = temporary.path().join("top");
        let target = temporary.path().join("target");
        let upper = temporary.path().join("upper");
        let journal = temporary.path().join("preimages");
        fs::create_dir(&top).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(top.join("value"), b"composed").unwrap();
        fs::write(target.join("value"), b"original target").unwrap();
        let expected = fingerprint_at(&target, Path::new("value")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![top, target],
            upper.clone(),
            None,
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();
        core.copy_up(Path::new("value")).unwrap();
        assert_eq!(fs::read(upper.join("value")).unwrap(), b"composed");
        assert_eq!(load_preimages(&journal).unwrap()[0].state, expected);
    }

    #[test]
    fn file_fingerprint_hashes_across_buffer_boundaries() {
        let temporary = tempfile::tempdir().unwrap();
        let content = vec![b'x'; 128 * 1024 + 3];
        fs::write(temporary.path().join("large"), &content).unwrap();
        assert_eq!(
            fingerprint_at(&temporary.path().join("large"), Path::new("")).unwrap(),
            fingerprint_at(temporary.path(), Path::new("large")).unwrap()
        );
        let PathFingerprint::File { sha256, .. } =
            fingerprint_at(temporary.path(), Path::new("large")).unwrap()
        else {
            panic!("expected file");
        };
        assert_eq!(sha256, sha256_hex(&content));
    }

    use super::*;
    use std::io::{Read, Write};
    use tempfile::TempDir;

    #[test]
    fn unmapped_ownership_is_ignorable_but_other_invalid_metadata_is_not() {
        let invalid = io::Error::from_raw_os_error(libc::EINVAL);
        assert!(ignorable_ownership_error(&invalid));
        assert!(!ignorable_metadata_error(&invalid));
    }

    struct Fixture {
        _temp: TempDir,
        lower1: PathBuf,
        lower2: PathBuf,
        upper: PathBuf,
        core: OverlayCore,
    }

    #[test]
    fn access_rules_cover_layers_new_files_and_directory_moves() {
        let mut fixture = Fixture::new();
        for root in [&fixture.lower1, &fixture.lower2, &fixture.upper] {
            fs::create_dir_all(root.join("project/.ssh")).unwrap();
            fs::write(root.join("project/.ssh/id_ed25519"), b"private").unwrap();
            fs::write(root.join("project/.env"), b"warn only").unwrap();
        }
        fixture.core = fixture.core.with_access_policy(
            &crate::FileAccessPolicy::new(
                vec!["**/.ssh".into(), "**/id_rsa".into()],
                vec!["**/.env".into()],
            )
            .unwrap(),
        );
        let core = &fixture.core;
        assert!(core.resolve(Path::new("project/.ssh/id_ed25519")).is_none());
        assert_eq!(
            core.list_names(Path::new("project")).unwrap(),
            [OsString::from(".env")]
        );
        assert!(core.copy_up(Path::new("project/.env")).is_ok());
        assert!(
            core.create_file(Path::new("id_rsa"), 0o600, libc::O_RDWR)
                .is_err()
        );
        assert!(
            core.rename(Path::new("project"), Path::new("renamed"), false)
                .is_err()
        );
        assert!(core.remove(Path::new("project/.env"), false).is_ok());
        assert!(core.remove(Path::new("project"), true).is_err());
        assert!(fixture.upper.join("project/.ssh/id_ed25519").exists());
        core.create_dir(Path::new("ordinary"), 0o700).unwrap();
        core.rename(Path::new("ordinary"), Path::new("renamed"), false)
            .unwrap();
    }

    #[test]
    fn access_rules_reject_hardlink_aliases_and_symlink_traversal() {
        let mut fixture = Fixture::new();
        for root in [&fixture.lower1, &fixture.upper] {
            fs::write(root.join("id_rsa"), b"private").unwrap();
            fs::hard_link(root.join("id_rsa"), root.join("alias")).unwrap();
        }
        fixture.core = fixture.core.with_access_policy(
            &crate::FileAccessPolicy::new(vec!["**/id_rsa".into()], vec![]).unwrap(),
        );
        let core = &fixture.core;
        assert!(core.resolve(Path::new("alias")).is_none());
        assert!(core.copy_up(Path::new("alias")).is_err());
        assert!(
            core.hard_link(Path::new("id_rsa"), Path::new("new-alias"))
                .is_err()
        );
        core.create_symlink(Path::new("link"), Path::new("id_rsa"))
            .unwrap();
        assert!(core.resolve(Path::new("link/child")).is_none());
        assert!(core.metadata(Path::new("../id_rsa")).is_err());
        assert!(core.metadata(Path::new("/id_rsa")).is_err());
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let lower1 = temp.path().join("lower1");
            let lower2 = temp.path().join("lower2");
            let upper = temp.path().join("upper");
            let work = temp.path().join("work");
            for directory in [&lower1, &lower2, &upper, &work] {
                fs::create_dir(directory).expect("create layer");
            }
            let core = OverlayCore::new(
                vec![lower1.clone(), lower2.clone()],
                upper.clone(),
                Some(work),
            )
            .expect("core");
            Self {
                _temp: temp,
                lower1,
                lower2,
                upper,
                core,
            }
        }
    }

    #[test]
    fn top_lower_wins_and_directories_merge() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.lower1.join("dir")).expect("dir");
        fs::create_dir(fixture.lower2.join("dir")).expect("dir");
        fs::write(fixture.lower1.join("dir/a"), b"a").expect("a");
        fs::write(fixture.lower1.join("dir/shared"), b"top").expect("top");
        fs::write(fixture.lower2.join("dir/b"), b"b").expect("b");
        fs::write(fixture.lower2.join("dir/shared"), b"bottom").expect("bottom");

        let names = fixture.core.list_names(Path::new("dir")).expect("names");
        assert_eq!(
            names,
            vec![
                OsString::from("a"),
                OsString::from("b"),
                OsString::from("shared")
            ]
        );
        let resolved = fixture
            .core
            .resolve(Path::new("dir/shared"))
            .expect("resolved");
        assert_eq!(fs::read(resolved.path).expect("read"), b"top");
    }

    #[test]
    fn recreating_a_whiteouted_directory_is_opaque() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.lower2.join("old")).expect("old");
        fixture
            .core
            .remove(Path::new("old"), true)
            .expect("whiteout");
        fixture
            .core
            .create_dir(Path::new("old"), 0o755)
            .expect("mkdir");
        assert!(fixture.upper.join("old").join(OPAQUE_NAME).is_file());
        assert!(
            fixture
                .core
                .list_names(Path::new("old"))
                .expect("names")
                .is_empty()
        );
    }

    #[test]
    fn renaming_lower_directory_keeps_complete_merged_tree() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.lower1.join("tree/nested")).expect("tree");
        fs::create_dir_all(fixture.lower2.join("tree/nested")).expect("tree");
        fs::write(fixture.lower1.join("tree/a"), b"a").expect("a");
        fs::write(fixture.lower2.join("tree/b"), b"b").expect("b");
        fs::write(fixture.lower1.join("tree/nested/c"), b"c").expect("c");

        fixture
            .core
            .rename(Path::new("tree"), Path::new("moved"), false)
            .expect("rename");

        assert!(fixture.core.resolve(Path::new("tree")).is_none());
        for path in ["moved/a", "moved/b", "moved/nested/c"] {
            assert!(fixture.core.resolve(Path::new(path)).is_some(), "{path}");
        }
        assert!(fixture.upper.join("moved").join(OPAQUE_NAME).is_file());
    }

    #[test]
    fn copy_up_preserves_contents_mode_and_xattrs_when_supported() {
        let fixture = Fixture::new();
        let source = fixture.lower2.join("file");
        fs::write(&source, b"payload").expect("write");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).expect("chmod");
        let xattr_supported =
            sys::set_xattr(&source, OsStr::new("user.pvisor.test"), b"value", 0).is_ok();

        let copied = fixture.core.copy_up(Path::new("file")).expect("copy up");
        let mut contents = Vec::new();
        File::open(&copied)
            .expect("open")
            .read_to_end(&mut contents)
            .expect("read");
        assert_eq!(contents, b"payload");
        assert_eq!(
            fs::symlink_metadata(&copied).expect("meta").mode() & 0o777,
            0o751
        );
        if xattr_supported {
            assert_eq!(
                sys::get_xattr(&copied, OsStr::new("user.pvisor.test")).expect("xattr"),
                b"value"
            );
        }
    }

    #[test]
    fn hard_link_copies_up_once_and_shares_data() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("source"), b"before").expect("source");
        fixture
            .core
            .hard_link(Path::new("source"), Path::new("linked"))
            .expect("link");
        let mut linked = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(fixture.upper.join("linked"))
            .expect("open");
        linked.write_all(b"after").expect("write");
        assert_eq!(
            fs::read(fixture.upper.join("source")).expect("read"),
            b"after"
        );
    }

    #[test]
    fn lower_hard_links_remain_linked_after_independent_copy_up() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("one"), b"before").expect("one");
        fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("two")).expect("link");
        fixture.core.copy_up(Path::new("one")).expect("copy one");
        fixture.core.copy_up(Path::new("two")).expect("copy two");
        fs::write(fixture.upper.join("one"), b"after").expect("write");
        assert_eq!(fs::read(fixture.upper.join("two")).expect("read"), b"after");
        assert_eq!(
            fs::metadata(fixture.upper.join("one")).expect("one").ino(),
            fs::metadata(fixture.upper.join("two")).expect("two").ino()
        );
    }

    #[test]
    fn lower_hard_link_copy_up_does_not_reuse_replaced_paths() {
        for surviving_alias in ["none", "copied", "linked"] {
            for replace_by_rename in [false, true] {
                let fixture = Fixture::new();
                let core = &fixture.core;
                fs::write(fixture.lower2.join("one"), b"original").unwrap();
                fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("two")).unwrap();
                core.copy_up(Path::new("one")).unwrap();
                let replaced = match surviving_alias {
                    "copied" => {
                        fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("alias"))
                            .unwrap();
                        core.copy_up(Path::new("alias")).unwrap();
                        Path::new("alias")
                    }
                    "linked" => {
                        core.hard_link(Path::new("one"), Path::new("alias"))
                            .unwrap();
                        Path::new("one")
                    }
                    _ => Path::new("one"),
                };
                if surviving_alias != "none" {
                    fs::write(fixture.upper.join("one"), b"updated").unwrap();
                }
                if replace_by_rename {
                    core.create_file(Path::new("replacement"), 0o600, libc::O_WRONLY)
                        .unwrap()
                        .write_all(b"replacement")
                        .unwrap();
                    core.rename(Path::new("replacement"), replaced, false)
                        .unwrap();
                } else {
                    core.remove(replaced, false).unwrap();
                    core.create_file(replaced, 0o600, libc::O_WRONLY)
                        .unwrap()
                        .write_all(b"replacement")
                        .unwrap();
                }
                let copied = core.copy_up(Path::new("two")).unwrap();
                let expected = if surviving_alias == "none" {
                    "original"
                } else {
                    "updated"
                };
                assert_eq!(
                    fs::read_to_string(&copied).unwrap(),
                    expected,
                    "alias={surviving_alias}, rename={replace_by_rename}"
                );
                if surviving_alias != "none" {
                    let survivor = if surviving_alias == "copied" {
                        "one"
                    } else {
                        "alias"
                    };
                    assert_eq!(
                        fs::metadata(copied).unwrap().ino(),
                        fs::metadata(fixture.upper.join(survivor)).unwrap().ino()
                    );
                }
                assert_eq!(fs::read(core.upper_path(replaced)).unwrap(), b"replacement");
            }
        }
    }

    #[test]
    fn lower_hard_link_index_survives_rename() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("one"), b"before").expect("one");
        fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("two")).expect("link");
        fixture.core.copy_up(Path::new("one")).expect("copy one");
        fixture
            .core
            .rename(Path::new("one"), Path::new("moved"), false)
            .expect("rename");
        fixture.core.copy_up(Path::new("two")).expect("copy two");
        fs::write(fixture.upper.join("moved"), b"after").expect("write");
        assert_eq!(fs::read(fixture.upper.join("two")).expect("read"), b"after");
    }

    #[test]
    fn exchange_materializes_and_swaps_lower_entries() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("a"), b"a").expect("a");
        fs::create_dir(fixture.lower2.join("b")).expect("b");
        fs::write(fixture.lower2.join("b/child"), b"b").expect("child");

        fixture
            .core
            .exchange(Path::new("a"), Path::new("b"))
            .expect("exchange");

        assert_eq!(
            fs::read(fixture.core.resolve(Path::new("b")).expect("b").path).expect("read"),
            b"a"
        );
        assert_eq!(
            fs::read(
                fixture
                    .core
                    .resolve(Path::new("a/child"))
                    .expect("child")
                    .path
            )
            .expect("read"),
            b"b"
        );
    }

    #[test]
    fn excluded_subtree_is_absent_and_cannot_be_recreated() {
        let temporary = tempfile::tempdir().unwrap();
        let lower = temporary.path().join("lower");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        fs::create_dir_all(lower.join("visible")).unwrap();
        fs::create_dir_all(lower.join("internal/nested")).unwrap();
        fs::write(lower.join("internal/nested/control"), b"secret").unwrap();
        let core = OverlayCore::new_with_exclusions(
            vec![lower],
            upper,
            Some(work),
            vec![PathBuf::from("internal")],
        )
        .unwrap();

        assert!(core.resolve(Path::new("internal")).is_none());
        assert!(core.resolve(Path::new("internal/nested/control")).is_none());
        assert!(
            !core
                .list_names(Path::new(""))
                .unwrap()
                .contains(&OsString::from("internal"))
        );
        assert_eq!(
            core.create_dir(Path::new("internal"), 0o755)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        assert_eq!(
            core.rename(
                Path::new("visible"),
                Path::new("internal/replacement"),
                false
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[test]
    fn rename_replaces_empty_upper_directory_transactionally() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.lower2.join("source/child")).expect("source");
        fs::write(fixture.lower2.join("source/child/file"), b"data").expect("file");
        fixture
            .core
            .create_dir(Path::new("destination"), 0o755)
            .expect("destination");

        fixture
            .core
            .rename(Path::new("source"), Path::new("destination"), false)
            .expect("rename");

        assert!(fixture.core.resolve(Path::new("source")).is_none());
        assert_eq!(
            fs::read(
                fixture
                    .core
                    .resolve(Path::new("destination/child/file"))
                    .expect("file")
                    .path
            )
            .expect("read"),
            b"data"
        );
    }

    #[test]
    fn first_touch_preimage_is_durable_and_never_rebased() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        let journal = temporary.path().join("preimages");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("value.txt"), b"original").unwrap();
        let original = fingerprint_at(&target, Path::new("value.txt")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(work),
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();

        core.copy_up(Path::new("value.txt")).unwrap();
        fs::write(upper.join("value.txt"), b"staged").unwrap();
        fs::write(target.join("value.txt"), b"concurrent").unwrap();
        core.copy_up(Path::new("value.txt")).unwrap();

        let entries = load_preimages(&journal).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path(), Path::new("value.txt"));
        assert_eq!(entries[0].state, original);

        remove_preimages(&journal, &[PathBuf::from("value.txt")]).unwrap();
        fs::remove_file(upper.join("value.txt")).unwrap();
        let rebased = fingerprint_at(&target, Path::new("value.txt")).unwrap();
        core.copy_up(Path::new("value.txt")).unwrap();
        let entries = load_preimages(&journal).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, rebased);
    }

    #[test]
    fn preimage_journal_covers_create_remove_and_rename_destinations() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        let journal = temporary.path().join("preimages");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("source"), b"source").unwrap();
        fs::write(target.join("victim"), b"victim").unwrap();
        let source = fingerprint_at(&target, Path::new("source")).unwrap();
        let victim = fingerprint_at(&target, Path::new("victim")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target],
            upper,
            Some(work),
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();

        drop(
            core.create_file(Path::new("created"), 0o600, libc::O_RDWR)
                .unwrap(),
        );
        core.remove(Path::new("victim"), false).unwrap();
        core.rename(Path::new("source"), Path::new("moved"), false)
            .unwrap();

        let entries = load_preimages(&journal)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.relative_path(), entry.state))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(entries[Path::new("created")], PathFingerprint::Absent);
        assert_eq!(entries[Path::new("moved")], PathFingerprint::Absent);
        assert_eq!(entries[Path::new("source")], source);
        assert_eq!(entries[Path::new("victim")], victim);
    }
}
#[test]
fn resolved_metadata_and_directory_entries_preserve_layer_and_visibility() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let last = temp.path().join("last");
    let upper = temp.path().join("upper");
    for root in [&first, &last, &upper] {
        fs::create_dir(root).unwrap();
    }
    fs::write(first.join("shared"), b"first").unwrap();
    fs::write(last.join("shared"), b"last").unwrap();
    fs::write(last.join("last-only"), b"last").unwrap();
    fs::write(last.join("hidden"), b"hidden").unwrap();
    fs::write(upper.join("upper-only"), b"upper").unwrap();
    fs::write(upper.join(".wh.hidden"), b"").unwrap();
    let core = OverlayCore::new(vec![first.clone(), last], upper, None).unwrap();
    let entries = core.list_entries(Path::new("")).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|e| e.name.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["last-only", "shared", "upper-only"]
    );
    for entry in entries {
        let resolved = core.metadata_resolved(Path::new(&entry.name)).unwrap();
        assert_eq!(entry.backing.layer, resolved.layer);
        assert_eq!(entry.backing.metadata.ino(), resolved.metadata.ino());
        assert_eq!(entry.backing.metadata.dev(), resolved.metadata.dev());
    }
    let shared = core.metadata_resolved(Path::new("shared")).unwrap();
    assert_eq!(shared.layer, 1);
    assert_eq!(shared.resolved.path, first.join("shared"));
    assert_eq!(
        core.metadata_resolved(Path::new("last-only"))
            .unwrap()
            .layer,
        2
    );
    assert_eq!(
        core.metadata_resolved(Path::new("upper-only"))
            .unwrap()
            .layer,
        0
    );
    assert_eq!(
        core.metadata_resolved(Path::new("hidden"))
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOENT)
    );
    let core = core.with_access_policy(
        &crate::FileAccessPolicy::new_with_ask(vec![], vec!["shared".into()], vec![]).unwrap(),
    );
    // Name enumeration has historically retained ask names, but serving
    // their attributes must still require authorization.
    assert!(
        core.list_names(Path::new(""))
            .unwrap()
            .contains(&OsString::from("shared"))
    );
    assert_eq!(
        core.list_entries(Path::new("")).unwrap_err().raw_os_error(),
        Some(libc::EACCES)
    );
}

#[cfg(test)]
mod backing_resolution_tests {
    use super::*;

    #[test]
    fn deep_walk_rechecks_final_physical_ancestors_after_host_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let outside = temp.path().join("outside");
        fs::create_dir_all(lower.join("a/b/c")).unwrap();
        fs::create_dir_all(outside.join("b/c")).unwrap();
        fs::write(lower.join("a/b/c/file"), b"inside").unwrap();
        fs::write(outside.join("b/c/file"), b"outside").unwrap();
        let core = OverlayCore::new(vec![lower.clone()], temp.path().join("upper"), None).unwrap();
        let result = core
            .resolve_metadata_walk::<true>(Path::new("a/b/c/file"), None, |prefix| {
                if prefix == Path::new("a/b/c") {
                    fs::rename(lower.join("a"), lower.join("old-a"))?;
                    std::os::unix::fs::symlink(&outside, lower.join("a"))?;
                }
                Ok(())
            })
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn deep_walk_returns_fresh_selected_layer_parent_identities() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(lower.join("a/b/c/d/e/f/g/h")).unwrap();
        fs::create_dir_all(upper.join("a/b")).unwrap();
        fs::write(lower.join("a/b/c/d/e/f/g/h/file"), b"content").unwrap();
        let core = OverlayCore::new(vec![lower.clone()], upper, None)
            .unwrap()
            .with_profile(crate::profile::Profile::enabled("overlay-core"));
        let resolved = core
            .metadata_for_backing_lookup(Path::new("a/b/c/d/e/f/g/h/file"))
            .unwrap();
        assert_eq!(resolved.entry.layer, 1);
        assert_eq!(resolved.parents.len(), 8);
        let mut path = lower.clone();
        for (component, identity) in Path::new("a/b/c/d/e/f/g/h")
            .components()
            .zip(&resolved.parents)
        {
            path.push(component);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (identity.device, identity.inode),
                (metadata.dev(), metadata.ino())
            );
        }
        let report = core.profile_report().unwrap();
        // Eight fresh final lower checks, plus the missing upper candidate
        // and its uncached absence checks. The old prefix walk needed 36
        // lower ancestor checks alone for this one resolution.
        assert!(report.measurements["layer_parent_stats"].units < 36);
        fs::write(lower.join("a/b/c/d/e/f/g/h/file"), b"changed content").unwrap();
        assert_eq!(
            core.metadata(Path::new("a/b/c/d/e/f/g/h/file"))
                .unwrap()
                .len(),
            15
        );
    }

    #[test]
    fn frozen_baseline_reads_do_not_fingerprint_and_mutation_uses_frozen_preimage() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let frozen = temp.path().join("frozen");
        let journal = temp.path().join("preimages");
        for root in [&target, &frozen] {
            fs::create_dir(root).unwrap();
            fs::write(root.join("file"), b"frozen").unwrap();
        }
        let original = fingerprint_at(&frozen, Path::new("file")).unwrap();
        let layout =
            OverlayLayout::with_baseline(vec![frozen.clone()], target.clone(), Some(&frozen))
                .unwrap();
        let core = OverlayCore::new_for_layout(
            layout,
            temp.path().join("upper"),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap()
        .with_profile(crate::profile::Profile::enabled("overlay-core"));
        for _ in 0..3 {
            let prepared = core.prepare_file_read(Path::new("file")).unwrap();
            assert_eq!(fs::read(prepared.resolved.path).unwrap(), b"frozen");
            core.observe_read(Path::new("file")).unwrap();
        }
        assert!(load_preimages(&journal).unwrap().is_empty());
        assert_eq!(
            core.profile_report()
                .unwrap()
                .measurements
                .get("fingerprint_bytes")
                .map_or(0, |measurement| measurement.units),
            0
        );
        fs::write(target.join("file"), b"host change").unwrap();
        core.copy_up(Path::new("file")).unwrap();
        assert_eq!(load_preimages(&journal).unwrap()[0].state, original);
        assert_eq!(
            core.profile_report()
                .unwrap()
                .measurements
                .get("fingerprint_bytes")
                .map(|measurement| measurement.units),
            Some(6)
        );
    }

    #[test]
    fn prepared_file_reads_observe_baseline_not_visible_upper_and_never_rebase() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let upper = temp.path().join("upper");
        let journal = temp.path().join("preimages");
        fs::create_dir_all(target.join("dir")).unwrap();
        fs::write(target.join("dir/file"), b"baseline").unwrap();
        let original = fingerprint_at(&target, Path::new("dir/file")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        fs::create_dir_all(upper.join("dir")).unwrap();
        fs::write(upper.join("dir/file"), b"visible stage content").unwrap();

        let prepared = core
            .prepare_file_read_for_backing_lookup(Path::new("dir/file"))
            .unwrap();
        assert_eq!(prepared.entry.layer, 0);
        assert_eq!(prepared.parents.len(), 1);
        assert_eq!(
            fs::read(prepared.entry.resolved.path).unwrap(),
            b"visible stage content"
        );
        assert_eq!(load_preimages(&journal).unwrap()[0].state, original);
        fs::write(target.join("dir/file"), b"host change").unwrap();
        core.prepare_file_read(Path::new("dir/file")).unwrap();
        core.copy_up(Path::new("dir/file")).unwrap();
        assert_eq!(load_preimages(&journal).unwrap()[0].state, original);
    }

    #[test]
    fn prepared_reads_reject_symlinks_and_denied_aliases_before_observation() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let journal = temp.path().join("preimages");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("secret"), b"secret content").unwrap();
        fs::hard_link(target.join("secret"), target.join("alias")).unwrap();
        std::os::unix::fs::symlink("secret", target.join("link")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target],
            temp.path().join("upper"),
            None,
            vec![],
            Some(journal.clone()),
        )
        .unwrap()
        .with_access_policy(&crate::FileAccessPolicy::new(vec!["secret".into()], vec![]).unwrap());
        for (path, errno) in [
            ("link", libc::ELOOP),
            ("alias", libc::EACCES),
            ("secret", libc::EACCES),
        ] {
            assert_eq!(
                core.prepare_file_read(Path::new(path))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
        assert!(load_preimages(&journal).unwrap().is_empty());
        assert_eq!(
            core.prepare_file_read(Path::new("missing"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        let entries = load_preimages(&journal).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, PathFingerprint::Absent);
    }

    #[test]
    fn backing_parents_belong_to_selected_layer_not_merged_directories() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        let first = temp.path().join("first");
        let last = temp.path().join("last");
        for root in [&upper, &first, &last] {
            fs::create_dir_all(root.join("tree/branch")).unwrap();
        }
        fs::write(last.join("tree/branch/file"), b"last").unwrap();
        let core = OverlayCore::new(vec![first, last.clone()], upper, None).unwrap();
        let backing = core
            .metadata_for_backing_lookup(Path::new("tree/branch/file"))
            .unwrap();
        assert_eq!(backing.entry.layer, 2);
        assert_eq!(backing.parents.len(), 2);
        for (identity, path) in backing.parents.iter().zip(["tree", "tree/branch"]) {
            let metadata = fs::symlink_metadata(last.join(path)).unwrap();
            assert_eq!(identity.device, metadata.dev());
            assert_eq!(identity.inode, metadata.ino());
        }
        assert!(
            core.metadata_for_backing_lookup(Path::new(""))
                .unwrap()
                .parents
                .is_empty()
        );
    }
}

/// Mechanism screening only: clone cost is a lower bound for an owned-preimage
/// design (no policy, journal, durable publication, or apply comparison here).
#[cfg(all(test, target_os = "macos"))]
#[test]
#[ignore = "manual APFS fingerprint versus COW preimage screening"]
fn content_observation_benchmark() {
    use std::ffi::CString;
    use std::hint::black_box;
    use std::time::Instant;
    let temp = tempfile::tempdir_in("/private/tmp").unwrap();
    for (size, count) in [(16, 2048), (4096, 2048), (1024 * 1024, 32)] {
        let source = temp.path().join(format!("source-{size}"));
        fs::create_dir(&source).unwrap();
        let data: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
        let paths: Vec<_> = (0..count)
            .map(|index| PathBuf::from(format!("f{index}")))
            .collect();
        for path in &paths {
            fs::write(source.join(path), &data).unwrap();
        }
        let expected = fingerprint_at(&source, &paths[0]).unwrap();
        for round in -3i32..15 {
            let modes = if round % 2 == 0 {
                ["fingerprint", "clonefile"]
            } else {
                ["clonefile", "fingerprint"]
            };
            for mode in modes {
                let destination = temp.path().join(format!("copies-{size}-{round}"));
                let pairs = if mode == "clonefile" {
                    fs::create_dir(&destination).unwrap();
                    paths
                        .iter()
                        .map(|path| {
                            (
                                CString::new(source.join(path).as_os_str().as_bytes()).unwrap(),
                                CString::new(destination.join(path).as_os_str().as_bytes())
                                    .unwrap(),
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let started = Instant::now();
                if mode == "fingerprint" {
                    for path in &paths {
                        let actual = fingerprint_at(&source, path).unwrap();
                        assert_eq!(actual, expected);
                        black_box(actual);
                    }
                } else {
                    for (from, to) in &pairs {
                        // SAFETY: both owned C strings stay live during the
                        // call; no-follow clones to a fresh destination name.
                        let rc = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 1) };
                        assert_eq!(rc, 0, "clonefile failed: {}", io::Error::last_os_error());
                    }
                }
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                if mode == "clonefile" {
                    for path in &paths {
                        assert_eq!(fs::read(destination.join(path)).unwrap(), data);
                    }
                    // COW must preserve the captured content across a later
                    // source write; hard-linking the file would fail this check.
                    fs::write(source.join(&paths[0]), b"later host edit").unwrap();
                    assert_eq!(fs::read(destination.join(&paths[0])).unwrap(), data);
                    fs::write(source.join(&paths[0]), &data).unwrap();
                    fs::remove_dir_all(destination).unwrap();
                }
                println!(
                    "PVISOR_OBSERVATION_BENCH {}",
                    serde_json::json!({
                        "mode": mode, "size": size, "files": count, "round": round,
                        "elapsed_ms": elapsed_ms,
                        "scope": "host-warm-cache mechanism screening; clone excludes journal/policy/apply",
                    })
                );
            }
        }
    }
}

#[cfg(test)]
#[test]
#[ignore = "manual paired directory-walk performance measurement"]
fn directory_walk_paired_benchmark() {
    use std::time::Instant;
    let temp = tempfile::tempdir().unwrap();
    for depth in [1, 8] {
        let lower = temp.path().join(format!("lower-{depth}"));
        let upper = temp.path().join(format!("upper-{depth}"));
        let mut paths = Vec::new();
        for group in 0..32 {
            let mut parent = PathBuf::from(format!("d{group:02}"));
            for level in 1..depth {
                parent.push(format!("l{level}"));
            }
            fs::create_dir_all(lower.join(&parent)).unwrap();
            for file in 0..64 {
                let path = parent.join(format!("f{file:04}"));
                fs::write(lower.join(&path), b"small-file-fixture").unwrap();
                paths.push(path);
            }
        }
        let core = OverlayCore::new(vec![lower], upper, None).unwrap();
        // Check selected backing, stat fields and fresh parent identities
        // before timing. The baseline runs the same walker without reuse.
        for path in &paths {
            let mut before = Vec::new();
            let mut after = Vec::new();
            let a = core
                .resolve_metadata_walk::<false>(path, Some(&mut before), |_| Ok(()))
                .unwrap()
                .unwrap();
            let b = core
                .resolve_metadata_walk::<true>(path, Some(&mut after), |_| Ok(()))
                .unwrap()
                .unwrap();
            assert_eq!(a.resolved.path, b.resolved.path);
            assert_eq!(
                (a.layer, a.metadata.ino(), a.metadata.len()),
                (b.layer, b.metadata.ino(), b.metadata.len())
            );
            assert_eq!(before, after);
        }
        for round in 0..33 {
            let order = if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for reuse in order {
                let start = Instant::now();
                for path in &paths {
                    let mut parents = Vec::new();
                    let entry = if reuse {
                        core.resolve_metadata_walk::<true>(path, Some(&mut parents), |_| Ok(()))
                    } else {
                        core.resolve_metadata_walk::<false>(path, Some(&mut parents), |_| Ok(()))
                    }
                    .unwrap()
                    .unwrap();
                    std::hint::black_box((entry, parents));
                }
                println!(
                    "PVISOR_WALK_BENCH {}",
                    serde_json::json!({
                        "depth": depth, "files": paths.len(), "round": round,
                        "warmup": round < 3, "reuse": reuse,
                        "elapsed_ms": start.elapsed().as_secs_f64() * 1000.0,
                        "scope": "paired Core resolution only, warm cache, no guest/VM/journal",
                    })
                );
            }
        }
    }
}
