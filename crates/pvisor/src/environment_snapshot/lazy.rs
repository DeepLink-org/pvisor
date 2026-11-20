//! Read-only, authenticated snapshot RAM. Cached FUSE reads are driven by host
//! page faults; MAP_PRIVATE guest mappings own all subsequent writes.
#[path = "ram_runtime.rs"]
mod ram_runtime;
#[cfg(target_os = "linux")]
#[path = "ram_unmount.rs"]
mod ram_unmount;
use super::{EnvironmentManifest, RamBlocks, SnapshotStore, store::valid_id};
use crate::ram_backing::BLOCK_BYTES;
#[cfg(target_os = "macos")]
use anyhow::Context;
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
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
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
    // One pass authenticates the whole sealed file and builds its block index.
    // The caller must keep this inode private and stable until publication.
    pub(super) fn capture(file: &File) -> anyhow::Result<(Self, String)> {
        let mut index = Self {
            length: file.metadata()?.len(),
            sha256: Vec::new(),
        };
        let mut whole = Sha256::new();
        let mut offset = 0;
        while offset < index.length {
            let mut bytes = vec![0; (index.length - offset).min(BLOCK_BYTES as u64) as usize];
            file.read_exact_at(&mut bytes, offset)?;
            whole.update(&bytes);
            index.sha256.push(hash(&bytes));
            offset += bytes.len() as u64;
        }
        ensure!(
            file.metadata()?.len() == index.length,
            "RAM size changed while indexing"
        );
        index.validate()?;
        Ok((index, crate::util::encode_hex(&whole.finalize())))
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
    store_root: std::path::PathBuf,
    length: u64,
    // Runtime-only compact view: entries are original authenticated block IDs.
    // No zero block has an address in this FUSE inode.
    compact: Option<Vec<usize>>,
    #[cfg(target_os = "linux")]
    direct: Option<File>,
    // Small bounded cache avoids repeatedly decoding a block for 4 KiB faults.
    // The kernel page cache is the primary decoded cache.
    cache: lru::LruCache<usize, Vec<u8>>,
}
impl SnapshotRamReader {
    pub(super) fn new(path: &Path, manifest: &EnvironmentManifest) -> anyhow::Result<Self> {
        Self::from_parts(
            path,
            manifest.ram_blocks.as_ref(),
            manifest.ram_index.as_ref(),
            &manifest.ram_sha256,
        )
    }
    fn from_parts(
        path: &Path,
        blocks: Option<&RamBlocks>,
        index: Option<&RawRamIndex>,
        ram_sha256: &str,
    ) -> anyhow::Result<Self> {
        let (backing, length) = if let Some(blocks) = blocks {
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
                    sha256: ram_sha256.to_owned(),
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
            if let Some(index) = index {
                index.validate()?;
                ensure!(index.length == meta.len(), "RAM file size mismatch");
            }
            (
                Backing::Raw {
                    file,
                    index: index.cloned(),
                },
                meta.len(),
            )
        };
        Ok(Self {
            backing,
            store_root: path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| anyhow::anyhow!("missing snapshot store"))?
                .canonicalize()?,
            length,
            compact: None,
            #[cfg(target_os = "linux")]
            direct: None,
            cache: lru::LruCache::new(std::num::NonZeroUsize::new(4).unwrap()),
        })
    }
    /// Select a runtime-only nonzero view after the publication/index has been
    /// authenticated by the host. Capture and durable file formats are unchanged.
    pub(crate) fn compact_zero_restore(
        &mut self,
    ) -> anyhow::Result<Vec<pvisor_vm::api::RamRestoreExtent>> {
        #[cfg(target_os = "linux")]
        {
            use pvisor_vm::api::RamRestoreExtent;
            use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
            ensure!(self.compact.is_none(), "RAM view already selected");
            let Backing::Raw {
                file,
                index: Some(index),
            } = &self.backing
            else {
                return Ok(Vec::new());
            };
            // Restrict this first implementation to full authenticated blocks.
            // A short tail retains the original view, never a guessed zero range.
            if !self.length.is_multiple_of(BLOCK_BYTES as u64) {
                return Ok(Vec::new());
            }
            let zero = hash(&vec![0; BLOCK_BYTES]);
            let mut extents: Vec<RamRestoreExtent> = Vec::new();
            let mut blocks = Vec::new();
            for (i, digest) in index.sha256.iter().enumerate() {
                let backing_offset = if digest == &zero {
                    None
                } else {
                    let offset = blocks.len() as u64 * BLOCK_BYTES as u64;
                    blocks.push(i);
                    Some(offset)
                };
                if let Some(last) =
                    extents
                        .last_mut()
                        .filter(|last| match (last.backing_offset, backing_offset) {
                            (None, None) => true,
                            (Some(a), Some(b)) => a + last.length == b,
                            _ => false,
                        })
                {
                    last.length += BLOCK_BYTES as u64;
                } else {
                    extents.push(RamRestoreExtent {
                        offset: i as u64 * BLOCK_BYTES as u64,
                        length: BLOCK_BYTES as u64,
                        backing_offset,
                    });
                }
            }
            if blocks.len() == index.sha256.len() || extents.len() > 8192 {
                return Ok(Vec::new());
            }
            // Independent open description: never change the source/capture FD.
            // Direct I/O prevents lower-file readahead from filling zero ranges
            // adjacent to a requested nonzero block. Unsupported storage fails
            // explicitly; it must not silently defeat the selected zero contract.
            let direct = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECT)
                .open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
            let a = file.metadata()?;
            let b = direct.metadata()?;
            ensure!(
                a.dev() == b.dev() && a.ino() == b.ino() && a.len() == b.len(),
                "RAM direct reader identity mismatch"
            );
            self.direct = Some(direct);
            self.compact = Some(blocks);
            Ok(extents)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Vec::new())
        }
    }

    /// Retain the exact authenticated raw input inode, never the FUSE mapping.
    #[cfg(target_os = "linux")]
    pub(crate) fn raw_preparation_cache(&self) -> io::Result<Option<File>> {
        match &self.backing {
            Backing::Raw {
                file,
                index: Some(_),
            } => file.try_clone().map(Some),
            _ => Ok(None),
        }
    }

    #[cfg(target_os = "linux")]
    fn raw_inode(&self) -> io::Result<Option<ram_unmount::Inode>> {
        match &self.backing {
            Backing::Raw { file, .. } => ram_unmount::Inode::file(file).map(Some),
            Backing::Compressed(_) => Ok(None),
        }
    }

    pub(crate) fn compressed_base(&self) -> Option<std::sync::Arc<super::blocks::PinnedRamBlocks>> {
        match &self.backing {
            Backing::Compressed(base) => Some(base.clone()),
            Backing::Raw { .. } => None,
        }
    }
    pub fn len(&self) -> u64 {
        self.compact.as_ref().map_or(self.length, |blocks| {
            blocks.len() as u64 * BLOCK_BYTES as u64
        })
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads across block boundaries and short tails, checking complete blocks
    /// even for partial requests. A corrupt block never supplies unchecked bytes.
    pub fn read_at(&mut self, offset: u64, output: &mut [u8]) -> anyhow::Result<usize> {
        let length = self.len().saturating_sub(offset).min(output.len() as u64) as usize;
        let mut copied = 0;
        while copied < length {
            let position = offset + copied as u64;
            let view_index = (position / BLOCK_BYTES as u64) as usize;
            let index = self
                .compact
                .as_ref()
                .map_or(view_index, |blocks| blocks[view_index]);
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
                        #[cfg(target_os = "linux")]
                        if let Some(direct) = &self.direct {
                            read_direct_block(direct, &mut bytes, block_offset)?;
                        } else {
                            file.read_exact_at(&mut bytes, block_offset)?;
                        }
                        #[cfg(not(target_os = "linux"))]
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

// One aligned, bounded scratch buffer. It never becomes guest RAM or a
// persistent second baseline. No buffered fallback after a direct-I/O failure.
#[cfg(target_os = "linux")]
fn read_direct_block(file: &File, output: &mut [u8], offset: u64) -> io::Result<()> {
    let length = output.len();
    if length != BLOCK_BYTES || !offset.is_multiple_of(BLOCK_BYTES as u64) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unaligned direct RAM block",
        ));
    }
    // SAFETY: a fresh anonymous mapping owns exactly length writable bytes.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the mapping is live and exclusively borrowed until munmap below.
    let buffer = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), length) };
    let result = file.read_exact_at(buffer, offset);
    if result.is_ok() {
        output.copy_from_slice(buffer);
    }
    // SAFETY: exact mapping allocated above, with no remaining borrowers.
    unsafe {
        libc::munmap(ptr, length);
    }
    result
}

/// Must outlive the VM and its RAM mappings. Drop attempts ordinary Linux
/// retirement and retains the mountpoint on failure for reconciliation.
pub struct SnapshotRamMount {
    session: Option<BackgroundSession>,
    server: Option<(Child, ChildStdin)>,
    watchdog: Option<(Child, ChildStdin)>,
    directory: PathBuf,
    retirement_started: bool,
    #[cfg(target_os = "linux")]
    directory_inode: ram_unmount::Inode,
    #[cfg(target_os = "linux")]
    identity: Option<ram_unmount::Identity>,
}
impl SnapshotRamMount {
    pub(crate) fn ram_path(&self) -> PathBuf {
        self.directory.join("ram")
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn runner_lease(&self) -> Option<&ChildStdin> {
        self.server.as_ref().map(|(_, pipe)| pipe)
    }

    /// Mount in a private host runtime directory, never in durable state.
    /// `directory` is retained for API compatibility and treated as an excluded
    /// environment/state root, not a mount parent; it need not exist.
    pub fn new(reader: SnapshotRamReader, directory: &Path) -> io::Result<(Self, File)> {
        Self::runtime(reader, &[directory])
    }

    pub(crate) fn runtime(
        reader: SnapshotRamReader,
        protected: &[&Path],
    ) -> io::Result<(Self, File)> {
        let mut roots = protected.to_vec();
        roots.push(&reader.store_root);
        let temporary = ram_runtime::temporary(&roots)?;
        Self::mount_at(reader, temporary)
    }

    fn mount_at(
        reader: SnapshotRamReader,
        temporary: tempfile::TempDir,
    ) -> io::Result<(Self, File)> {
        let mut mount = Self::new_owner(temporary)?;
        mount.start(reader)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let file = loop {
            match File::open(mount.ram_path()) {
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
        #[cfg(target_os = "linux")]
        mount.identity.as_ref().unwrap().verify_file(&file)?;
        Ok((mount, file))
    }

    fn new_owner(temporary: tempfile::TempDir) -> io::Result<Self> {
        Ok(Self {
            session: None,
            server: None,
            watchdog: None,
            retirement_started: false,
            #[cfg(target_os = "linux")]
            directory_inode: ram_unmount::Inode::directory(temporary.path())?,
            #[cfg(target_os = "linux")]
            identity: None,
            directory: temporary.keep(),
        })
    }

    fn start(&mut self, reader: SnapshotRamReader) -> io::Result<()> {
        let options = [
            #[cfg(target_os = "linux")]
            MountOption::FSName(ram_unmount::source(&self.directory)),
            #[cfg(target_os = "macos")]
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
        {
            let session = fuser::Session::new_with_external_unmount(
                RamFs { reader },
                &self.directory,
                &options,
            );
            // A helper can fail after creating the mount. Capture the uniquely
            // named entry before propagating that error so Drop can retire it.
            self.identity = ram_unmount::Identity::capture(&self.directory).ok();
            let session = session?;
            if self.identity.is_none() {
                return Err(io::Error::other(
                    "cannot identify newly created snapshot RAM mount",
                ));
            }
            self.session = Some(BackgroundSession::new_interruptible(session)?);
        }
        #[cfg(target_os = "macos")]
        {
            self.session = Some(fuser::spawn_mount2(
                RamFs { reader },
                &self.directory,
                &options,
            )?);
        }
        Ok(())
    }

    /// Serve faults outside the VMM process. Its kernel teardown must never
    /// wait on a FUSE server that was killed along with its vCPUs.
    /// The public ram_mount directory argument is an excluded state root, not
    /// a staging or mount parent. Both spec and mount use private runtime storage.
    pub(super) fn external(
        object: &Path,
        manifest: &EnvironmentManifest,
        directory: &Path,
        executable: &Path,
        restore_reader: Option<&SnapshotRamReader>,
    ) -> io::Result<(Self, File)> {
        use std::os::unix::{io::AsRawFd, process::CommandExt};
        let object = fs::canonicalize(object)?;
        let store = object
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("missing snapshot store"))?;
        let temporary = ram_runtime::temporary(&[store, directory])?;
        // Keep the spec outside the mountpoint: mounting hides its contents,
        // and cleanup must only remove an empty mount directory. The staging
        // owner is released on readiness or on any startup failure.
        let staging = ram_runtime::temporary(&[store, directory])?;
        let mut spec = tempfile::NamedTempFile::new_in(staging.path())?;
        let mut mount = Self::new_owner(temporary)?;
        serde_json::to_writer(
            spec.as_file_mut(),
            &RamServerSpec {
                object: object.as_os_str().as_bytes().to_vec(),
                mount: mount.directory.as_os_str().as_bytes().to_vec(),
                blocks: manifest.ram_blocks.clone(),
                index: manifest.ram_index.clone(),
                ram_sha256: manifest.ram_sha256.clone(),
                compact: restore_reader.is_some_and(|reader| reader.compact.is_some()),
                #[cfg(target_os = "linux")]
                raw_inode: restore_reader
                    .map(SnapshotRamReader::raw_inode)
                    .transpose()?
                    .flatten(),
            },
        )?;
        spec.as_file_mut().flush()?;
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env(crate::env::runner::RESTORE_RAM_SERVER_ENV, spec.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing RAM server pipe"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing RAM readiness pipe"))?;
        mount.server = Some((child, pipe));
        // The pipe is private and has one writer. Bound readiness without a
        // detached reader thread or an unbounded wait on a failed helper.
        let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = Vec::new();
        loop {
            let mut bytes = [0; 16];
            match output.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::other(
                        "snapshot RAM server exited before readiness",
                    ));
                }
                Ok(count) => {
                    ready.extend_from_slice(&bytes[..count]);
                    if ready.ends_with(b"\n") {
                        break;
                    }
                    if ready.len() >= 512 {
                        return Err(io::Error::other("invalid RAM server readiness"));
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "snapshot RAM server readiness timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(target_os = "linux")]
        {
            let identity = serde_json::from_slice::<ram_unmount::Identity>(&ready)?;
            if ram_unmount::Identity::capture(&mount.directory)? != identity {
                return Err(io::Error::other(
                    "RAM server mount identity changed during readiness",
                ));
            }
            mount.identity = Some(identity);
        }
        #[cfg(target_os = "macos")]
        if ready != b"ready\n" {
            return Err(io::Error::other("invalid RAM server readiness"));
        }
        // The caller's published-object lease remains held until the helper
        // has opened raw backing or acquired independent compressed pins.
        let file = File::open(mount.ram_path())?;
        #[cfg(target_os = "linux")]
        mount.identity.as_ref().unwrap().verify_file(&file)?;
        Ok((mount, file))
    }

    /// A VMM can terminate with _exit or SIGKILL, bypassing Rust destructors.
    /// A separate process group watches this pipe and unmounts after EOF. It
    /// inherits no RAM descriptors and does not require FUSE allow_other.
    pub fn watch_runner_exit(&mut self, executable: &Path) -> io::Result<()> {
        use std::os::unix::process::CommandExt;
        if self.watchdog.is_some() || self.server.is_some() {
            return Err(io::Error::other("RAM cleanup owner already installed"));
        }
        let mut child = Command::new(executable)
            .env_clear()
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
            )
            .env(
                crate::env::runner::RESTORE_RAM_WATCHDOG_ENV,
                &self.directory,
            )
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
        #[cfg(target_os = "linux")]
        {
            let (_, pipe) = self.watchdog.as_mut().unwrap();
            serde_json::to_writer(
                &mut *pipe,
                &WatchIdentity {
                    directory: self.directory_inode,
                    mount: self
                        .identity
                        .clone()
                        .ok_or_else(|| io::Error::other("missing RAM mount ID"))?,
                },
            )?;
            pipe.write_all(b"\n")?;
        }
        Ok(())
    }
}
impl SnapshotRamMount {
    fn shutdown(&mut self) -> io::Result<()> {
        self.retirement_started = true;
        if let Some((mut child, pipe)) = self.server.take().or_else(|| self.watchdog.take()) {
            drop(pipe);
            // A still-live helper owns retirement. Do not kill its request
            // loop or race it with another unmount after a timeout.
            let status = match poll_exit(&mut child, Duration::from_secs(7)) {
                Ok(status) => status,
                Err(error) => {
                    let reaper = std::thread::Builder::new()
                        .name("pvisor-ram-reap".into())
                        .spawn(move || child.wait());
                    if let Err(error) = reaper {
                        tracing::warn!(%error, "cannot schedule RAM cleanup helper reap");
                    }
                    return Err(error);
                }
            };
            if !status.success() {
                tracing::warn!(%status, path = %self.directory.display(), "snapshot RAM cleanup helper exited unsuccessfully");
            }
        }
        #[cfg(target_os = "linux")]
        ram_unmount::retire(
            &self.directory,
            self.directory_inode,
            self.identity.as_ref(),
        )?;
        #[cfg(target_os = "macos")]
        detach_mount(&self.directory).map_err(io::Error::other)?;
        if let Some(session) = self.session.take() {
            session.unmount()?;
        }
        Ok(())
    }
}

impl Drop for SnapshotRamMount {
    fn drop(&mut self) {
        if !self.retirement_started
            && let Err(error) = self.shutdown()
        {
            #[cfg(target_os = "macos")]
            tracing::warn!(%error, path = %self.directory.display(), "snapshot RAM retirement incomplete");
            #[cfg(target_os = "linux")]
            tracing::warn!(%error, identity = ?self.identity, path = %self.directory.display(), "snapshot RAM retirement incomplete");
        }
    }
}

fn poll_exit(child: &mut Child, timeout: Duration) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "RAM cleanup helper timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_exit(child: &mut Child, timeout: Duration) -> io::Result<std::process::ExitStatus> {
    match poll_exit(child, timeout) {
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
        result => result,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RamServerSpec {
    object: Vec<u8>,
    mount: Vec<u8>,
    blocks: Option<RamBlocks>,
    index: Option<RawRamIndex>,
    ram_sha256: String,
    #[serde(default)]
    compact: bool,
    #[cfg(target_os = "linux")]
    #[serde(default)]
    raw_inode: Option<ram_unmount::Inode>,
}

#[cfg(target_os = "linux")]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchIdentity {
    directory: ram_unmount::Inode,
    mount: ram_unmount::Identity,
}

/// Private snapshot pager entry point; keep answering faults until its sole
/// owning runner closes the pipe, including during kernel process teardown.
pub(crate) fn serve_ram(spec: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    ram_runtime::private(
        spec.parent()
            .ok_or_else(|| anyhow::anyhow!("missing RAM spec parent"))?,
        &[],
    )?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(spec)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.len() <= 128 * 1024 * 1024,
        "invalid private RAM server specification"
    );
    let spec: RamServerSpec = serde_json::from_reader(file.take(128 * 1024 * 1024 + 1))?;
    let object = Path::new(OsStr::from_bytes(&spec.object));
    let mount = Path::new(OsStr::from_bytes(&spec.mount));
    ensure!(
        object.is_absolute()
            && mount.is_absolute()
            && mount
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ram-mount-")),
        "RAM server requires absolute object and private ram-mount paths"
    );
    let metadata = fs::symlink_metadata(mount)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "invalid private RAM server mountpoint"
    );
    let mut reader = SnapshotRamReader::from_parts(
        object,
        spec.blocks.as_ref(),
        spec.index.as_ref(),
        &spec.ram_sha256,
    )?;
    #[cfg(target_os = "linux")]
    if let Some(expected) = spec.raw_inode {
        ensure!(
            reader.raw_inode()? == Some(expected),
            "snapshot Raw RAM inode changed before pager launch"
        );
    }
    if spec.compact {
        ensure!(
            !reader.compact_zero_restore()?.is_empty(),
            "invalid compact RAM pager view"
        );
    }
    ram_runtime::private(mount, &[&reader.store_root])?;
    let mut owner = SnapshotRamMount {
        session: None,
        server: None,
        watchdog: None,
        directory: mount.to_path_buf(),
        retirement_started: false,
        #[cfg(target_os = "linux")]
        directory_inode: ram_unmount::Inode::directory(mount)?,
        #[cfg(target_os = "linux")]
        identity: None,
    };
    owner.start(reader)?;
    let result = (|| -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !owner.ram_path().try_exists()? {
            ensure!(
                !owner.session.as_ref().unwrap().guard.is_finished(),
                "snapshot RAM server stopped before mount readiness"
            );
            ensure!(
                Instant::now() < deadline,
                "snapshot RAM mount readiness timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut output = io::stdout().lock();
        #[cfg(target_os = "linux")]
        {
            serde_json::to_writer(&mut output, owner.identity.as_ref().unwrap())?;
            output.write_all(b"\n")?;
        }
        #[cfg(target_os = "macos")]
        output.write_all(b"ready\n")?;
        output.flush()?;
        drop(output);
        #[cfg(target_os = "linux")]
        wait_for_ram_consumers(owner.session.as_ref().unwrap())?;
        #[cfg(target_os = "macos")]
        io::copy(&mut io::stdin().lock(), &mut io::sink())?;
        Ok(())
    })();
    // The supervisor and each Linux native consumer hold a writer. The
    // independent server continues answering requests during ordinary unmount.
    let retired = owner.shutdown().map_err(|error| {
        #[cfg(target_os = "linux")]
        let identity = format!(" ({:?})", owner.identity);
        #[cfg(target_os = "macos")]
        let identity = "";
        anyhow::anyhow!("retire snapshot RAM {}{identity}: {error}", mount.display())
    });
    result?;
    retired?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn wait_for_ram_consumers(session: &BackgroundSession) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut input = libc::pollfd {
        fd: io::stdin().as_raw_fd(),
        events: 0,
        revents: 0,
    };
    loop {
        if session.guard.is_finished() {
            return Err(io::Error::other("snapshot RAM request loop exited"));
        }
        let polled = unsafe { libc::poll(&mut input, 1, 100) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if input.revents & libc::POLLHUP != 0 {
            return Ok(());
        } else if input.revents != 0 {
            return Err(io::Error::other("invalid snapshot RAM consumer pipe"));
        }
    }
}

/// Internal CLI watchdog entry point. EOF means the owner has exited or
/// explicitly released the mount after dropping every known consumer.
pub(crate) fn watch_mount(path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("ram-mount-")),
        "invalid RAM watchdog mountpoint"
    );
    let mut input = io::stdin().lock();
    #[cfg(target_os = "linux")]
    let identity: WatchIdentity = {
        use std::io::BufRead;
        let mut bytes = Vec::new();
        (&mut input).take(512).read_until(b'\n', &mut bytes)?;
        ensure!(bytes.ends_with(b"\n"), "missing RAM watchdog identity");
        serde_json::from_slice(&bytes)?
    };
    io::copy(&mut input, &mut io::sink())?;
    #[cfg(target_os = "linux")]
    ram_unmount::retire(path, identity.directory, Some(&identity.mount)).map_err(|error| {
        anyhow::anyhow!(
            "retire snapshot RAM {} ({:?}): {error}",
            path.display(),
            identity.mount
        )
    })?;
    #[cfg(target_os = "macos")]
    detach_mount(path)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn detach_mount(path: &Path) -> anyhow::Result<()> {
    let native = super::native_path(path)?;
    // SAFETY: native is a live NUL-terminated path; the OS checks ownership.
    let detached = unsafe { libc::unmount(native.as_ptr(), libc::MNT_FORCE) } == 0;
    let error = io::Error::last_os_error();
    if !detached && !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
        return Err(error).context("snapshot RAM detach failed");
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

#[cfg(test)]
mod raw_index_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn raw_advice_handle_keeps_index_verification_on_future_reads() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let object = dir.path().join("objects").join("test");
        fs::create_dir_all(&object).unwrap();
        let path = object.join("ram.bin");
        fs::write(&path, vec![3u8; BLOCK_BYTES * 2]).unwrap();
        let (index, digest) = RawRamIndex::capture(&File::open(&path).unwrap()).unwrap();
        let mut reader =
            SnapshotRamReader::from_parts(&object, None, Some(&index), &digest).unwrap();
        let source = reader.raw_preparation_cache().unwrap().unwrap();
        assert_eq!(
            unsafe { libc::posix_fadvise(source.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) },
            0
        );
        let mut byte = [0];
        reader.read_at(0, &mut byte).unwrap();
        assert_eq!(byte, [3]);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .write_all_at(&[9], BLOCK_BYTES as u64)
            .unwrap();
        assert!(reader.read_at(BLOCK_BYTES as u64, &mut byte).is_err());
        let unindexed = SnapshotRamReader::from_parts(&object, None, None, &digest).unwrap();
        assert!(unindexed.raw_preparation_cache().unwrap().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn compact_zero_view_reads_only_authenticated_nonzero_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let object = dir.path().join("objects/test");
        fs::create_dir_all(&object).unwrap();
        let path = object.join("ram.bin");
        let mut bytes = vec![0; BLOCK_BYTES * 5];
        bytes[BLOCK_BYTES..BLOCK_BYTES * 2].fill(3);
        bytes[BLOCK_BYTES * 4..].fill(7);
        fs::write(&path, &bytes).unwrap();
        let (index, digest) = RawRamIndex::capture(&File::open(&path).unwrap()).unwrap();
        let mut reader =
            SnapshotRamReader::from_parts(&object, None, Some(&index), &digest).unwrap();
        let extents = reader.compact_zero_restore().unwrap();
        assert_eq!(extents.len(), 4);
        assert_eq!(reader.len(), (BLOCK_BYTES * 2) as u64);
        assert_eq!(reader.compact.as_ref().unwrap(), &[1, 4]);
        let mut output = vec![0; BLOCK_BYTES + 2];
        reader
            .read_at((BLOCK_BYTES - 1) as u64, &mut output)
            .map(|n| assert_eq!(n, BLOCK_BYTES + 1))
            .unwrap();
        assert_eq!(output[0], 3);
        assert!(output[1..BLOCK_BYTES + 1].iter().all(|b| *b == 7));
        // Canonical zero authority comes from the authenticated index, not mutable
        // source bytes. Nonzero corruption is still rejected on the next read.
        let writable = OpenOptions::new().write(true).open(&path).unwrap();
        writable.write_all_at(&[9], 0).unwrap();
        writable
            .write_all_at(&[9], (BLOCK_BYTES * 4) as u64)
            .unwrap();
        writable.sync_all().unwrap();
        reader.cache.clear();
        assert!(reader.read_at(BLOCK_BYTES as u64, &mut [0]).is_err());
        assert!(reader.compact_zero_restore().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn compact_all_zero_and_unindexed_views_have_explicit_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let object = dir.path().join("objects/test");
        fs::create_dir_all(&object).unwrap();
        let path = object.join("ram.bin");
        fs::write(&path, vec![0; BLOCK_BYTES * 2]).unwrap();
        let (index, digest) = RawRamIndex::capture(&File::open(&path).unwrap()).unwrap();
        let mut reader =
            SnapshotRamReader::from_parts(&object, None, Some(&index), &digest).unwrap();
        let extents = reader.compact_zero_restore().unwrap();
        assert_eq!(extents.len(), 1);
        assert_eq!(extents[0].backing_offset, None);
        assert!(reader.is_empty());
        assert_eq!(reader.read_at(0, &mut [1]).unwrap(), 0);
        let mut legacy = SnapshotRamReader::from_parts(&object, None, None, &digest).unwrap();
        assert!(legacy.compact_zero_restore().unwrap().is_empty());
        assert_eq!(legacy.len(), (BLOCK_BYTES * 2) as u64);
    }

    #[test]
    fn fused_raw_digest_matches_logical_bytes_and_each_block() {
        for length in [
            1,
            BLOCK_BYTES - 1,
            BLOCK_BYTES,
            BLOCK_BYTES + 1,
            3 * BLOCK_BYTES + 17,
        ] {
            let file = tempfile::tempfile().unwrap();
            file.set_len(length as u64).unwrap();
            let mut expected = vec![0; length];
            // Exercise holes, block boundaries and a short final block.
            for offset in [0, length / 2, length - 1] {
                file.write_all_at(&[0xa5], offset as u64).unwrap();
                expected[offset] = 0xa5;
            }
            let (index, digest) = RawRamIndex::capture(&file).unwrap();
            assert_eq!(index.length, length as u64);
            assert_eq!(digest, hash(&expected));
            assert_eq!(
                index.sha256,
                expected.chunks(BLOCK_BYTES).map(hash).collect::<Vec<_>>()
            );
            file.write_all_at(&[0x42], (length - 1) as u64).unwrap();
            let (changed, digest_after) = RawRamIndex::capture(&file).unwrap();
            assert_ne!(digest, digest_after);
            assert_ne!(index.sha256.last(), changed.sha256.last());
        }
    }

    #[test]
    fn fused_raw_index_rejects_empty_or_unreadable_files() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(RawRamIndex::capture(file.as_file()).is_err());
        file.as_file().set_len(64).unwrap();
        let write_only = OpenOptions::new().write(true).open(file.path()).unwrap();
        assert!(RawRamIndex::capture(&write_only).is_err());
    }
}
