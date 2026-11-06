//! Pure projections of validated completion evidence; storage validation stays in completion.
use super::super::RunRecord;
use anyhow::ensure;
use pvisor_core::{RunResult, RunState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Stored Execution outcome. Terminal shape alone is not completion or mutation authority;
/// receipt validation and the existing Session/Execution leases remain mandatory.
pub enum RunRecordState {
    /// Initial stored projection; a live observation is required to claim running.
    Running,
    /// Successful completion, trusted only after receipt validation.
    Completed,
    /// Cancelled completion, trusted only after receipt validation.
    Cancelled,
    /// Failed completion, trusted only after receipt validation.
    Failed,
    /// Explicit termination in a legacy record.
    Terminated,
    /// Confirmed native termination with continuation evidence.
    Hibernated,
    /// The owner stopped without a committed completion; retained effects are not approved.
    Unknown,
}

impl RunRecordState {
    /// Whether this is a terminal shape; does not validate completion evidence.
    pub fn is_stopped(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed | Self::Terminated | Self::Hibernated
        )
    }
    /// Stable stored spelling, independent of the live status display phase.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Terminated => "terminated",
            Self::Hibernated => "hibernated",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) fn record_state(state: RunState) -> anyhow::Result<RunRecordState> {
    Ok(match state {
        RunState::Completed => RunRecordState::Completed,
        RunState::Failed => RunRecordState::Failed,
        RunState::Cancelled => RunRecordState::Cancelled,
        RunState::Hibernated => RunRecordState::Hibernated,
        _ => anyhow::bail!("completion result is not terminal"),
    })
}

pub(crate) fn project_record(
    record: &mut RunRecord,
    result: Option<&RunResult>,
) -> anyhow::Result<()> {
    let (state, finished) = match result {
        Some(result) => {
            ensure!(
                record.run_id == result.run_id.as_str()
                    && record.execution_id.as_deref() == Some(result.execution_id.as_str()),
                "completion Execution identity mismatch"
            );
            (
                record_state(result.state)?,
                Some(result.finished_at_unix_ms),
            )
        }
        None => (RunRecordState::Unknown, None),
    };
    record.state = state;
    record.finished_at_unix_ms = finished;
    record.live_control = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version":3,"completion_protocol":2,"run_id":"session","execution_id":"execution","session_id":"session","agent":"test","pid":0,"command":["true"],"state":"running","storage":"/stage","started_at_unix_ms":1,"finished_at_unix_ms":9,"network":{},"gateway_listen":null,"overlay":null
        })).unwrap()
    }
    #[test]
    fn absent_and_foreign_receipts_cannot_confirm_or_partially_change_a_record() {
        let mut record = record();
        let mut result: RunResult = serde_json::from_value(serde_json::json!({
            "run_id":"session","execution_id":"foreign","state":"completed","started_at_unix_ms":1,"finished_at_unix_ms":3,"exit_code":0
        })).unwrap();
        let before = serde_json::to_value(&record).unwrap();
        assert!(project_record(&mut record, Some(&result)).is_err());
        assert_eq!(before, serde_json::to_value(&record).unwrap());
        result.execution_id = "execution".into();
        result.state = RunState::Running;
        assert!(project_record(&mut record, Some(&result)).is_err());
        assert_eq!(before, serde_json::to_value(&record).unwrap());
        result.state = RunState::Completed;
        project_record(&mut record, Some(&result)).unwrap();
        assert_eq!(record.state, RunRecordState::Completed);
        assert_eq!(record.finished_at_unix_ms, Some(3));
        project_record(&mut record, None).unwrap();
        assert_eq!(record.state, RunRecordState::Unknown);
        assert_eq!(record.finished_at_unix_ms, None);
        assert!(record.live_control.is_none());
    }
}
