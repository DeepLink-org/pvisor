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
pub(crate) fn host_authority_root() -> anyhow::Result<std::path::PathBuf> {
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
pub(crate) fn validate_host_target(
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
    use pvisor_core::host_protocol::{
        AGENTCTL_HOST_VERSION, AgentCtlHostRequest, AgentCtlHostResponse,
    };

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
        writer.write_all(b"bad\n").await.unwrap();
        assert!(read_host_frame::<u32>(&mut reader).await.is_err());
        writer.write_all(b"3").await.unwrap();
        writer.shutdown().await.unwrap();
        assert!(read_host_frame::<u32>(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn bounded_frames_and_peer_credentials() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        authorize_host_peer(&reader).unwrap();
        assert!(
            write_host_frame(&mut writer, &"x".repeat(AGENTCTL_HOST_MAX_FRAME_BYTES))
                .await
                .is_err()
        );
        let sender = tokio::spawn(async move {
            let _ = writer
                .write_all(&vec![b'x'; AGENTCTL_HOST_MAX_FRAME_BYTES + 1])
                .await;
        });
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
    fn version_target_and_error_correlation() {
        let mut request = AgentCtlHostRequest {
            version: AGENTCTL_HOST_VERSION,
            request_id: "r".into(),
            target: Some(AgentCtlTarget {
                job_id: "job".into(),
                attempt_id: Some("attempt".into()),
                generation: None,
            }),
            command: (),
        };
        request.validate().unwrap();
        request.version += 1;
        assert_eq!(
            request.validate().unwrap_err().code,
            AgentCtlHostErrorCode::VersionMismatch
        );
        validate_host_target(request.target.as_ref(), "job", "attempt").unwrap();
        assert!(validate_host_target(request.target.as_ref(), "other", "attempt").is_err());
        assert!(validate_host_target(request.target.as_ref(), "job", "new-attempt").is_err());
        assert!(validate_host_target(None, "job", "attempt").is_err());
        request.target.as_mut().unwrap().generation = Some("old".into());
        assert!(validate_host_target(request.target.as_ref(), "job", "attempt").is_err());
        let response = AgentCtlHostResponse::<()> {
            version: AGENTCTL_HOST_VERSION,
            request_id: "r".into(),
            result: Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Unavailable,
                "uncertain",
            )),
        };
        response.validate("r").unwrap();
        assert_eq!(
            response.validate("other").unwrap_err().code,
            AgentCtlHostErrorCode::Conflict
        );
    }
}
