use super::{JobSelection, RuntimeJobService, ServiceContext, check_selected_record};
use crate::RunBundle;

use crate::runtime::RunRecord;
use anyhow::Context;

pub struct ReviewRequest {
    pub job: JobSelection,
    pub checkpoint: Option<String>,
}
#[derive(Debug, serde::Serialize)]
pub struct ReviewContext {
    pub job_id: String,
    pub attempt_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub workspace_generation: Option<u64>,
    pub file_view: &'static str,
    pub execution_evidence: &'static str,
}
/// A retained Attempt has no committed completion, so review cannot claim a result.
/// Read-only status can reconcile an existing receipt; it cannot invent a missing one.
#[derive(Debug, serde::Serialize, thiserror::Error)]
#[error("Job {job_id} review is UNKNOWN: {reason}")]
pub struct ReviewUnavailable {
    /// Selected Job identity, checked against the locked current record.
    pub job_id: String,
    /// Selected Attempt identity, when the record carries one.
    pub attempt_id: Option<String>,
    /// Retained stage; no cleanup or workspace mutation is authorized by this error.
    pub stage: std::path::PathBuf,
    /// Why execution evidence cannot currently be reviewed.
    pub reason: &'static str,
}

pub struct ReviewResponse {
    pub record: RunRecord,
    pub bundle: RunBundle,
    pub context: ReviewContext,
    // Keep the chosen file view stable through frontend diff rendering.
    _lease: crate::runtime::registry::RunLease,
}
impl ReviewResponse {
    /// Borrow review data while this response retains the selected file-view lease.
    pub fn view(&self) -> (&RunRecord, &RunBundle, &ReviewContext) {
        (&self.record, &self.bundle, &self.context)
    }
}
impl RuntimeJobService {
    pub fn review(
        context: &ServiceContext<'_>,
        request: ReviewRequest,
    ) -> anyhow::Result<ReviewResponse> {
        let selected = Self::resolve(context, &request.job)?;
        let (mut record, _lease) = selected.lock_current()?;
        check_selected_record(&selected, &record)?;
        context.check(&record)?;
        if record.state == crate::RunRecordState::Unknown {
            return Err(ReviewUnavailable {
                job_id: record.run_id.clone(),
                attempt_id: record.attempt_id.clone(),
                stage: record.stage_dir(),
                reason: "Attempt completion is not committed; execution outcome is unknown",
            }
            .into());
        }
        let bundle_stage = record.stage_dir();
        let checkpoint_id = if let Some(id) = &request.checkpoint {
            let checkpoint = crate::runtime::checkpoint::resolve_checkpoint(&record, id)?;
            record = crate::runtime::checkpoint::workspace_view(&record, &checkpoint)?;
            Some(checkpoint.checkpoint_id)
        } else {
            record.require_stopped()?;
            None
        };
        let mut bundle = RunBundle::read(&bundle_stage).with_context(|| {
            format!(
                "Job {} has no readable Run Bundle; re-run it with this pVisor version",
                record.run_id
            )
        })?;
        // Refresh files after apply/drop while retaining the Bundle's execution evidence.
        if let Some(overlay) = &record.overlay {
            let status = crate::runtime::overlay_status(overlay)?;
            let filesystem = bundle
                .filesystem
                .as_mut()
                .context("Job Bundle has no workspace evidence")?;
            filesystem.state = overlay.state;
            filesystem.target = overlay.target.clone();
            filesystem.upper = overlay.upper.path().to_path_buf();
            filesystem.changed_files = status.changed_files;
            filesystem.whiteouts = status.whiteouts;
            filesystem.sample_paths = status.sample_paths;
            filesystem.changes = crate::runtime::overlay_changes(overlay, &record.overlay_lowers)?;
        }
        let review_context = ReviewContext {
            job_id: record.run_id.clone(),
            attempt_id: record.attempt_id.clone(),
            checkpoint_id,
            workspace_generation: record.overlay.as_ref().map(|overlay| overlay.generation),
            file_view: "staged_upper_with_external_lowers",
            execution_evidence: "historical_bundle",
        };
        Ok(ReviewResponse {
            record,
            bundle,
            context: review_context,
            _lease,
        })
    }
}
