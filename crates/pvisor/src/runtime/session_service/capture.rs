use super::{
    ServiceContext, SessionSelection, SessionService, execution_blocker, validate_request_id,
};
use crate::runtime::RunRecord;
use pvisor_core::operation::{ExecutionCheckpoint, SnapshotRamStorage};
use std::time::Duration;

pub struct CaptureRequest {
    pub session: SessionSelection,
    /// Suspend captures and terminates the current Execution; it is not live pause.
    pub suspend: bool,
    pub ram_storage: SnapshotRamStorage,
    pub request_id: Option<String>,
    pub timeout: Duration,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CaptureResponse {
    pub session_id: String,
    pub request_id: String,
    pub suspended: bool,
    pub checkpoint: ExecutionCheckpoint,
}
impl SessionService {
    pub async fn capture_execution(
        context: &ServiceContext<'_>,
        request: CaptureRequest,
    ) -> anyhow::Result<CaptureResponse> {
        let record = Self::resolve(context, &request.session)?;
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
        // Native capture owns the Session lock-time Execution rechecks, request receipts,
        // timeout ambiguity and confirmed suspended-head termination semantics.
        let checkpoint = crate::runtime::session_execution::capture(
            context,
            record,
            suspend,
            ram_storage,
            Some(request_id.clone()),
            timeout,
        )
        .await?;
        Ok(CaptureResponse {
            session_id: record.run_id.clone(),
            request_id,
            suspended: suspend,
            checkpoint,
        })
    }
}
pub fn require_execution(record: &RunRecord) -> anyhow::Result<()> {
    if let Some(blocker) = execution_blocker(record) {
        anyhow::bail!(
            "CAPABILITY_UNSUPPORTED: Session {}: {}",
            record.run_id,
            blocker
        );
    }
    Ok(())
}

impl SessionService {
    /// Delete an unreferenced native checkpoint under the Session operation lease.
    /// Returns false when no native checkpoint matches, permitting workspace lookup.
    /// Rechecks the selected Execution and caller admission before store mutation.
    /// Suspended heads and child references are refused. Store reader leases fence
    /// active restores. A persistence error after deletion requires reconciliation;
    /// admitted request IDs remain retained and cannot become fresh requests.
    pub fn delete_selected_execution_checkpoint(
        context: &ServiceContext<'_>,
        selected: &RunRecord,
        id: &str,
    ) -> anyhow::Result<bool> {
        let Some(template) = crate::Session::read(selected)? else {
            return Ok(false);
        };
        let _lease = template.lock()?;
        let mut session = template.current()?;
        session.validate_record_target(selected)?;
        let current = RunRecord::read(session.active_stage())?;
        super::check_selected_record(selected, &current)?;
        context.check(&current)?;
        let Some(capture) = session.checkpoints().get(id) else {
            return Ok(false);
        };
        session.check_deletable_checkpoint(id)?;
        let store = match session.config().vm.snapshot_filesystem_pool.as_deref() {
            Some(pool) => crate::environment_snapshot::SnapshotStore::with_filesystem_pool(
                &capture.checkpoint.store,
                pool,
            )?,
            None => crate::environment_snapshot::SnapshotStore::new(&capture.checkpoint.store)?,
        };
        store.delete(id)?;
        session.apply(crate::runtime::execution::SessionTransition::CheckpointDeleted { id })?;
        session.write()?;
        Ok(true)
    }
}
