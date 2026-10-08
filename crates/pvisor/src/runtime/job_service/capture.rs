use super::{
    JobSelection, RuntimeJobService, ServiceContext, execution_blocker, validate_request_id,
};
use crate::runtime::RunRecord;
use pvisor_core::operation::{ExecutionCheckpoint, SnapshotRamStorage};
use std::time::Duration;

pub struct CaptureRequest {
    pub job: JobSelection,
    /// Suspend captures and terminates the current Attempt; it is not live pause.
    pub suspend: bool,
    pub ram_storage: SnapshotRamStorage,
    pub request_id: Option<String>,
    pub timeout: Duration,
}
#[derive(Debug, serde::Serialize)]
pub struct CaptureResponse {
    pub job_id: String,
    pub request_id: String,
    pub suspended: bool,
    pub checkpoint: ExecutionCheckpoint,
}
impl RuntimeJobService {
    pub async fn capture_execution(
        context: &ServiceContext<'_>,
        request: CaptureRequest,
    ) -> anyhow::Result<CaptureResponse> {
        let record = Self::resolve(context, &request.job)?;
        Self::capture_selected_execution(
            context,
            &record,
            request.suspend,
            request.ram_storage,
            request.request_id,
            request.timeout,
        )
        .await
    }
    pub async fn capture_selected_execution(
        context: &ServiceContext<'_>,
        record: &RunRecord,
        suspend: bool,
        ram_storage: SnapshotRamStorage,
        request_id: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<CaptureResponse> {
        context.check(record)?;
        require_execution(record)?;
        let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_request_id(&request_id)?;
        // Native capture owns the Job lock-time Attempt rechecks, request receipts,
        // timeout ambiguity and confirmed suspended-head termination semantics.
        let checkpoint = crate::runtime::job_execution::capture(
            context,
            record,
            suspend,
            ram_storage,
            Some(request_id.clone()),
            timeout,
        )
        .await?;
        Ok(CaptureResponse {
            job_id: record.run_id.clone(),
            request_id,
            suspended: suspend,
            checkpoint,
        })
    }
}
pub fn require_execution(record: &RunRecord) -> anyhow::Result<()> {
    if let Some(blocker) = execution_blocker(record) {
        anyhow::bail!("CAPABILITY_UNSUPPORTED: Job {}: {}", record.run_id, blocker);
    }
    Ok(())
}
