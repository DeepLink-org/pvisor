//! Executor contract, concrete backends, and their isolation helpers.

pub(crate) mod artifact;
pub(crate) mod container;
pub(crate) mod delegated;
pub(crate) mod process;
pub use crate::session::lifecycle::ExecutorOutput;
pub(crate) use crate::session::lifecycle::{SessionEnd, exit_outcome};
pub mod sandbox;
pub(crate) mod vm;

use async_trait::async_trait;
use pvisor_control::StdioMode;
use pvisor_control::{ExecutorPlan, RunInvocation};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Clone, Default)]
pub(crate) struct AttemptAttachments {
    pub filesystem: Option<pvisor_control::FileAccessPolicy>,
    pub vm_network: Option<Arc<std::sync::Mutex<Option<crate::runtime::VmNetworkAttachment>>>>,
}

pub(crate) use crate::session::Session;

/// The production execution boundary: consumes the resolved RunSpec and controls.
/// RunPlan is an audit projection, not an arbitrary-expression dispatch API.
#[async_trait]
pub trait RunExecutor: Send + Sync {
    fn descriptor(&self) -> ExecutorPlan;
    fn supports(&self, invocation: &RunInvocation) -> bool;
    /// Whether this executor consumes pVisor's VM network attachment.
    ///
    /// A virtual-machine descriptor alone is not sufficient to claim that the
    /// Attempt network is non-bypassable: pluggable executors must explicitly
    /// opt into the transport handoff contract.
    fn supports_vm_network_attachment(&self) -> bool {
        false
    }
    async fn execute(&self, session: &crate::Session) -> ExecutorOutput;
}

#[derive(Debug)]
pub(crate) struct Captured {
    pub text: String,
    pub truncated: bool,
}

pub(crate) fn stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Inherit => Stdio::inherit(),
        StdioMode::Capture => Stdio::piped(),
        StdioMode::Null => Stdio::null(),
    }
}

pub(crate) async fn read_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Captured> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let keep = limit.saturating_sub(retained.len()).min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(Captured {
        text: String::from_utf8_lossy(&retained).into_owned(),
        truncated,
    })
}

pub(crate) async fn join_capture(
    task: Option<tokio::task::JoinHandle<std::io::Result<Captured>>>,
) -> Option<Captured> {
    match task {
        Some(task) => task.await.ok().and_then(Result::ok),
        None => None,
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn bounded_capture_drains_output_after_the_limit() {
        let (reader, mut writer) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move { writer.write_all(&[b'x'; 32768]).await.unwrap() });
        let captured =
            tokio::time::timeout(std::time::Duration::from_secs(2), read_limited(reader, 8))
                .await
                .unwrap()
                .unwrap();
        writer.await.unwrap();
        assert_eq!(captured.text, "xxxxxxxx");
        assert!(captured.truncated);
        let captured = read_limited(&b"ok"[..], 8).await.unwrap();
        assert_eq!(captured.text, "ok");
        assert!(!captured.truncated);
    }
}
