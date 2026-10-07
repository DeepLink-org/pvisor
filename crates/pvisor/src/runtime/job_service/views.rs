use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, execution_blocker,
};
use crate::RunBundle;

use crate::runtime::{
    RunRecord, control_observations, control_overlay_status, control_ping, is_live,
    load_apply_records, overlay_status,
};
use anyhow::Context;

pub struct StatusRequest {
    pub job: JobSelection,
}
#[derive(Debug, serde::Serialize)]
pub struct FilesystemSummary {
    pub changed_files: usize,
    pub whiteouts: usize,
    pub sample_paths: Vec<String>,
}
pub struct StatusResponse {
    pub record: RunRecord,
    pub live: bool,
    pub execution_blocker: Option<String>,
    pub checkpoints: Vec<crate::LogicalCheckpoint>,
    pub apply_history: Vec<pvisor_core::overlay::ApplyRecord>,
    pub filesystem: Option<FilesystemSummary>,
    pub filesystem_observation: Option<pvisor_core::operation::FilesystemObservation>,
    pub network_observation: Option<pvisor_overlaynet::InterceptionSnapshot>,
    /// Versioned durable execution state; not a CLI DTO.
    pub execution: Option<serde_json::Value>,
}
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
pub struct ReviewResponse {
    pub record: RunRecord,
    pub bundle: RunBundle,
    pub context: ReviewContext,
    // Keep the chosen file view stable through frontend diff rendering.
    pub(crate) lease: crate::runtime::registry::RunLease,
}
impl RuntimeJobService {
    pub fn status(
        context: &ServiceContext<'_>,
        request: StatusRequest,
    ) -> anyhow::Result<StatusResponse> {
        let record = Self::resolve(context, &request.job)?;
        let live = control_ping(&record.stage_dir()) || is_live(&record.stage_dir())?;
        let checkpoints = crate::runtime::checkpoint::list_checkpoints(&record)?;
        let blocker = execution_blocker(&record);
        let apply_history = load_apply_records(&record.stage_dir())?;
        let fs = record
            .overlay
            .as_ref()
            .map(|overlay| {
                if control_ping(&record.stage_dir()) {
                    control_overlay_status(&record.stage_dir()).map(|status| FilesystemSummary {
                        changed_files: status.changed_files,
                        whiteouts: status.whiteouts,
                        sample_paths: status.sample_paths,
                    })
                } else {
                    overlay_status(overlay)
                        .map(|status| FilesystemSummary {
                            changed_files: status.changed_files,
                            whiteouts: status.whiteouts,
                            sample_paths: status.sample_paths,
                        })
                        .map_err(Into::into)
                }
            })
            .transpose()?;
        let observations = if live {
            control_observations(&record.stage_dir()).ok()
        } else {
            None
        };
        let file_observed = observations
            .as_ref()
            .and_then(|value| value.get("filesystem"))
            .and_then(|value| {
                serde_json::from_value::<pvisor_core::operation::FilesystemObservation>(
                    value.clone(),
                )
                .ok()
            })
            .or_else(|| record.filesystem_observation.clone());
        let net_observed = observations
            .as_ref()
            .and_then(|value| value.get("network"))
            .and_then(|value| {
                serde_json::from_value::<pvisor_overlaynet::InterceptionSnapshot>(value.clone())
                    .ok()
            })
            .or_else(|| record.network_interception_metrics.clone());
        let execution = crate::runtime::job_execution::Job::read(&record)?.map(|job| serde_json::json!({"state":job.state,"suspended_head":job.head,"active_attempt":job.active_attempt,"job_root":job.root,"checkpoints":job.checkpoints,"requests":job.requests,"resume_requests":job.resumes,"fork_requests":job.forks,"checkpoint_stores":job.stores}));
        Ok(StatusResponse {
            record,
            live,
            execution_blocker: blocker,
            checkpoints,
            apply_history,
            filesystem: fs,
            filesystem_observation: file_observed,
            network_observation: net_observed,
            execution,
        })
    }
    pub fn review(
        context: &ServiceContext<'_>,
        request: ReviewRequest,
    ) -> anyhow::Result<ReviewResponse> {
        let selected = Self::resolve(context, &request.job)?;
        let (mut record, _lease) = selected.lock_current()?;
        check_selected_record(&selected, &record)?;
        context.check(&record)?;
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
        // The Bundle's execution evidence is historical. Read the selected file
        // view again so a preceding apply/drop cannot leave review showing old files.
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
            lease: _lease,
        })
    }
}
