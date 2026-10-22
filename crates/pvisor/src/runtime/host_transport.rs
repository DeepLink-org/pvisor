//! Host-only AgentCtl transport. No cooperative guest token grants host authority.
//!
//! Frames are compact newline-delimited JSON, bounded by
//! [`AGENTCTL_HOST_MAX_FRAME_BYTES`] excluding the delimiter. Codec functions do
//! not validate envelopes or authorize commands: endpoint owners retain version,
//! target, secret, admission and timeout checks.
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_MAX_FRAME_BYTES, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlTarget,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

/// All host-authority sockets share this service-compatible exclusion root.
/// Never chmod or follow a preexisting entry: an invalid root fails startup.
pub fn host_authority_root() -> anyhow::Result<std::path::PathBuf> {
    let root =
        std::fs::canonicalize("/tmp")?.join(format!("pvisor-host-{}", unsafe { libc::geteuid() }));
    create_authority_root(&root)?;
    Ok(root)
}

fn validate_authority_directory(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o700,
        "host authority directory must be same-UID, non-symlink and 0700"
    );
    let canonical = path.canonicalize()?;
    let checked = std::fs::symlink_metadata(&canonical)?;
    anyhow::ensure!(
        canonical == path && metadata.dev() == checked.dev() && metadata.ino() == checked.ino(),
        "host authority directory changed during validation"
    );
    Ok(())
}

fn create_authority_root(root: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_authority_directory(root)
}

pub(crate) fn allocate_host_directory(prefix: &str) -> anyhow::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let root = host_authority_root()?;
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&root)?;
    validate_authority_directory(&root)?;
    validate_authority_directory(directory.path())?;
    Ok(directory)
}

/// Wire contract for one running Job owner; independent of its binary digest.
pub const INSTANCE_CONTROL_PROTOCOL: &str = "pvisor-instance-control-1";

/// Host-only discovery persisted before execution, cleared on terminal publication.
/// The path is canonical, excluded from guest inputs, and bound to device/inode.
/// Binary identity diagnoses incompatible owners; it is not a client admission key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveControlRecord {
    /// Canonical host-only socket address; never a guest locator.
    pub path: std::path::PathBuf,
    /// Device sampled after binding the socket.
    pub device: u64,
    /// Inode sampled after binding the socket.
    pub inode: u64,
    /// Named instance wire contract, checked independently of executable identity.
    pub protocol: String,
    /// Starting executable path, reported for protocol incompatibility.
    pub binary: std::path::PathBuf,
    /// Starting executable BLAKE3 digest for diagnostics.
    pub binary_digest: String,
    /// Owner PID used with socket identity to reject dead discovery records.
    pub pid: u32,
}

/// Commands accepted by an instance owner; no terminal descriptors or guest tokens.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstanceCommand {
    /// Read native VM status; non-VM executors return Unsupported.
    Status {},
    /// Read Job status and owner observations.
    JobStatus {},
    /// Request cancellation; completion and resource release remain owner-owned.
    Terminate {},
    /// Pause vCPUs of the same live VM Attempt.
    Pause {},
    /// Resume/unpause the same live VM Attempt; never restores a checkpoint.
    Resume {},
    /// Quiesce the VM and offload RAM using its existing transition lock.
    Offload {
        /// Optional host-only RAM file; None reuses current backing.
        file: Option<std::path::PathBuf>,
    },
    /// Capture execution with a scoped receipt; cancellation may follow effects.
    Capture {
        /// Stop the Attempt after sealing its checkpoint.
        suspend: bool,
        /// Encoding for captured RAM.
        ram_storage: pvisor_core::operation::SnapshotRamStorage,
        /// Maximum caller wait in milliseconds; timeout does not undo effects.
        timeout_ms: u64,
        /// Scoped durable capture receipt key.
        request_id: String,
    },
    /// Retain a read-only workspace mount in the owner until unmount or exit.
    Inspect {
        /// Optional immutable workspace checkpoint; None inspects current stage.
        checkpoint: Option<String>,
    },
    /// Release one owner-created inspect mount.
    UnmountInspect {
        /// Mount identity returned by Inspect.
        id: String,
    },
}

/// Create checked, private authority directories for one exact Job/Attempt.
/// IDs must each be a single normal path component; existing symlinks fail closed.
pub fn instance_control_path(job: &str, attempt: &str) -> anyhow::Result<std::path::PathBuf> {
    let mut directory = host_authority_root()?;
    for name in ["jobs", job, attempt] {
        let mut components = std::path::Path::new(name).components();
        anyhow::ensure!(
            matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none(),
            "invalid instance control identity"
        );
        directory.push(name);
        create_authority_root(&directory)?;
    }
    Ok(directory.join("control.sock"))
}

/// Validate all authority ancestors without following symlinks or creating state.
pub fn validate_instance_socket(path: &std::path::Path) -> anyhow::Result<()> {
    let root =
        std::fs::canonicalize("/tmp")?.join(format!("pvisor-host-{}", unsafe { libc::geteuid() }));
    let relative = path.strip_prefix(&root)?;
    let mut directory = root;
    validate_authority_directory(&directory)?;
    let parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("control socket needs a parent"))?;
    for part in parent.components() {
        anyhow::ensure!(
            matches!(part, std::path::Component::Normal(_)),
            "invalid control socket path"
        );
        directory.push(part);
        validate_authority_directory(&directory)?;
    }
    anyhow::ensure!(
        path.file_name().is_some(),
        "control socket needs a file name"
    );
    Ok(())
}

/// Resolve a caller's absolute debug path through the standard `/tmp` alias,
/// then validate every private ancestor without following user-created symlinks.
/// Existing sockets are not removed or replaced.
pub fn instance_socket_override(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let absolute = std::path::absolute(path)?;
    let path = match absolute.strip_prefix("/tmp") {
        Ok(relative) => std::fs::canonicalize("/tmp")?.join(relative),
        Err(_) => absolute,
    };
    validate_instance_socket(&path)?;
    Ok(path)
}

// Full Job/Attempt paths exceed sockaddr_un even with ordinary UUIDs. Resolve
// only the checked parent through a directory fd, retaining the canonical record.
fn with_instance_socket<T: Send + 'static>(
    path: &std::path::Path,
    operation: impl FnOnce(&std::path::Path) -> std::io::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("socket needs a parent"))?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("socket needs a name"))?
        .to_owned();
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        Ok(operation(
            &std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
                .join(name),
        )?)
    }
    #[cfg(target_os = "macos")]
    {
        // Darwin supports a thread-local cwd. A short-lived thread cannot change
        // the owner's process cwd or another runtime thread's path resolution.
        Ok(std::thread::spawn(move || {
            use std::os::fd::AsRawFd;
            unsafe extern "C" {
                fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
            }
            if unsafe { pthread_fchdir_np(directory.as_raw_fd()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            operation(std::path::Path::new(&name))
        })
        .join()
        .map_err(|_| anyhow::anyhow!("instance socket thread panicked"))??)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (directory, name, operation);
        anyhow::bail!("instance sockets are unsupported");
    }
}

/// Bind a checked private endpoint without truncating long canonical paths.
pub fn bind_instance_socket(
    path: &std::path::Path,
) -> anyhow::Result<std::os::unix::net::UnixListener> {
    validate_instance_socket(path)?;
    with_instance_socket(path, |path| std::os::unix::net::UnixListener::bind(path))
}

impl LiveControlRecord {
    /// Submit one bounded, explicitly targeted command. No automatic replay after
    /// a write, cancellation or missing response: the operation may have happened.
    pub fn exchange(
        &self,
        job: &str,
        attempt: &str,
        command: InstanceCommand,
    ) -> anyhow::Result<serde_json::Value> {
        self.exchange_checked(job, attempt, command, None)
    }

    /// Exchange with caller cancellation checks while awaiting I/O. Cancellation
    /// closes only this connection, joins its I/O thread, and reports uncertain
    /// effects without resending. The check must be quick and must not take Job locks.
    pub fn exchange_checked(
        &self,
        job: &str,
        attempt: &str,
        command: InstanceCommand,
        check: Option<&(dyn Fn() -> anyhow::Result<()> + Send + Sync)>,
    ) -> anyhow::Result<serde_json::Value> {
        if let Some(check) = check {
            check()?;
        }
        use pvisor_core::host_protocol::{
            AGENTCTL_HOST_VERSION, AgentCtlHostRequest, AgentCtlHostResponse, AgentCtlTarget,
        };
        let request = AgentCtlHostRequest {
            version: AGENTCTL_HOST_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            target: Some(AgentCtlTarget {
                job_id: job.to_owned(),
                attempt_id: Some(attempt.to_owned()),
                generation: None,
            }),
            command,
        };
        request.validate()?;
        let mut stream = self.connect()?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(315)))?;
        let interrupt = stream.try_clone()?;
        let mut exchange = move || -> anyhow::Result<serde_json::Value> {
            write_host_frame_sync(&mut stream, &request).map_err(|error| {
                anyhow::anyhow!(
                    "control request write failed; effects may be uncertain; not retried: {error}"
                )
            })?;
            let response: AgentCtlHostResponse<serde_json::Value> = read_host_frame_sync(&mut stream)
            .map_err(|error| {
            anyhow::anyhow!(
                "control response lost or timed out; effects may be uncertain; not retried: {error}"
            )
        })?;
            response.validate(&request.request_id)?;
            Ok(response.result?)
        };
        let Some(check) = check else {
            return exchange();
        };
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let _ = tx.send(exchange());
            });
            loop {
                match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                    Ok(result) => return result,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        anyhow::bail!("control I/O failed; effects may be uncertain; not retried")
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Err(error) = check() {
                            let _ = interrupt.shutdown(std::net::Shutdown::Both);
                            anyhow::bail!(
                                "control request cancelled; effects may be uncertain; not retried: {error}"
                            );
                        }
                    }
                }
            }
        })
    }

    /// Whether discovery metadata still matches the socket inode and a live PID.
    /// Connection must still prove listener availability and authenticate its peer;
    /// this check alone neither grants authority nor rules out PID reuse.
    pub fn is_available(&self) -> bool {
        self.check_identity().is_ok()
    }

    fn check_identity(&self) -> anyhow::Result<()> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        validate_instance_socket(&self.path)?;
        let metadata = std::fs::symlink_metadata(&self.path)?;
        anyhow::ensure!(
            metadata.file_type().is_socket()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o7777 == 0o600
                && metadata.dev() == self.device
                && metadata.ino() == self.inode,
            "instance control socket changed; refusing connection"
        );
        anyhow::ensure!(
            self.pid > 0
                && self.pid <= i32::MAX as u32
                && unsafe { libc::kill(self.pid as i32, 0) } == 0,
            "instance control owner is no longer alive"
        );
        Ok(())
    }

    /// Connect only to the recorded socket identity and same-UID peer. No retries.
    /// Missing/dead/replaced owners are unavailable; protocol mismatch identifies
    /// the owner's executable. A successful connect alone admits no operation.
    pub fn connect(&self) -> anyhow::Result<std::os::unix::net::UnixStream> {
        self.check_identity()?;
        anyhow::ensure!(
            self.protocol == INSTANCE_CONTROL_PROTOCOL,
            "incompatible Job control protocol {}; owner binary: {} ({})",
            self.protocol,
            self.binary.display(),
            self.binary_digest
        );
        let stream = with_instance_socket(&self.path, |path| {
            std::os::unix::net::UnixStream::connect(path)
        })?;
        authorize_host_peer_sync(&stream)?;
        self.check_identity()?;
        Ok(stream)
    }
}

/// Synchronous same-effective-UID authentication, matching the async transport.
pub fn authorize_host_peer_sync(stream: &std::os::unix::net::UnixStream) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "macos")]
    {
        let (mut uid, mut gid) = (0, 0);
        anyhow::ensure!(
            unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0
                && uid == unsafe { libc::geteuid() },
            "unauthorized host peer"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&cred) as libc::socklen_t;
        anyhow::ensure!(
            unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut cred as *mut libc::ucred).cast(),
                    &mut length,
                )
            } == 0
                && cred.uid == unsafe { libc::geteuid() },
            "unauthorized host peer"
        );
    }
    Ok(())
}

fn frame_chunk_length(buffered: usize, chunk: &[u8]) -> anyhow::Result<(usize, bool)> {
    let newline = chunk.iter().position(|byte| *byte == b'\n');
    let length = newline.unwrap_or(chunk.len());
    anyhow::ensure!(
        length <= AGENTCTL_HOST_MAX_FRAME_BYTES - buffered,
        "host frame too large"
    );
    Ok((length, newline.is_some()))
}

/// Synchronous counterpart of `read_host_frame`, with identical JSON/bound rules.
/// Peek in chunks, then consume only through the newline: buffered readers can
/// swallow the next SCM_RIGHTS marker and discard its ancillary descriptors.
/// One reader must own the socket; callers impose deadlines and admission bounds.
pub fn read_host_frame_sync<T: DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
) -> anyhow::Result<T> {
    use std::{io::Read, os::fd::AsRawFd};
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                chunk.as_mut_ptr().cast(),
                chunk.len(),
                libc::MSG_PEEK,
            )
        };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        anyhow::ensure!(count != 0, "host connection closed before newline");
        let (length, newline) = frame_chunk_length(bytes.len(), &chunk[..count as usize])?;
        stream.read_exact(&mut chunk[..length + usize::from(newline)])?;
        bytes.extend_from_slice(&chunk[..length]);
        if newline {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}

pub fn write_host_frame_sync<T: Serialize>(
    stream: &mut std::os::unix::net::UnixStream,
    value: &T,
) -> anyhow::Result<()> {
    std::io::Write::write_all(stream, &encode_host_frame(value)?)?;
    Ok(())
}

/// Read exactly one bounded newline-delimited JSON frame, retaining pipelined bytes.
/// Endpoint owners impose their own I/O deadlines and admission bounds.
pub async fn read_host_frame<T: DeserializeOwned>(stream: &mut UnixStream) -> anyhow::Result<T> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        use std::os::fd::AsRawFd;
        stream.readable().await?;
        let count = match stream.try_io(tokio::io::Interest::READABLE, || {
            let count = unsafe {
                libc::recv(
                    stream.as_raw_fd(),
                    chunk.as_mut_ptr().cast(),
                    chunk.len(),
                    libc::MSG_PEEK,
                )
            };
            if count < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(count as usize)
            }
        }) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(count != 0, "host connection closed before newline");
        let (length, newline) = frame_chunk_length(bytes.len(), &chunk[..count])?;
        stream
            .read_exact(&mut chunk[..length + usize::from(newline)])
            .await?;
        bytes.extend_from_slice(&chunk[..length]);
        if newline {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}

/// Compact JSON plus one newline; the 1 MiB limit excludes that delimiter.
/// Bound serialization before growing the output or writing any socket bytes.
pub fn encode_host_frame<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > AGENTCTL_HOST_MAX_FRAME_BYTES - self.0.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "host frame too large",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bounded(Vec::new());
    serde_json::to_writer(&mut bytes, value)?;
    bytes.0.push(b'\n');
    Ok(bytes.0)
}

pub async fn write_host_frame<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> anyhow::Result<()> {
    stream.write_all(&encode_host_frame(value)?).await?;
    Ok(())
}

/// Fail closed when kernel peer credentials cannot establish same-effective-UID ownership.
pub fn authorize_host_peer(stream: &UnixStream) -> Result<(), AgentCtlHostError> {
    #[cfg(target_os = "macos")]
    let authorized = {
        use std::os::fd::AsRawFd;
        let mut uid = 0;
        let mut gid = 0;
        // Authorization needs the kernel's UID, not a live peer PID. Darwin's
        // LOCAL_PEERPID query can fail after the client disconnects even while
        // its authenticated request remains buffered in the socket.
        (unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 })
            && uid == unsafe { libc::geteuid() }
    };
    #[cfg(not(target_os = "macos"))]
    let authorized = stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == unsafe { libc::geteuid() });
    if authorized {
        Ok(())
    } else {
        Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Unauthorized,
            "host peer must belong to the effective user",
        ))
    }
}

/// A live endpoint must never route to a different Attempt, including after restore.
/// There is no independent generation in these endpoint records; reject supplied generations.
pub fn validate_host_target(
    target: Option<&AgentCtlTarget>,
    job: &str,
    attempt: &str,
) -> Result<(), AgentCtlHostError> {
    match target {
        Some(target)
            if target.job_id == job
                && target.attempt_id.as_deref() == Some(attempt)
                && target.generation.is_none() =>
        {
            Ok(())
        }
        _ => Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Conflict,
            "host control identity mismatch (stale Job/Attempt or generation)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_accepted_exchange_reports_uncertainty_without_replay() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let directory = allocate_host_directory("cancel-control-").unwrap();
        let path = directory.path().join("control.sock");
        let listener = bind_instance_socket(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let endpoint = LiveControlRecord {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            protocol: INSTANCE_CONTROL_PROTOCOL.into(),
            binary: std::env::current_exe().unwrap(),
            binary_digest: "owner".into(),
            pid: std::process::id(),
        };
        let (ready, accepted) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: pvisor_core::host_protocol::AgentCtlHostRequest<InstanceCommand> =
                read_host_frame(&mut stream).await.unwrap();
            ready.send(()).unwrap();
            let mut remaining = Vec::new();
            stream.read_to_end(&mut remaining).await.unwrap();
            assert!(remaining.is_empty());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let checked = cancelled.clone();
        let client = tokio::task::spawn_blocking(move || {
            endpoint.exchange_checked(
                "job",
                "attempt",
                InstanceCommand::Terminate {},
                Some(&|| {
                    anyhow::ensure!(!checked.load(Ordering::SeqCst), "caller cancelled");
                    Ok(())
                }),
            )
        });
        accepted.await.unwrap();
        cancelled.store(true, Ordering::SeqCst);
        let error = client.await.unwrap().unwrap_err().to_string();
        assert!(
            error.contains("uncertain") && error.contains("not retried"),
            "{error}"
        );
        server.await.unwrap();
    }

    #[test]
    fn authority_root_rejects_symlinks_public_modes_and_non_directories() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let root = parent.join("authority");
        create_authority_root(&root).unwrap();
        validate_authority_directory(&root).unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(create_authority_root(&root).is_err());
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        let link = parent.join("symlink");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(create_authority_root(&link).is_err());
        let file = parent.join("file");
        std::fs::write(&file, b"owner").unwrap();
        assert!(create_authority_root(&file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"owner");
    }

    #[test]
    fn allocations_share_service_root_and_do_not_remove_siblings() {
        let root = host_authority_root().unwrap();
        assert_eq!(
            root,
            std::fs::canonicalize("/tmp")
                .unwrap()
                .join(format!("pvisor-host-{}", unsafe { libc::geteuid() }))
        );
        let first = allocate_host_directory("vm-").unwrap();
        let second = allocate_host_directory("exec-").unwrap();
        assert_eq!(first.path().parent(), Some(root.as_path()));
        assert_eq!(second.path().parent(), Some(root.as_path()));
        drop(first);
        assert!(second.path().is_dir());
        assert!(root.is_dir());
    }

    #[test]
    fn sync_frames_preserve_boundaries_null_and_reject_trailing_json_and_eof() {
        use std::io::Write;
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        writer
            .write_all(b"null\n42\nnull true\n7\nnull\n\n8")
            .unwrap();
        assert_eq!(
            read_host_frame_sync::<serde_json::Value>(&mut reader).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(read_host_frame_sync::<u32>(&mut reader).unwrap(), 42);
        assert!(read_host_frame_sync::<serde_json::Value>(&mut reader).is_err());
        assert_eq!(read_host_frame_sync::<u32>(&mut reader).unwrap(), 7);
        assert!(read_host_frame_sync::<u32>(&mut reader).is_err()); // null is not a typed number
        assert!(read_host_frame_sync::<serde_json::Value>(&mut reader).is_err()); // empty frame
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(
            read_host_frame_sync::<u32>(&mut reader)
                .unwrap_err()
                .to_string()
                .contains("before newline")
        );
    }

    #[test]
    fn sync_limits_reject_before_extending_or_writing_and_accept_exact_bound() {
        use std::io::{Read, Write};
        // Both readers use this check before extending their bounded payload.
        assert!(frame_chunk_length(AGENTCTL_HOST_MAX_FRAME_BYTES, b"x").is_err());
        assert_eq!(
            frame_chunk_length(AGENTCTL_HOST_MAX_FRAME_BYTES, b"\nmarker").unwrap(),
            (0, true)
        );
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        assert!(
            write_host_frame_sync(&mut writer, &"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES)).is_err()
        );
        let mut byte = [0];
        assert!(matches!(
            reader.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        reader
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let sender = std::thread::spawn(move || {
            let exact = "x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES - 2);
            write_host_frame_sync(&mut writer, &exact).unwrap();
            let _ = writer.write_all(&vec![b'x'; AGENTCTL_HOST_MAX_FRAME_BYTES + 1]);
        });
        assert_eq!(
            read_host_frame_sync::<String>(&mut reader).unwrap().len(),
            AGENTCTL_HOST_MAX_FRAME_BYTES - 2
        );
        assert!(
            read_host_frame_sync::<serde_json::Value>(&mut reader)
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
        drop(reader);
        sender.join().unwrap();
    }

    #[tokio::test]
    async fn sync_and_async_frames_interoperate_in_both_directions() {
        let (mut sync, async_stream) = std::os::unix::net::UnixStream::pair().unwrap();
        async_stream.set_nonblocking(true).unwrap();
        let mut asynchronous = UnixStream::from_std(async_stream).unwrap();
        write_host_frame_sync(&mut sync, &serde_json::Value::Null).unwrap();
        write_host_frame_sync(&mut sync, &42).unwrap();
        assert_eq!(
            read_host_frame::<serde_json::Value>(&mut asynchronous)
                .await
                .unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(read_host_frame::<u32>(&mut asynchronous).await.unwrap(), 42);
        write_host_frame(&mut asynchronous, &"embedded\nnewline")
            .await
            .unwrap();
        write_host_frame(&mut asynchronous, &7).await.unwrap();
        assert_eq!(
            read_host_frame_sync::<String>(&mut sync).unwrap(),
            "embedded\nnewline"
        );
        assert_eq!(read_host_frame_sync::<u32>(&mut sync).unwrap(), 7);
    }

    #[tokio::test]
    async fn framing_preserves_next_frame_and_requires_newline_and_json() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.write_all(b"1\n2\n").await.unwrap();
        assert_eq!(read_host_frame::<u32>(&mut reader).await.unwrap(), 1);
        assert_eq!(read_host_frame::<u32>(&mut reader).await.unwrap(), 2);
        for frame in [b"\n".as_slice(), b"true false\n", b"bad\n"] {
            writer.write_all(frame).await.unwrap();
            assert!(
                read_host_frame::<serde_json::Value>(&mut reader)
                    .await
                    .is_err()
            );
        }
        writer.write_all(b"3").await.unwrap();
        writer.shutdown().await.unwrap();
        assert!(read_host_frame::<u32>(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn async_limits_accept_exact_bound_reject_oversized_frames_and_authenticate_peer() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        authorize_host_peer(&reader).unwrap();
        assert!(
            write_host_frame(&mut writer, &"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES))
                .await
                .is_err()
        );
        let sender = tokio::spawn(async move {
            // JSON quotes count towards the payload limit; the delimiter does not.
            let exact = "x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES - 2);
            write_host_frame(&mut writer, &exact).await.unwrap();
            assert!(
                write_host_frame(&mut writer, &"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES - 1))
                    .await
                    .is_err()
            );
            let _ = writer
                .write_all(&vec![b'x'; AGENTCTL_HOST_MAX_FRAME_BYTES + 1])
                .await;
        });
        assert_eq!(
            read_host_frame::<String>(&mut reader).await.unwrap().len(),
            AGENTCTL_HOST_MAX_FRAME_BYTES - 2
        );
        assert!(
            read_host_frame::<serde_json::Value>(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
        drop(reader);
        sender.await.unwrap();
    }

    #[test]
    fn live_target_requires_matching_job_explicit_attempt_and_no_generation() {
        let mut target = AgentCtlTarget {
            job_id: "job".into(),
            attempt_id: Some("attempt".into()),
            generation: None,
        };
        validate_host_target(Some(&target), "job", "attempt").unwrap();
        assert!(validate_host_target(Some(&target), "other", "attempt").is_err());
        assert!(validate_host_target(Some(&target), "job", "new-attempt").is_err());
        assert!(validate_host_target(None, "job", "attempt").is_err());
        target.generation = Some("old".into());
        assert!(validate_host_target(Some(&target), "job", "attempt").is_err());
        target.generation = None;
        target.attempt_id = None;
        assert!(validate_host_target(Some(&target), "job", "attempt").is_err());
    }
}
