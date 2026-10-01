//! Versioned execution Session control and observation contracts.
//! AgentCtl remains the separate cooperative protocol for workload clients.
use crate::{AttemptId, RunId, RunResult, RunStatus};
use serde::{Deserialize, Serialize};

pub const SESSION_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Preparing,
    Prepared,
    Executing,
    Executed,
    Finalizing,
    Committing,
    Finished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionObservation {
    pub version: u32,
    pub session: SessionIdentity,
    pub phase: SessionPhase,
    pub status: RunStatus,
    pub result: Option<RunResult>,
}

/// Controls target an exact Attempt; stale identities cannot affect a new Session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionControlRequest {
    pub version: u32,
    pub session: SessionIdentity,
    pub action: SessionControlAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionControlAction {
    Status,
    Cancel,
    Checkpoint {
        checkpoint_id: String,
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionControlResponse {
    Status {
        observation: Box<SessionObservation>,
    },
    CancellationRequested,
    Checkpoint {
        manifest: std::path::PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_wire_contract_is_versioned_and_rejects_unknown_fields() {
        let value = serde_json::json!({
            "version": SESSION_PROTOCOL_VERSION,
            "session": {"run_id":"run", "attempt_id":"attempt", "lease_epoch":7},
            "action": {"type":"checkpoint", "checkpoint_id":"boundary", "timeout_ms":50}
        });
        let request: SessionControlRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        let mut invalid = value;
        invalid["action"]["unexpected"] = true.into();
        assert!(serde_json::from_value::<SessionControlRequest>(invalid).is_err());
        assert_eq!(
            serde_json::to_value(SessionControlResponse::CancellationRequested).unwrap(),
            serde_json::json!({"type":"cancellation_requested"})
        );
    }
}
