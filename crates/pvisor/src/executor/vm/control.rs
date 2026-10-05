//! Attempt-local VM controls and the live file backing owned by the supervisor.

use pvisor_core::operation::{OperationKind, VmMemory, VmState};
use pvisor_vm::api::{RamFileMapping, RamFileMount};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_FRAME: usize = 16 * 1024;

/// Same inode throughout the attempt: path selection never copies or remaps RAM.
pub(super) struct RamBacking {
    pub file: Arc<File>,
    storage: Arc<File>,
    pub path: PathBuf,
    _mount: Option<RamFileMount>,
    compression_store: Option<Arc<std::sync::Mutex<crate::ram_backing::CompressedRam>>>,
    layers: Option<PathBuf>,
    _temporary_layers: Option<tempfile::TempDir>,
    _temporary: Option<tempfile::NamedTempFile>,
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    restored: Option<Arc<super::checkpoint::PreparedRestore>>,
}

impl RamBacking {
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    pub(super) fn restored(restore: Arc<super::checkpoint::PreparedRestore>) -> Self {
        Self {
            file: restore.ram.clone(),
            storage: restore.ram.clone(),
            path: restore.ram_path.clone(),
            _mount: None,
            compression_store: None,
            layers: None,
            _temporary_layers: None,
            _temporary: None,
            restored: Some(restore),
        }
    }
    pub(super) fn create(path: Option<&Path>) -> anyhow::Result<Self> {
        if let Some(path) = path {
            let path = destination(path)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            let file = Arc::new(file);
            return Ok(Self {
                storage: file.clone(),
                file,
                path,
                _mount: None,
                compression_store: None,
                layers: None,
                _temporary_layers: None,
                _temporary: None,
                #[cfg(any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                restored: None,
            });
        }
        let directory = cache_directory()?;
        let temporary = tempfile::NamedTempFile::new_in(directory)?;
        let file = Arc::new(temporary.reopen()?);
        Ok(Self {
            storage: file.clone(),
            file,
            path: temporary.path().canonicalize()?,
            _mount: None,
            compression_store: None,
            layers: None,
            _temporary_layers: None,
            _temporary: Some(temporary),
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            restored: None,
        })
    }

    pub(super) fn enable_compression(&mut self) -> anyhow::Result<()> {
        let cache = cache_directory()?;
        let temporary = if self._temporary.is_some() {
            use std::os::unix::fs::PermissionsExt;
            Some(
                tempfile::Builder::new()
                    .prefix("layers-")
                    .permissions(std::fs::Permissions::from_mode(0o700))
                    .tempdir_in(&cache)?,
            )
        } else {
            None
        };
        let layers = if let Some(directory) = &temporary {
            directory.path().to_path_buf()
        } else {
            let mut name = self.path.as_os_str().to_os_string();
            name.push(".layers");
            let path = PathBuf::from(name);
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            path
        };
        let store = Arc::new(std::sync::Mutex::new(
            crate::ram_backing::CompressedRam::create(self.storage.try_clone()?, &layers)?,
        ));
        let (mount, file) = RamFileMount::mount(store.clone(), &cache)?;
        self.compression_store = Some(store);
        self.file = Arc::new(file);
        self._mount = Some(mount);
        self.layers = Some(layers);
        self._temporary_layers = temporary;
        Ok(())
    }

    pub(super) fn layer_directory(&self) -> Option<&Path> {
        self.layers.as_deref()
    }

    async fn finish_offload(&self) -> anyhow::Result<()> {
        let logical = self.file.clone();
        let physical = self.storage.clone();
        let layers = self.layers.clone();
        let commit_store = self.compression_store.clone();
        tokio::task::spawn_blocking(move || {
            // The VMM has paused CPUs and closed/drained device RAM access.
            // Drain kernel writeback first; compression runs outside the FUSE thread.
            logical.sync_all()?;
            if let Some(store) = commit_store {
                store
                    .lock()
                    .map_err(|_| std::io::Error::other("RAM store lock poisoned"))?
                    .sync_all()?;
            }
            #[cfg(target_os = "linux")]
            {
                use std::os::fd::AsRawFd;
                let error = unsafe {
                    libc::posix_fadvise(physical.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                };
                if error != 0 {
                    return Err(std::io::Error::from_raw_os_error(error));
                }
                if let Some(directory) = layers {
                    for entry in std::fs::read_dir(directory)? {
                        let entry = entry?;
                        // Only immutable generations: transient capture files can
                        // disappear while unrelated kernel writeback is committing.
                        if entry
                            .path()
                            .extension()
                            .is_none_or(|suffix| suffix != "pvdelta")
                        {
                            continue;
                        }
                        let file = OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(entry.path())?;
                        let error = unsafe {
                            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                        };
                        if error != 0 {
                            return Err(std::io::Error::from_raw_os_error(error));
                        }
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = (physical, layers);
            Ok::<_, std::io::Error>(())
        })
        .await??;
        Ok(())
    }

    fn select_path(&mut self, path: &Path) -> anyhow::Result<()> {
        let path = destination(path)?;
        let metadata = self.storage.metadata()?;
        // Reject replacement of the source pathname as well as destination
        // overwrites. The VMM must continue backing the exact inode we opened.
        let source = std::fs::symlink_metadata(&self.path)?;
        anyhow::ensure!(
            source.is_file() && source.dev() == metadata.dev() && source.ino() == metadata.ino(),
            "RAM backing pathname was replaced"
        );
        if path != self.path {
            // Hard-link publication is atomic and never overwrites an existing
            // file. EXDEV rejects unsafe cross-filesystem RAM migration.
            std::fs::hard_link(&self.path, &path).map_err(|error| anyhow::anyhow!("cannot publish live RAM backing at {} (must be a new path on the same filesystem): {error}", path.display()))?;
            let destination = std::fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                destination.is_file()
                    && destination.dev() == metadata.dev()
                    && destination.ino() == metadata.ino(),
                "RAM backing path changed during publication"
            );
            self.path = path;
            // Publishing a temporary compressed manifest makes its referenced
            // generations persistent too. Their private directory stays in place.
            if let Some(directory) = self._temporary_layers.take() {
                let _ = directory.keep();
            }
        }
        Ok(())
    }
}

fn cache_directory() -> anyhow::Result<PathBuf> {
    let directory = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot find user cache for VM RAM"))?
        .join("pvisor/ram");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    Ok(directory)
}

fn destination(path: &Path) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("RAM backing requires a file name"))?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent.canonicalize()?.join(name))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlReply {
    pub state: Option<VmState>,
    pub memory: Option<VmMemory>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<pvisor_core::operation::ExecutionCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<super::checkpoint::CaptureReady>,
}

struct Connection {
    stream: UnixStream,
    backing: RamBacking,
    checkpoint: Option<super::checkpoint::LaunchBinding>,
    filesystem_lowers: Arc<Vec<super::checkpoint::RetainedLower>>,
    private_files: Option<Arc<crate::environment_snapshot::PrivateFilesystemOwner>>,
    suspension: Option<pvisor_core::operation::ExecutionSuspension>,
}

struct SealedCapture {
    checkpoint: pvisor_core::operation::ExecutionCheckpoint,
    lowers: Arc<Vec<super::checkpoint::RetainedLower>>,
    private_files: Option<Arc<crate::environment_snapshot::PrivateFilesystemOwner>>,
}

#[derive(Clone)]
pub(crate) struct VmControl {
    pub(crate) transition: Arc<Mutex<()>>,
    connection: Arc<Mutex<Option<Connection>>>,
    cancellation: CancellationToken,
    #[cfg(target_os = "linux")]
    memory_target: Arc<std::sync::Mutex<Option<Result<super::memory::Target, String>>>>,
}

impl VmControl {
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
        Self {
            transition: Arc::new(Mutex::new(())),
            connection: Arc::new(Mutex::new(None)),
            cancellation,
            #[cfg(target_os = "linux")]
            memory_target: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub(super) async fn attach(&self, stream: UnixStream, backing: RamBacking) {
        self.attach_with_checkpoint(stream, backing, None).await;
    }

    pub(super) async fn attach_with_checkpoint(
        &self,
        stream: UnixStream,
        backing: RamBacking,
        checkpoint: Option<super::checkpoint::LaunchBinding>,
    ) {
        #[allow(unused_mut)]
        let mut filesystem_lowers = Vec::new();
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        if let (Some(binding), Some(restore)) = (&checkpoint, &backing.restored) {
            for owner in &restore._filesystem_owners {
                if binding.filesystem_pool.as_deref() == Some(owner.pool())
                    && let Some(lower) = binding
                        .readonly_lowers
                        .iter()
                        .find(|lower| lower.source == owner.root())
                {
                    filesystem_lowers.push(super::checkpoint::RetainedLower {
                        slot: lower.slot,
                        source: lower.source.clone(),
                        owner: owner.clone(),
                    });
                }
            }
        }
        *self.connection.lock().await = Some(Connection {
            stream,
            backing,
            checkpoint,
            filesystem_lowers: Arc::new(filesystem_lowers),
            private_files: None,
            suspension: None,
        });
    }

    pub(super) async fn detach(&self) -> Option<pvisor_core::operation::ExecutionSuspension> {
        #[cfg(target_os = "linux")]
        {
            *self.memory_target.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        let connection = self.connection.lock().await.take();
        let suspension = connection.as_ref().and_then(|c| c.suspension.clone());
        // Closing a cached FUSE inode and unmounting may wait for writeback.
        let _ = tokio::task::spawn_blocking(move || drop(connection)).await;
        suspension
    }

    #[cfg(target_os = "linux")]
    pub(super) fn track_native_process(&self, pid: u32, ram: &File) {
        let target = super::memory::Target::new(pid, ram).map_err(|e| format!("{e:#}"));
        *self.memory_target.lock().unwrap_or_else(|e| e.into_inner()) = Some(target);
    }

    pub(crate) async fn memory_usage(&self) -> anyhow::Result<pvisor_core::memory::NativeVmMemory> {
        #[cfg(target_os = "linux")]
        {
            let target = self
                .memory_target
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| anyhow::anyhow!("native VM process is not attached"))?
                .map_err(anyhow::Error::msg)?;
            tokio::task::spawn_blocking(move || target.sample()).await?
        }
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("native VM physical memory observation requires Linux")
    }

    pub(crate) async fn cpu_usage(&self) -> anyhow::Result<pvisor_core::cpu::ProcessCpuUsage> {
        #[cfg(target_os = "linux")]
        {
            let target = self
                .memory_target
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| anyhow::anyhow!("native VM process is not attached"))?
                .map_err(anyhow::Error::msg)?;
            tokio::task::spawn_blocking(move || target.cpu_sample()).await?
        }
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("native VM CPU observation requires Linux")
    }

    #[cfg(target_os = "linux")]
    pub(super) fn cpu_exit_observer(&self) -> anyhow::Result<super::exit_cpu::Observer> {
        let target = self
            .memory_target
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("native VM process is not attached"))?
            .map_err(anyhow::Error::msg)?;
        target.cpu_exit_observer()
    }

    pub(crate) async fn command(&self, operation: OperationKind) -> anyhow::Result<ControlReply> {
        anyhow::ensure!(
            matches!(
                operation,
                OperationKind::RunPause
                    | OperationKind::RunResume
                    | OperationKind::RunOffload { .. }
                    | OperationKind::RunCheckpoint { .. }
                    | OperationKind::RunSuspend { .. }
            ),
            "unsupported live VM control primitive"
        );
        let control = self.clone();
        match tokio::spawn(async move { control.exchange(operation).await }).await {
            Ok(result) => result,
            Err(error) => {
                self.cancellation.cancel();
                Err(error.into())
            }
        }
    }

    // One exchange survives caller cancellation so the next request never
    // consumes an acknowledgement belonging to a dropped future.
    async fn exchange(&self, operation: OperationKind) -> anyhow::Result<ControlReply> {
        operation.validate()?;
        let mut guard = self.connection.lock().await;
        let connection = guard.as_mut().ok_or_else(|| {
            anyhow::anyhow!("VM control unavailable: not a VM, not started, or stopped")
        })?;
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        anyhow::ensure!(
            !(matches!(operation, OperationKind::RunOffload { .. })
                && connection.backing.restored.is_some()),
            "CAPABILITY_UNSUPPORTED: restored private COW RAM cannot use writable-backing offload"
        );
        if let OperationKind::RunCheckpoint {
            request_id,
            ram_storage,
        }
        | OperationKind::RunSuspend {
            request_id,
            ram_storage,
        } = &operation
        {
            let binding = connection
                .checkpoint
                .clone()
                .ok_or_else(|| anyhow::anyhow!("VM has no durable execution checkpoint binding"))?;
            let request = request_id.clone();
            let storage = *ram_storage;
            if let Some(checkpoint) = tokio::task::spawn_blocking(move || {
                super::checkpoint::lookup_request(&binding, &request, storage)
            })
            .await??
            {
                anyhow::ensure!(
                    !matches!(operation, OperationKind::RunSuspend { .. }),
                    "suspend cannot reuse a previously sealed capture; await terminal completion"
                );
                return Ok(ControlReply {
                    state: Some(VmState::Running),
                    memory: None,
                    error: None,
                    checkpoint: Some(checkpoint),
                    capture: None,
                });
            }
        }
        if let OperationKind::RunOffload { file: Some(path) } = &operation {
            connection.backing.select_path(path)?;
        }
        let capture_directory = if matches!(
            operation,
            OperationKind::RunCheckpoint { .. } | OperationKind::RunSuspend { .. }
        ) {
            let binding = connection
                .checkpoint
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("VM has no durable execution checkpoint binding"))?;
            Some(
                tempfile::Builder::new()
                    .prefix("capture-")
                    .tempdir_in(binding.store.join("captures"))?,
            )
        } else {
            None
        };
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let incremental_base = if matches!(
            operation,
            OperationKind::RunCheckpoint {
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                ..
            } | OperationKind::RunSuspend {
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                ..
            }
        ) {
            connection
                .backing
                .restored
                .as_ref()
                .and_then(|restore| restore._ram_owner.base.clone())
        } else {
            None
        };
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        let incremental_base: Option<Arc<crate::environment_snapshot::PinnedRamBlocks>> = None;
        // Keep verified lower owners through publication, including control
        // cancellation/teardown. Live initial VMs acquire these after first seal.
        let filesystem_pin = connection.filesystem_lowers.clone();
        let private_pin = connection.private_files.clone();
        let filesystem_reuse = filesystem_pin
            .iter()
            .map(super::checkpoint::RetainedLower::reuse)
            .collect::<Vec<_>>();
        let request = if let Some(directory) = &capture_directory {
            serde_json::to_vec(&super::checkpoint::CaptureRequest {
                operation: operation.clone(),
                directory: directory.path().to_owned(),
                filesystem_reuse: filesystem_reuse.clone(),
                #[cfg(any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                ram_delta: incremental_base
                    .as_ref()
                    .map(|base| -> std::io::Result<_> {
                        let metadata = connection.backing.file.metadata()?;
                        Ok(pvisor_vm::api::RamDeltaSpec {
                            device: metadata.dev(),
                            inode: metadata.ino(),
                            length: base.blocks.length,
                            block_bytes: crate::ram_backing::BLOCK_BYTES as u32,
                            base_sha256: base.sha256.clone(),
                        })
                    })
                    .transpose()
                    .map_err(anyhow::Error::from)?,
            })?
        } else {
            serde_json::to_vec(&operation)?
        };
        anyhow::ensure!(request.len() <= MAX_FRAME, "VM control request too large");
        let budget = if matches!(
            operation,
            OperationKind::RunOffload { .. }
                | OperationKind::RunCheckpoint { .. }
                | OperationKind::RunSuspend { .. }
        ) {
            300
        } else {
            10
        };
        let checkpoint_run_id = connection
            .checkpoint
            .as_ref()
            .filter(|_| capture_directory.is_some())
            .map(|binding| binding.run_id.clone());
        let result = tokio::time::timeout(Duration::from_secs(budget), async {
            connection.stream.write_u32(request.len() as u32).await?;
            connection.stream.write_all(&request).await?;
            if let Some(run_id) = &checkpoint_run_id {
                crate::util::startup_mark_run("checkpoint.request_sent", run_id);
            }
            let mut reply = read_reply(&mut connection.stream).await?;
            let mut committed = None;
            if let Some(ready) = reply.capture.take() {
                anyhow::ensure!(
                    reply.error.is_none()
                        && reply.state == Some(VmState::Paused)
                        && reply.memory.is_none()
                        && reply.checkpoint.is_none(),
                    "invalid frozen capture acknowledgement"
                );
                let (OperationKind::RunCheckpoint { ram_storage, .. }
                | OperationKind::RunSuspend { ram_storage, .. }) = operation
                else {
                    anyhow::bail!("unexpected native capture")
                };
                let binding = connection
                    .checkpoint
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("missing checkpoint binding"))?;
                let directory = capture_directory
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing capture directory"))?
                    .path()
                    .to_owned();
                anyhow::ensure!(
                    ready.directory == directory
                        && ready.run_id == binding.run_id
                        && ready.attempt_id == binding.attempt_id
                        && ready.created_at_unix_ms > 0,
                    "native capture readiness binding mismatch"
                );
                let (OperationKind::RunCheckpoint { ref request_id, .. }
                | OperationKind::RunSuspend { ref request_id, .. }) = operation
                else {
                    unreachable!()
                };
                let request_id = request_id.clone();
                let run_id = binding.run_id.clone();
                // Include blocking-pool queue time as well as publication work.
                crate::util::startup_mark_run("checkpoint.host_publication_begin", &run_id);
                let published = tokio::task::spawn_blocking(
                    move || -> anyhow::Result<anyhow::Result<SealedCapture>> {
                        let _private_pin = private_pin;
                        match super::checkpoint::publish(
                            &binding,
                            ready,
                            &directory,
                            ram_storage,
                            incremental_base.as_deref(),
                            filesystem_pin.as_slice(),
                        )? {
                            super::checkpoint::Publication::Sealed {
                                checkpoint,
                                lowers,
                                private_files,
                            } => {
                                // Once sealed, an unrecorded receipt is uncertain.
                                // Fail-stop instead of allowing another capture at
                                // a different execution point under the same key.
                                super::checkpoint::remember_request(
                                    &binding,
                                    &request_id,
                                    &checkpoint,
                                )?;
                                Ok(Ok(SealedCapture {
                                    checkpoint,
                                    lowers: Arc::new(lowers),
                                    private_files,
                                }))
                            }
                            super::checkpoint::Publication::Rejected(error) => Ok(Err(error)),
                        }
                    },
                )
                .await??;
                crate::util::startup_mark_run("checkpoint.host_publication_done", &run_id);
                if let Ok(sealed) = &published {
                    connection.filesystem_lowers = sealed.lowers.clone();
                    connection.private_files = sealed.private_files.clone();
                }
                let commit = super::checkpoint::CommitReply {
                    checkpoint: published
                        .as_ref()
                        .ok()
                        .map(|sealed| sealed.checkpoint.clone()),
                    error: published
                        .as_ref()
                        .err()
                        .map(|e| format!("{e:#}").chars().take(2048).collect()),
                };
                let bytes = serde_json::to_vec(&commit)?;
                anyhow::ensure!(
                    bytes.len() <= MAX_FRAME,
                    "checkpoint commit response too large"
                );
                crate::util::startup_mark_run("checkpoint.commit_begin", &run_id);
                connection.stream.write_u32(bytes.len() as u32).await?;
                connection.stream.write_all(&bytes).await?;
                crate::util::startup_mark_run("checkpoint.commit_sent", &run_id);
                reply = read_reply(&mut connection.stream).await?;
                crate::util::startup_mark_run("checkpoint.ack_received", &run_id);
                committed = commit.checkpoint;
                anyhow::ensure!(
                    reply.capture.is_none(),
                    "duplicate frozen capture acknowledgement"
                );
            }
            if let Some(error) = &reply.error {
                // A complete rejection with a known live state leaves the
                // connection usable. Unknown transition errors remain fail-stop.
                if matches!(reply.state, Some(VmState::Running | VmState::Paused))
                    && reply.memory.is_none()
                    && reply.checkpoint.is_none()
                    && reply.capture.is_none()
                {
                    return Ok(Err(anyhow::anyhow!("VMM rejected control: {error}")));
                }
            }
            anyhow::ensure!(
                reply.error.is_none(),
                "VMM rejected control: {}",
                reply.error.as_deref().unwrap_or_default()
            );
            let expected = match operation {
                OperationKind::RunPause => VmState::Paused,
                OperationKind::RunResume => VmState::Running,
                OperationKind::RunOffload { .. } => VmState::Offloaded,
                OperationKind::RunCheckpoint { .. } => VmState::Running,
                OperationKind::RunSuspend { .. } => VmState::Paused,
                _ => anyhow::bail!("not a VM control primitive"),
            };
            anyhow::ensure!(reply.state == Some(expected), "unexpected VM control state");
            if expected == VmState::Offloaded {
                connection.backing.finish_offload().await?;
                let memory = reply
                    .memory
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("missing RAM reclaim report"))?;
                memory.backing_file = connection.backing.path.clone();
            }
            let value = if let OperationKind::RunCheckpoint { ram_storage, .. }
            | OperationKind::RunSuspend { ram_storage, .. } = &operation
            {
                anyhow::ensure!(
                    reply.memory.is_none()
                        && reply.checkpoint.is_some()
                        && reply.checkpoint == committed,
                    "checkpoint acknowledgement does not match committed object"
                );
                let checkpoint = reply.checkpoint.as_ref().unwrap();
                let binding = connection.checkpoint.as_ref().unwrap();
                anyhow::ensure!(
                    checkpoint.store == binding.store
                        && checkpoint.source_run_id == binding.run_id
                        && checkpoint.source_attempt_id == binding.attempt_id
                        && checkpoint.ram_storage == *ram_storage,
                    "checkpoint authority mismatch"
                );
                pvisor_core::operation::Value::ExecutionCheckpoint {
                    checkpoint: checkpoint.clone(),
                }
            } else {
                anyhow::ensure!(
                    reply.checkpoint.is_none(),
                    "unexpected execution checkpoint"
                );
                pvisor_core::operation::Value::Vm {
                    state: expected,
                    memory: reply.memory.clone(),
                }
            };
            pvisor_core::operation::Outcome::success(value).validate()?;
            if let OperationKind::RunSuspend { request_id, .. } = &operation {
                connection.suspension = Some(pvisor_core::operation::ExecutionSuspension {
                    request_id: request_id.clone(),
                    checkpoint: reply.checkpoint.clone().unwrap(),
                });
            }
            Ok::<_, anyhow::Error>(Ok(reply))
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("VM control timed out; terminating attempt")));
        if result.is_err() {
            let connection = guard.take();
            self.cancellation.cancel();
            drop(tokio::task::spawn_blocking(move || drop(connection)));
        }
        result?
    }
}

async fn read_reply(stream: &mut UnixStream) -> anyhow::Result<ControlReply> {
    let size = stream.read_u32().await? as usize;
    anyhow::ensure!(size <= MAX_FRAME, "VM control response too large");
    let mut response = vec![0; size];
    stream.read_exact(&mut response).await?;
    Ok(serde_json::from_slice(&response)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[tokio::test]
    async fn unsupported_checkpoint_never_writes_to_or_cancels_live_connection() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let directory = tempfile::tempdir().unwrap();
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
            )
            .await;
        let operation: OperationKind = serde_json::from_value(serde_json::json!({
            "op": "run.checkpoint", "request_id": "unsupported", "ram_storage": "raw"
        }))
        .unwrap();
        assert!(control.command(operation).await.is_err());
        assert!(!cancellation.is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), runner.read_u32())
                .await
                .is_err()
        );
        assert!(control.connection.lock().await.is_some());
        control.detach().await;
    }

    fn checkpoint_binding(directory: &Path) -> super::super::checkpoint::LaunchBinding {
        let store = directory.join("snapshots");
        crate::environment_snapshot::SnapshotStore::new(&store).unwrap();
        std::fs::create_dir(store.join("captures")).unwrap();
        super::super::checkpoint::LaunchBinding {
            store,
            filesystem_pool: None,
            readonly_lowers: vec![],
            private_roots: vec![],
            compatibility: crate::environment_snapshot::Compatibility {
                host_boot: "test-boot".into(),
                build: "test-build".into(),
                firmware: "test-firmware".into(),
                profile: "test-profile".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        }
    }

    async fn read_capture(runner: &mut UnixStream) -> super::super::checkpoint::CaptureRequest {
        let size = runner.read_u32().await.unwrap() as usize;
        let mut bytes = vec![0; size];
        runner.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn suspend_ack_without_host_sealing_cancels_and_cannot_prove_native_termination() {
        let directory = tempfile::tempdir().unwrap();
        let binding = checkpoint_binding(directory.path());
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach_with_checkpoint(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
                Some(binding.clone()),
            )
            .await;
        let peer = tokio::spawn(async move {
            let request = read_capture(&mut runner).await;
            assert!(matches!(
                request.operation,
                OperationKind::RunSuspend { .. }
            ));
            send_reply(
                &mut runner,
                ControlReply {
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                    capture: None,
                    checkpoint: Some(pvisor_core::operation::ExecutionCheckpoint {
                        snapshot_id: "e".repeat(64),
                        store: binding.store,
                        source_run_id: binding.run_id,
                        source_attempt_id: binding.attempt_id,
                        created_at_unix_ms: 1,
                        ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                    }),
                },
            )
            .await;
        });
        assert!(
            control
                .command(OperationKind::RunSuspend {
                    request_id: "save-stop".into(),
                    ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                })
                .await
                .is_err()
        );
        assert!(cancellation.is_cancelled());
        assert!(control.detach().await.is_none());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn checkpoint_publication_failure_drains_commit_ack_before_next_control() {
        let directory = tempfile::tempdir().unwrap();
        let binding = checkpoint_binding(directory.path());
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach_with_checkpoint(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
                Some(binding),
            )
            .await;
        let peer = tokio::spawn(async move {
            let request = read_capture(&mut runner).await;
            // Missing payload models a rejected/damaged capture. The host
            // must send a failure commit, then consume the source-resume ack.
            send_reply(
                &mut runner,
                ControlReply {
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                    checkpoint: None,
                    capture: Some(super::super::checkpoint::CaptureReady {
                        directory: request.directory,
                        run_id: "run".into(),
                        attempt_id: "attempt".into(),
                        created_at_unix_ms: 1,
                    }),
                },
            )
            .await;
            let size = runner.read_u32().await.unwrap() as usize;
            let mut bytes = vec![0; size];
            runner.read_exact(&mut bytes).await.unwrap();
            let commit: super::super::checkpoint::CommitReply =
                serde_json::from_slice(&bytes).unwrap();
            assert!(commit.checkpoint.is_none() && commit.error.is_some());
            send_reply(
                &mut runner,
                ControlReply {
                    state: Some(VmState::Running),
                    memory: None,
                    error: Some("host publication failed".into()),
                    checkpoint: None,
                    capture: None,
                },
            )
            .await;
            assert_eq!(read_request(&mut runner).await, OperationKind::RunPause);
            send_reply(
                &mut runner,
                ControlReply {
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                    checkpoint: None,
                    capture: None,
                },
            )
            .await;
        });
        let error = control
            .command(OperationKind::RunCheckpoint {
                request_id: "save".into(),
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("publication"), "{error:#}");
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            control
                .command(OperationKind::RunPause)
                .await
                .unwrap()
                .state,
            Some(VmState::Paused)
        );
        peer.await.unwrap();
        assert!(!cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn malformed_checkpoint_readiness_or_uncommitted_success_cancels_attempt() {
        for early_success in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let binding = checkpoint_binding(directory.path());
            let store = binding.store.clone();
            let cancellation = CancellationToken::new();
            let control = VmControl::new(cancellation.clone());
            let (host, mut runner) = UnixStream::pair().unwrap();
            control
                .attach_with_checkpoint(
                    host,
                    RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
                    Some(binding),
                )
                .await;
            let peer = tokio::spawn(async move {
                let request = read_capture(&mut runner).await;
                let mut reply = ControlReply {
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                    checkpoint: None,
                    capture: Some(super::super::checkpoint::CaptureReady {
                        directory: request.directory,
                        run_id: "other".into(),
                        attempt_id: "attempt".into(),
                        created_at_unix_ms: 1,
                    }),
                };
                if early_success {
                    reply.state = Some(VmState::Running);
                    reply.capture = None;
                    reply.checkpoint = Some(pvisor_core::operation::ExecutionCheckpoint {
                        snapshot_id: "a".repeat(64),
                        store,
                        source_run_id: "run".into(),
                        source_attempt_id: "attempt".into(),
                        created_at_unix_ms: 1,
                        ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                    });
                }
                send_reply(&mut runner, reply).await;
            });
            assert!(
                control
                    .command(OperationKind::RunCheckpoint {
                        request_id: "save".into(),
                        ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed
                    })
                    .await
                    .is_err()
            );
            peer.await.unwrap();
            assert!(cancellation.is_cancelled());
            assert!(control.connection.lock().await.is_none());
        }
    }

    #[test]
    fn backing_is_private_persistent_and_rejects_symlinks_and_missing_parents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ram");
        let backing = RamBacking::create(Some(&path)).unwrap();
        assert!(backing.path.is_absolute());
        assert_eq!(
            backing.file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(backing.file.metadata().unwrap().len(), 0);
        drop(backing);
        assert!(path.is_file());
        let link = directory.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(RamBacking::create(Some(&link)).is_err());
        assert!(RamBacking::create(Some(&directory.path().join("missing/ram"))).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn replaced_source_cannot_publish_a_different_inode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ram");
        let mut backing = RamBacking::create(Some(&path)).unwrap();
        std::fs::rename(&path, directory.path().join("original")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        let target = directory.path().join("offload");
        assert!(backing.select_path(&target).is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }

    #[test]
    fn publication_uses_physical_inode_instead_of_the_logical_ram_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("container");
        let physical = RamBacking::create(Some(&path)).unwrap();
        let mut backing = RamBacking {
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            restored: None,
            storage: physical.storage.clone(),
            file: Arc::new(tempfile::tempfile().unwrap()),
            path: physical.path.clone(),
            _mount: None,
            compression_store: None,
            layers: None,
            _temporary_layers: None,
            _temporary: None,
        };
        let target = directory.path().join("published");
        backing.select_path(&target).unwrap();
        assert_eq!(
            std::fs::metadata(target).unwrap().ino(),
            backing.storage.metadata().unwrap().ino()
        );
        assert_ne!(
            backing.file.metadata().unwrap().ino(),
            backing.storage.metadata().unwrap().ino()
        );
    }

    #[test]
    fn temporary_name_is_removed_but_published_backing_survives_drop() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        let original = temporary.path().to_owned();
        let mut backing = RamBacking {
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            restored: None,
            storage: Arc::new(temporary.reopen().unwrap()),
            file: Arc::new(temporary.reopen().unwrap()),
            path: original.clone(),
            _mount: None,
            compression_store: None,
            layers: None,
            _temporary_layers: None,
            _temporary: Some(temporary),
        };
        let published = directory.path().join("published.ram");
        backing.select_path(&published).unwrap();
        backing.select_path(&published).unwrap();
        (&*backing.file).write_all(b"persist").unwrap();
        drop(backing);
        assert!(!original.exists());
        assert_eq!(std::fs::read(published).unwrap(), b"persist");
    }

    #[tokio::test]
    async fn unavailable_or_detached_control_rejects_without_cancelling() {
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        assert!(control.command(OperationKind::RunPause).await.is_err());
        let directory = tempfile::tempdir().unwrap();
        let (host, _runner) = UnixStream::pair().unwrap();
        control
            .attach(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
            )
            .await;
        control.detach().await;
        assert!(control.command(OperationKind::RunResume).await.is_err());
        assert!(!cancellation.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn missing_acknowledgement_times_out_without_real_sleep() {
        for operation in [
            OperationKind::RunPause,
            OperationKind::RunOffload { file: None },
        ] {
            let directory = tempfile::tempdir().unwrap();
            let cancellation = CancellationToken::new();
            let control = VmControl::new(cancellation.clone());
            let (host, _runner) = UnixStream::pair().unwrap();
            control
                .attach(
                    host,
                    RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
                )
                .await;
            let started = tokio::time::Instant::now();
            let seconds = if matches!(operation, OperationKind::RunPause) {
                10
            } else {
                300
            };
            let error = control.command(operation).await.unwrap_err();
            assert!(error.to_string().contains("timed out"));
            assert_eq!(started.elapsed(), Duration::from_secs(seconds));
            assert!(control.connection.lock().await.is_none());
            assert!(cancellation.is_cancelled());
        }
    }

    async fn read_request(runner: &mut UnixStream) -> OperationKind {
        let size = runner.read_u32().await.unwrap() as usize;
        assert!(size <= MAX_FRAME);
        let mut bytes = vec![0; size];
        runner.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn send_reply(runner: &mut UnixStream, reply: ControlReply) {
        let bytes = serde_json::to_vec(&reply).unwrap();
        runner.write_u32(bytes.len() as u32).await.unwrap();
        runner.write_all(&bytes).await.unwrap();
    }

    #[tokio::test]
    async fn known_live_rejection_preserves_connection_for_next_control() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
            )
            .await;
        let peer = tokio::spawn(async move {
            assert_eq!(
                read_request(&mut runner).await,
                OperationKind::RunOffload { file: None }
            );
            send_reply(
                &mut runner,
                ControlReply {
                    checkpoint: None,
                    capture: None,
                    state: Some(VmState::Running),
                    memory: None,
                    error: Some("incompatible cold pager".into()),
                },
            )
            .await;
            assert_eq!(read_request(&mut runner).await, OperationKind::RunPause);
            send_reply(
                &mut runner,
                ControlReply {
                    checkpoint: None,
                    capture: None,
                    state: Some(VmState::Paused),
                    memory: None,
                    error: None,
                },
            )
            .await;
        });
        assert!(
            control
                .command(OperationKind::RunOffload { file: None })
                .await
                .is_err()
        );
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            control
                .command(OperationKind::RunPause)
                .await
                .unwrap()
                .state,
            Some(VmState::Paused)
        );
        peer.await.unwrap();
        assert!(!cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn offload_rejection_preserves_connection_and_success_uses_owned_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ram");
        let target = directory.path().join("offload");
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach(host, RamBacking::create(Some(&path)).unwrap())
            .await;
        let occupied = directory.path().join("occupied");
        std::fs::write(&occupied, b"keep").unwrap();
        assert!(
            control
                .command(OperationKind::RunOffload {
                    file: Some(occupied.clone())
                })
                .await
                .is_err()
        );
        assert!(!cancellation.is_cancelled());
        assert_eq!(std::fs::read(&occupied).unwrap(), b"keep");
        let peer = tokio::spawn(async move {
            assert_eq!(
                read_request(&mut runner).await,
                OperationKind::RunOffload { file: Some(target) }
            );
            send_reply(
                &mut runner,
                ControlReply {
                    checkpoint: None,
                    capture: None,
                    state: Some(VmState::Offloaded),
                    memory: Some(VmMemory {
                        backing_file: "/untrusted/runner-path".into(),
                        backed_bytes: 4096,
                        resident_before_bytes: Some(4096),
                        resident_after_bytes: Some(0),
                    }),
                    error: None,
                },
            )
            .await;
        });
        let reply = control
            .command(OperationKind::RunOffload {
                file: Some(directory.path().join("offload")),
            })
            .await
            .unwrap();
        assert_eq!(
            reply.memory.unwrap().backing_file,
            directory.path().canonicalize().unwrap().join("offload")
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().ino(),
            std::fs::metadata(directory.path().join("offload"))
                .unwrap()
                .ino()
        );
        peer.await.unwrap();
        assert!(!cancellation.is_cancelled());
    }

    // Characterizes VMR-03: a published alias survives a rejected transition.
    // This is an observable side effect, not a successful offload.
    #[tokio::test]
    async fn rejected_offload_leaves_published_backing_alias() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("ram");
        let target = directory.path().join("published");
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach(host, RamBacking::create(Some(&source)).unwrap())
            .await;
        let published = target.clone();
        let peer = tokio::spawn(async move {
            assert_eq!(
                read_request(&mut runner).await,
                OperationKind::RunOffload {
                    file: Some(published.clone())
                }
            );
            assert!(published.exists());
            send_reply(
                &mut runner,
                ControlReply {
                    checkpoint: None,
                    capture: None,
                    state: None,
                    memory: None,
                    error: Some("injected offload failure".into()),
                },
            )
            .await;
        });
        assert!(
            control
                .command(OperationKind::RunOffload {
                    file: Some(target.clone())
                })
                .await
                .is_err()
        );
        peer.await.unwrap();
        assert!(cancellation.is_cancelled());
        assert_eq!(
            std::fs::metadata(source).unwrap().ino(),
            std::fs::metadata(target).unwrap().ino()
        );
    }

    #[tokio::test]
    async fn invalid_acknowledgements_discard_connection_and_cancel_attempt() {
        for fault in ["oversize", "json", "state", "report", "disconnect"] {
            let directory = tempfile::tempdir().unwrap();
            let cancellation = CancellationToken::new();
            let control = VmControl::new(cancellation.clone());
            let (host, mut runner) = UnixStream::pair().unwrap();
            control
                .attach(
                    host,
                    RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
                )
                .await;
            let peer = tokio::spawn(async move {
                assert_eq!(read_request(&mut runner).await, OperationKind::RunPause);
                match fault {
                    "oversize" => runner.write_u32((MAX_FRAME + 1) as u32).await.unwrap(),
                    "json" => {
                        runner.write_u32(1).await.unwrap();
                        runner.write_all(b"{").await.unwrap();
                    }
                    "disconnect" => (),
                    _ => {
                        send_reply(
                            &mut runner,
                            ControlReply {
                                checkpoint: None,
                                capture: None,
                                state: Some(if fault == "state" {
                                    VmState::Running
                                } else {
                                    VmState::Paused
                                }),
                                memory: (fault == "report").then(|| VmMemory {
                                    backing_file: "/ram".into(),
                                    backed_bytes: 4096,
                                    resident_before_bytes: None,
                                    resident_after_bytes: None,
                                }),
                                error: None,
                            },
                        )
                        .await
                    }
                }
            });
            assert!(
                control.command(OperationKind::RunPause).await.is_err(),
                "{fault}"
            );
            peer.await.unwrap();
            assert!(cancellation.is_cancelled(), "{fault}");
            assert!(control.connection.lock().await.is_none(), "{fault}");
        }
    }

    #[test]
    fn backing_publication_keeps_live_inode_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("ram");
        let target = directory.path().join("offloaded");
        let mut backing = RamBacking::create(Some(&source)).unwrap();
        backing.select_path(&target).unwrap();
        (&*backing.file).write_all(b"live RAM").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"live RAM");
        assert_eq!(
            std::fs::metadata(&source).unwrap().ino(),
            std::fs::metadata(&target).unwrap().ino()
        );
        assert!(RamBacking::create(Some(&target)).is_err());
        assert!(backing.select_path(&source).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"live RAM");
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_abandon_ack_and_rejection_cancels_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        let control = VmControl::new(cancellation.clone());
        let (host, mut runner) = UnixStream::pair().unwrap();
        control
            .attach(
                host,
                RamBacking::create(Some(&directory.path().join("ram"))).unwrap(),
            )
            .await;
        let request = tokio::spawn({
            let control = control.clone();
            async move { control.command(OperationKind::RunPause).await }
        });
        let size = runner.read_u32().await.unwrap() as usize;
        let mut data = vec![0; size];
        runner.read_exact(&mut data).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<OperationKind>(&data).unwrap(),
            OperationKind::RunPause
        );
        assert!(!request.is_finished());
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let ack = serde_json::to_vec(&ControlReply {
            checkpoint: None,
            capture: None,
            state: Some(VmState::Paused),
            memory: None,
            error: None,
        })
        .unwrap();
        runner.write_u32(ack.len() as u32).await.unwrap();
        runner.write_all(&ack).await.unwrap();
        let request = tokio::spawn({
            let control = control.clone();
            async move { control.command(OperationKind::RunResume).await }
        });
        let size = runner.read_u32().await.unwrap() as usize;
        let mut data = vec![0; size];
        runner.read_exact(&mut data).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<OperationKind>(&data).unwrap(),
            OperationKind::RunResume
        );
        let ack = serde_json::to_vec(&ControlReply {
            checkpoint: None,
            capture: None,
            state: None,
            memory: None,
            error: Some("partial transition".into()),
        })
        .unwrap();
        runner.write_u32(ack.len() as u32).await.unwrap();
        runner.write_all(&ack).await.unwrap();
        assert!(request.await.unwrap().is_err());
        assert!(cancellation.is_cancelled());
        assert!(control.command(OperationKind::RunResume).await.is_err());
    }
}
