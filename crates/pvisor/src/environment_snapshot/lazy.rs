//! Read-only, authenticated snapshot RAM. Cached FUSE reads are driven by host
//! page faults; MAP_PRIVATE guest mappings own all subsequent writes.
use super::{EnvironmentManifest, SnapshotStore, store::valid_id};
use crate::ram_backing::BLOCK_BYTES;
use anyhow::ensure;
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr,
    ReplyData, ReplyEntry, ReplyOpen, Request,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant, UNIX_EPOCH},
};

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Raw snapshot v3 authenticates each 64 KiB block before supplying any bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRamIndex {
    pub length: u64,
    pub sha256: Vec<String>,
}
impl RawRamIndex {
    pub(super) fn capture(file: &File) -> anyhow::Result<Self> {
        let mut index = Self {
            length: file.metadata()?.len(),
            sha256: Vec::new(),
        };
        let mut offset = 0;
        while offset < index.length {
            let mut bytes = vec![0; (index.length - offset).min(BLOCK_BYTES as u64) as usize];
            file.read_exact_at(&mut bytes, offset)?;
            index.sha256.push(hash(&bytes));
            offset += bytes.len() as u64;
        }
        index.validate()?;
        Ok(index)
    }
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.length > 0 && self.sha256.len() as u64 == self.length.div_ceil(BLOCK_BYTES as u64),
            "invalid raw RAM block inventory"
        );
        for id in &self.sha256 {
            valid_id(id)?;
        }
        Ok(())
    }
}

enum Backing {
    Raw {
        file: File,
        index: Option<RawRamIndex>,
    },
    Compressed(std::sync::Arc<super::blocks::PinnedRamBlocks>),
}

/// Owns the backing, including hard links retaining compressed blocks after
/// snapshot deletion. Construction reads metadata only, never RAM payloads.
pub struct SnapshotRamReader {
    backing: Backing,
    length: u64,
    // Small bounded cache avoids repeatedly decoding a block for 4 KiB faults.
    // The kernel page cache is the primary decoded cache.
    cache: lru::LruCache<usize, Vec<u8>>,
}
impl SnapshotRamReader {
    pub(super) fn new(path: &Path, manifest: &EnvironmentManifest) -> anyhow::Result<Self> {
        let (backing, length) = if let Some(blocks) = &manifest.ram_blocks {
            blocks.validate()?;
            // Pin on the store's filesystem (hard links cannot cross devices).
            // The existing writer lease lets gc reap crashed readers while
            // preserving live ones after the published snapshot is deleted.
            let store = path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| anyhow::anyhow!("missing snapshot store"))?;
            let references = SnapshotStore::new(store)?.begin()?;
            fs::create_dir(references.directory().join("ram-blocks"))?;
            let mut seen = HashSet::new();
            for block in &blocks.blocks {
                if seen.insert(&block.id) {
                    let pinned = references.directory().join("ram-blocks").join(&block.id);
                    fs::hard_link(path.join("ram-blocks").join(&block.id), &pinned)?;
                    let meta = fs::symlink_metadata(&pinned)?;
                    ensure!(
                        meta.is_file() && (46..=(45 + BLOCK_BYTES) as u64).contains(&meta.len()),
                        "invalid RAM content object"
                    );
                }
            }
            (
                Backing::Compressed(std::sync::Arc::new(super::blocks::PinnedRamBlocks {
                    blocks: blocks.clone(),
                    references,
                    sha256: manifest.ram_sha256.clone(),
                })),
                blocks.length,
            )
        } else {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path.join("ram.bin"))?;
            let meta = file.metadata()?;
            ensure!(meta.is_file() && meta.len() > 0, "invalid raw RAM backing");
            if let Some(index) = &manifest.ram_index {
                index.validate()?;
                ensure!(index.length == meta.len(), "RAM file size mismatch");
            }
            (
                Backing::Raw {
                    file,
                    index: manifest.ram_index.clone(),
                },
                meta.len(),
            )
        };
        Ok(Self {
            backing,
            length,
            cache: lru::LruCache::new(std::num::NonZeroUsize::new(4).unwrap()),
        })
    }
    pub(crate) fn compressed_base(&self) -> Option<std::sync::Arc<super::blocks::PinnedRamBlocks>> {
        match &self.backing {
            Backing::Compressed(base) => Some(base.clone()),
            Backing::Raw { .. } => None,
        }
    }
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Reads across block boundaries and short tails, checking complete blocks
    /// even for partial requests. A corrupt block never supplies unchecked bytes.
    pub fn read_at(&mut self, offset: u64, output: &mut [u8]) -> anyhow::Result<usize> {
        let length = self.length.saturating_sub(offset).min(output.len() as u64) as usize;
        let mut copied = 0;
        while copied < length {
            let position = offset + copied as u64;
            let index = (position / BLOCK_BYTES as u64) as usize;
            let start = (position % BLOCK_BYTES as u64) as usize;
            if !self.cache.contains(&index) {
                let bytes = match &self.backing {
                    Backing::Compressed(base) => base
                        .blocks
                        .read_block(&base.references.directory().join("ram-blocks"), index)?,
                    Backing::Raw { file, index: seal } => {
                        let block_offset = index as u64 * BLOCK_BYTES as u64;
                        let mut bytes =
                            vec![0; (self.length - block_offset).min(BLOCK_BYTES as u64) as usize];
                        file.read_exact_at(&mut bytes, block_offset)?;
                        if let Some(seal) = seal {
                            ensure!(
                                hash(&bytes) == seal.sha256[index],
                                "raw RAM block digest mismatch"
                            );
                        }
                        bytes
                    }
                };
                self.cache.put(index, bytes);
            }
            let bytes = self.cache.get(&index).unwrap();
            let count = (length - copied).min(bytes.len() - start);
            output[copied..copied + count].copy_from_slice(&bytes[start..start + count]);
            copied += count;
        }
        Ok(copied)
    }
}

/// Must outlive the VM and its RAM mappings. Dropping it unmounts the pager and
/// releases content pins; it must never be dropped at the guest-ready callback.
pub struct SnapshotRamMount {
    session: Option<BackgroundSession>,
    watchdog: Option<(Child, ChildStdin)>,
    directory: tempfile::TempDir,
}
impl SnapshotRamMount {
    pub(crate) fn ram_path(&self) -> std::path::PathBuf {
        self.directory.path().join("ram")
    }

    /// The ordinary executor owns its pager in the supervisor, whose binary
    /// enters internal modes before parsing the worker/CLI arguments.
    pub(crate) fn watch_native_owner_exit(&mut self, executable: &Path) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.watchdog.is_some() {
            return Err(io::Error::other("RAM exit watchdog already installed"));
        }
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env("PVISOR_VM_RESTORE_RAM_WATCHDOG", self.directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM watchdog pipe"))?;
        self.watchdog = Some((child, pipe));
        Ok(())
    }

    pub fn new(reader: SnapshotRamReader, directory: &Path) -> io::Result<(Self, File)> {
        let temporary = tempfile::Builder::new()
            .prefix("ram-mount-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(directory)?;
        let options = [
            MountOption::FSName("pvisor-snapshot-ram".into()),
            MountOption::RO,
            MountOption::DefaultPermissions,
            MountOption::NoExec,
            MountOption::NoSuid,
            MountOption::NoDev,
            #[cfg(target_os = "macos")]
            MountOption::CUSTOM("backend=kernel".into()),
        ];
        #[cfg(target_os = "linux")]
        let session = BackgroundSession::new_interruptible(fuser::Session::new(
            RamFs { reader },
            temporary.path(),
            &options,
        )?)?;
        #[cfg(not(target_os = "linux"))]
        let session = fuser::spawn_mount2(RamFs { reader }, temporary.path(), &options)?;
        let mount = Self {
            session: Some(session),
            watchdog: None,
            directory: temporary,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let file = loop {
            match File::open(mount.directory.path().join("ram")) {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if mount.session.as_ref().unwrap().guard.is_finished() {
                        return Err(io::Error::other(
                            "snapshot RAM FUSE session exited before mount became ready",
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "snapshot RAM mount did not become ready",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        };
        Ok((mount, file))
    }

    /// A VMM can terminate with _exit or SIGKILL, bypassing Rust destructors.
    /// A separate process group watches this pipe and unmounts after EOF. It
    /// inherits no RAM descriptors and does not require FUSE allow_other.
    pub fn watch_runner_exit(&mut self, executable: &Path) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.watchdog.is_some() {
            return Err(io::Error::other("RAM exit watchdog already installed"));
        }
        let mut child = Command::new(executable)
            .args(["snapshot", "ram-watchdog"])
            .arg(self.directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM watchdog pipe"))?;
        self.watchdog = Some((child, pipe));
        Ok(())
    }
}
impl Drop for SnapshotRamMount {
    fn drop(&mut self) {
        if let Some((mut child, pipe)) = self.watchdog.take() {
            drop(pipe);
            if let Err(error) = child.wait() {
                tracing::warn!(%error, "cannot wait for snapshot RAM cleanup");
            }
        }
        if let Some(session) = self.session.take()
            && let Err(error) = session.unmount()
        {
            tracing::warn!(%error, "cannot unmount snapshot RAM");
        }
    }
}

/// Internal CLI watchdog entry point. EOF means the sole owning runner has
/// exited or explicitly released its mount after all RAM users were dropped.
pub(crate) fn watch_mount(path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ram-mount-")),
        "invalid RAM watchdog mountpoint"
    );
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    #[cfg(target_os = "linux")]
    {
        let native = super::native_path(path)?;
        // Privileged deployments may mount directly without fusermount.
        let detached = unsafe { libc::umount2(native.as_ptr(), libc::MNT_DETACH) } == 0;
        let error = io::Error::last_os_error();
        if !detached && !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
            let mut result = None;
            for helper in ["fusermount3", "fusermount"] {
                match Command::new(helper)
                    .args(["-u", "-z", "--"])
                    .arg(path)
                    .output()
                {
                    Ok(output) => {
                        result = Some(output);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            let output =
                result.ok_or_else(|| anyhow::anyhow!("FUSE unmount helper unavailable"))?;
            ensure!(
                output.status.success(),
                "snapshot RAM unmount failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/sbin/umount").arg(path).output()?;
        ensure!(
            output.status.success(),
            "snapshot RAM unmount failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

const ROOT: u64 = 1;
const RAM: u64 = 2;
struct RamFs {
    reader: SnapshotRamReader,
}
impl RamFs {
    fn attr(&self, ino: u64) -> FileAttr {
        FileAttr {
            ino,
            size: if ino == RAM { self.reader.len() } else { 0 },
            blocks: if ino == RAM {
                self.reader.len().div_ceil(512)
            } else {
                0
            },
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if ino == RAM {
                FileType::RegularFile
            } else {
                FileType::Directory
            },
            perm: if ino == RAM { 0o400 } else { 0o500 },
            nlink: if ino == RAM { 1 } else { 2 },
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}
impl Filesystem for RamFs {
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), i32> {
        // Fetch bounded blocks near the fault, rather than the entire snapshot.
        let _ = config.set_max_readahead(BLOCK_BYTES as u32);
        Ok(())
    }
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == ROOT && name == OsStr::new("ram") {
            reply.entry(&Duration::from_secs(3600), &self.attr(RAM), 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if [ROOT, RAM].contains(&ino) {
            reply.attr(&Duration::from_secs(3600), &self.attr(ino));
        } else {
            reply.error(libc::ENOENT);
        }
    }
    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if ino != RAM {
            reply.error(libc::ENOENT);
        } else if flags & libc::O_ACCMODE != libc::O_RDONLY || flags & libc::O_TRUNC != 0 {
            reply.error(libc::EROFS);
        } else {
            reply.opened(0, 0);
        } // Cached I/O is required for mmap.
    }
    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if ino != RAM || offset < 0 || size > 1024 * 1024 {
            reply.error(libc::EINVAL);
            return;
        }
        let mut bytes = vec![0; size as usize];
        match self.reader.read_at(offset as u64, &mut bytes) {
            Ok(count) => reply.data(&bytes[..count]),
            Err(error) => {
                tracing::error!(%error, offset, "snapshot RAM fault read failed");
                // A failed mmap fault must stop the VM, never supply zero-filled
                // or unchecked RAM. The kernel propagates EIO as a mapping fault.
                reply.error(libc::EIO);
            }
        }
    }
}
