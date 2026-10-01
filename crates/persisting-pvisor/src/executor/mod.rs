//! Executor contract, concrete backends, and their isolation helpers.

pub(crate) mod artifact;
pub(crate) mod container;
pub(crate) mod delegated;
pub(crate) mod process;
mod session;
pub use session::ExecutorOutput;
pub(crate) use session::{SessionEnd, exit_outcome};
pub mod sandbox;
pub(crate) mod vm;

use crate::runtime::event::RunEventPublisher;
use async_trait::async_trait;
use persisting_control::StdioMode;
use persisting_control::{AttemptId, ExecutorPlan, RunInvocation, RunSpec, RunState, RunStatus};
use serde_json::json;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct AttemptAttachments {
    pub filesystem: Option<persisting_control::FileAccessPolicy>,
    pub vm_network: Option<Arc<std::sync::Mutex<Option<crate::runtime::VmNetworkAttachment>>>>,
}

/// Attempt-scoped execution identity, controls, policy and lifecycle owner.
pub struct ExecutorSession {
    spec: Arc<RunSpec>,
    created_at_unix_ms: u64,
    network_policy: persisting_control::NetworkPolicy,
    attempt_id: AttemptId,
    cancel: CancellationToken,
    status: watch::Sender<RunStatus>,
    events: RunEventPublisher,
    agentctl: crate::AgentCtlControl,
    attachments: AttemptAttachments,
}

impl ExecutorSession {
    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    pub fn network_policy(&self) -> &persisting_control::NetworkPolicy {
        &self.network_policy
    }

    pub fn filesystem_policy(&self) -> Option<&persisting_control::FileAccessPolicy> {
        self.attachments.filesystem.as_ref()
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn take_vm_network(
        &self,
    ) -> anyhow::Result<Option<crate::runtime::VmNetworkAttachment>> {
        let Some(attachment) = &self.attachments.vm_network else {
            return Ok(None);
        };
        let mut attachment = attachment
            .lock()
            .map_err(|_| anyhow::anyhow!("VM network attachment lock poisoned"))?;
        Ok(attachment.take())
    }

    pub fn status(&self) -> RunStatus {
        self.status.borrow().clone()
    }

    fn events(&self) -> &RunEventPublisher {
        &self.events
    }

    pub(crate) fn import_delegated_agentctl(&self, snapshot: crate::AgentCtlSnapshot) {
        self.agentctl.import_delegated_snapshot(snapshot);
    }

    pub async fn transition(&self, state: RunState, message: impl Into<Option<String>>) {
        // Terminal publication is reserved for Session completion after resource teardown.
        if state.is_terminal() {
            return;
        }
        let now = crate::util::unix_now_ms();
        let message = message.into();
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            if matches!(state, RunState::Starting | RunState::Running)
                && status.attempt.started_at_unix_ms.is_none()
            {
                status.attempt.started_at_unix_ms = Some(now);
            }
            if state.is_terminal() {
                status.attempt.finished_at_unix_ms = Some(now);
            }
        });
        let _ = self
            .events
            .publish(
                "run.state_changed",
                "runtime",
                json!({
                    "state": state,
                    "message": message,
                }),
            )
            .await;
    }

    /// Make a terminal status visible after finalization and terminal-event commit.
    pub(crate) fn finish(&self, state: RunState, message: Option<String>, now: u64) {
        self.status.send_modify(|status| {
            status.state = state;
            status.updated_at_unix_ms = now;
            status.message = message.clone();
            status.attempt.finished_at_unix_ms = Some(now);
        });
    }
}

/// The production execution boundary: consumes the resolved RunSpec and controls.
/// RunPlan IR is an audit projection, not an arbitrary-expression dispatch API.
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
    async fn execute(&self, session: &ExecutorSession) -> ExecutorOutput;
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
