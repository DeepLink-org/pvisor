//! Executor contract, concrete backends, and their isolation helpers.

pub(crate) mod artifact;
pub(crate) mod container;
pub(crate) mod delegated;
#[cfg(target_os = "linux")]
pub(crate) mod host_exec;
pub(crate) mod process;
pub use crate::execution_context::lifecycle::ExecutorOutput;
pub(crate) use crate::execution_context::lifecycle::{SessionEnd, exit_outcome};
pub mod sandbox;
pub(crate) mod vm;

use async_trait::async_trait;
use pvisor_core::StdioMode;
use pvisor_core::{ExecutorPlan, RunInvocation};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Clone, Default)]
pub(crate) struct ExecutionAttachments {
    pub workspace_overlay: Option<crate::GuestOverlay>,
    pub filesystem: Option<pvisor_core::FileAccessPolicy>,
    pub vm_network: Option<Arc<std::sync::Mutex<Option<crate::runtime::VmNetworkAttachment>>>>,
    pub workspace_base: Option<Arc<crate::runtime::owned_workspace::OwnedWorkspace>>,
}

pub(crate) use crate::execution_context::ExecutionContext;

/// The production execution boundary: consumes the resolved RunSpec and controls.
/// Operation is an audit projection, not an arbitrary-expression dispatch API.
// async_trait adds #[must_use] to the already must-use boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RunExecutor: Send + Sync {
    fn descriptor(&self) -> ExecutorPlan;
    /// Capture this executor's actual VM settings and feature selection for durable
    /// continuation. None means no native Session continuation contract. Managed VM
    /// startup rejects None before starting drivers; bare Execution startup remains
    /// available. Wrappers must delegate this method to preserve continuation.
    fn continuation_settings(
        &self,
    ) -> Option<(crate::VmSettings, crate::features::FeatureSettings)> {
        None
    }
    fn supports(&self, invocation: &RunInvocation) -> bool;
    /// Whether this executor consumes pVisor's VM network attachment.
    ///
    /// A virtual-machine descriptor alone is not sufficient to claim that the
    /// Execution network is non-bypassable: pluggable executors must explicitly
    /// opt into the transport handoff contract. On true, consume the owner through
    /// `ExecutionContext::take_vm_network`, install its transport as the guest's
    /// network path and retain it through guest teardown. Backend policy alone is
    /// not installation evidence; report actual controls in ExecutorOutput.
    fn supports_vm_network_attachment(&self) -> bool {
        false
    }
    /// Consumes the guest workspace overlay instead of a host-mounted merged view.
    /// Executor names and VM isolation alone do not imply this handoff contract.
    /// On true, use `ExecutionContext::workspace_overlay` to install the prepared
    /// layers, exclusions, preimage journal and file policy. None means no workspace
    /// was requested. Stop all adapter/guest writes before execute returns; pVisor
    /// retains stage ownership and performs sealing and review/apply lifecycle.
    fn supports_guest_workspace_overlay(&self) -> bool {
        false
    }
    /// Explicit opt-in to native CPU QoS installation and executor observations.
    fn supports_cpu_qos(&self) -> bool {
        false
    }
    /// Execute the admitted request and join/stop owned workload and adapter IO
    /// before returning. Do not allocate a Session identity or publish completion.
    /// Driver attachments do not add native checkpoint/live-control support to a
    /// custom backend. Return observed controls, errors and cleanup warnings;
    /// ExecutionContext retains terminal publication and driver teardown ownership.
    async fn execute(&self, session: &crate::ExecutionContext) -> ExecutorOutput;
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
