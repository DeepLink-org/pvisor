//! Query projections for live and offline status. Frontends only render these values.
use super::{ExecutionSnapshot, RunRecordState};
use crate::job_service::{StatusAttempt, StatusResponse};
use anyhow::ensure;
use pvisor_core::RunState;
use serde::{Deserialize, Serialize};

/// Effective execution phase for status queries. Liveness is reported separately;
/// Paused means a live vCPU pause, while Hibernated means native execution ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusState {
    /// The Attempt has been admitted but startup has not begun.
    Created,
    /// Executor startup is in progress.
    Starting,
    /// A live owner confirms running execution.
    Running,
    /// A native suspension checkpoint awaits confirmed termination.
    Checkpointing,
    /// A live VM is paused; it still owns native resources.
    Paused,
    /// Native execution ended with a retained continuation checkpoint.
    Hibernated,
    /// Cancellation was observed and teardown is pending.
    Cancelling,
    /// A validated completion reports success.
    Completed,
    /// A validated completion reports failure.
    Failed,
    /// A validated completion reports cancellation.
    Cancelled,
    /// A legacy durable record confirms explicit termination.
    Terminated,
    /// No authoritative live or completed outcome is available.
    Unknown,
}
impl StatusState {
    /// Stable machine/text spelling shared by JSON and CLI rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Checkpointing => "checkpointing",
            Self::Paused => "paused",
            Self::Hibernated => "hibernated",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Terminated => "terminated",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn recorded(state: RunRecordState) -> Self {
        match state {
            RunRecordState::Running | RunRecordState::Unknown => Self::Unknown,
            RunRecordState::Completed => Self::Completed,
            RunRecordState::Failed => Self::Failed,
            RunRecordState::Cancelled => Self::Cancelled,
            RunRecordState::Terminated => Self::Terminated,
            RunRecordState::Hibernated => Self::Hibernated,
        }
    }
    fn live(state: RunState) -> Self {
        match state {
            RunState::Created => Self::Created,
            RunState::Starting => Self::Starting,
            RunState::Running => Self::Running,
            RunState::Checkpointing => Self::Checkpointing,
            RunState::Suspended => Self::Paused,
            RunState::Hibernated => Self::Hibernated,
            RunState::Cancelling => Self::Cancelling,
            RunState::Completed => Self::Completed,
            RunState::Failed => Self::Failed,
            RunState::Cancelled => Self::Cancelled,
        }
    }
}
impl StatusResponse {
    pub(crate) fn project_live(&mut self, snapshot: ExecutionSnapshot) -> anyhow::Result<()> {
        ensure!(
            snapshot.status.run_id.as_str() == self.record.run_id
                && Some(snapshot.status.attempt.attempt_id.as_str())
                    == self.record.attempt_id.as_deref(),
            "foreign or unavailable live status observation"
        );
        self.state = if snapshot.status.state.is_terminal() {
            StatusState::recorded(self.record.state)
        } else {
            StatusState::live(snapshot.status.state)
        };
        self.live_attempt = Some(StatusAttempt::from(&snapshot.status));
        self.execution_progress = Some(snapshot.progress);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_service::StatusRun;
    use crate::runtime::execution::{ControlOrigin, tests::execution};
    use pvisor_core::operation::{OperationKind, Value, VmState};
    fn response() -> StatusResponse {
        StatusResponse {
            schema_version: 2,
            state: StatusState::Unknown,
            record: StatusRun {
                run_id: "execution-test".into(),
                attempt_id: Some("attempt-test".into()),
                session_id: "session".into(),
                state: RunRecordState::Running,
                pid: 1,
                agent: "test".into(),
                command: vec!["true".into()],
                command_truncated: false,
                stage: "/stage".into(),
                started_at_unix_ms: 1,
                finished_at_unix_ms: None,
                gateway_listen: None,
                overlaynet_listen: None,
                network: "unspecified".into(),
                overlay: None,
            },
            live: true,
            live_attempt: None,
            execution_progress: None,
            execution_blocker: None,
            workspace_checkpoint_count: 0,
            apply_batch_count: 0,
            filesystem: None,
            filesystem_observation: None,
            network_observation: None,
            execution: None,
        }
    }
    #[tokio::test]
    async fn live_query_projects_pause_cancellation_and_cursor_from_one_snapshot() {
        let owner = execution();
        let mut response = response();
        response.project_live(owner.snapshot()).unwrap();
        assert_eq!(response.state, StatusState::Created); // Liveness alone is not Running.
        owner.ready().await;
        let id = owner
            .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
            .await
            .unwrap();
        owner.mark_dispatched(&id).await.unwrap();
        owner
            .succeeded(
                &id,
                Value::Vm {
                    state: VmState::Paused,
                    memory: None,
                },
            )
            .await
            .unwrap();
        let snapshot = owner.snapshot();
        let position = snapshot.progress.applied.clone();
        response.project_live(snapshot).unwrap();
        assert_eq!(response.state, StatusState::Paused);
        assert_eq!(response.state.as_str(), "paused");
        assert_eq!(
            response.live_attempt.as_ref().unwrap().state,
            RunState::Suspended
        );
        assert_eq!(
            response.execution_progress.as_ref().unwrap().applied,
            position
        );
        assert_eq!(serde_json::to_value(&response).unwrap()["state"], "paused");
        owner.cancelling().await;
        response.project_live(owner.snapshot()).unwrap();
        assert_eq!(response.state, StatusState::Cancelling);
    }
    #[tokio::test]
    async fn projection_rejects_foreign_snapshots_and_does_not_invent_completion() {
        let owner = execution();
        let mut response = response();
        let before = serde_json::to_value(&response).unwrap();
        let mut foreign = owner.snapshot();
        foreign.status.attempt.attempt_id = "foreign".into();
        assert!(response.project_live(foreign).is_err());
        assert_eq!(before, serde_json::to_value(&response).unwrap());
        owner.finish(RunState::Completed, None, 3).await;
        response.record.state = RunRecordState::Unknown;
        response.project_live(owner.snapshot()).unwrap();
        assert_eq!(response.state, StatusState::Unknown);
        response.record.state = RunRecordState::Completed;
        response.project_live(owner.snapshot()).unwrap();
        assert_eq!(response.state, StatusState::Completed);
    }
}
