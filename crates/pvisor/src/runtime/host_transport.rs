//! Host-only AgentCtl transport. No cooperative guest token grants host authority.
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_MAX_FRAME_BYTES, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlTarget,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

/// Read exactly one bounded newline-delimited JSON frame, retaining pipelined bytes.
/// Endpoint owners impose their own I/O deadlines and admission bounds.
pub(crate) async fn read_host_frame<T: DeserializeOwned>(
    stream: &mut UnixStream,
) -> anyhow::Result<T> {
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
        let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(count);
        anyhow::ensure!(
            bytes.len() + length <= AGENTCTL_HOST_MAX_FRAME_BYTES,
            "host frame too large"
        );
        stream
            .read_exact(&mut chunk[..length + usize::from(newline.is_some())])
            .await?;
        bytes.extend_from_slice(&chunk[..length]);
        if newline.is_some() {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}

pub(crate) async fn write_host_frame<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> anyhow::Result<()> {
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
    stream.write_all(&bytes.0).await?;
    Ok(())
}

/// Fail closed when kernel peer credentials cannot establish same-effective-UID ownership.
pub(crate) fn authorize_host_peer(stream: &UnixStream) -> Result<(), AgentCtlHostError> {
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
