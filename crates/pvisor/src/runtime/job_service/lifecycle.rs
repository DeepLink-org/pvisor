use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, validate_request_id,
};
use crate::runtime::RunRecord;
use crate::runtime::job_execution::JobState;
use crate::{OverlayHint, RunConfig, VmExecutor};
use anyhow::Context;
use std::future::Future;
use std::path::PathBuf;

pub struct ResumeRequest {
    pub job: JobSelection,
    pub request_id: Option<String>,
    pub eager_ram: bool,
}
#[derive(Debug, serde::Serialize)]
pub enum ResumeResponse {
    AlreadyAdmitted { stage: PathBuf },
    Finished { exit_code: i32 },
}
/// Native restore preparation for `start_with`, which owns captured-input
/// projection and durable Job startup/completion. The resume service retains the
/// previous Attempt lease through launch completion and owns pre-start rollback.
pub struct RestoredAttempt {
    pub config: RunConfig,
    pub spec: pvisor_core::RunSpec,
    pub stage: PathBuf,
    pub executor: VmExecutor,
    pub overlay: OverlayHint,
    pub checkpoint: pvisor_core::operation::ExecutionCheckpoint,
}
impl RuntimeJobService {
    pub async fn resume<F, Fut>(
        context: &ServiceContext<'_>,
        request: ResumeRequest,
        launch: F,
    ) -> anyhow::Result<ResumeResponse>
    where
        F: FnOnce(RestoredAttempt) -> Fut,
        Fut: Future<Output = anyhow::Result<i32>>,
    {
        use crate::runtime::job_execution;
        let source = RuntimeJobService::resolve(context, &request.job)?;
        let request_id = request.request_id;
        let eager_ram = request.eager_ram;
        let template = job_execution::job(&source)?;
        let lease = template.lock()?;
        let mut job = template.current()?;
        job.validate_record_target(&source)?;
        let current_record = RunRecord::read(&job.active_stage)?;
        check_selected_record(&source, &current_record)?;
        context.check(&current_record)?;
        let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_request_id(&request_id)?;
        if let Some(request) = job.resumes.get(&request_id) {
            let stage = &request.stage;
            anyhow::ensure!(
                request.eager_ram == eager_ram,
                "REQUEST_ID_CONFLICT: resume RAM policy changed"
            );
            anyhow::ensure!(
                stage.join("run.json").is_file() && job.state != JobState::Restoring,
                "EXECUTION_UNKNOWN: resume request admitted without confirmed successor startup at {}",
                stage.display()
            );
            return Ok(ResumeResponse::AlreadyAdmitted {
                stage: stage.clone(),
            });
        }
        anyhow::ensure!(
            job.state == JobState::Suspended,
            "JOB_BUSY: resume requires a confirmed suspended head (state={})",
            job.state
        );
        super::require_execution(&source)?;
        let checkpoint = job.checkpoint(job.head.as_deref().context("missing suspended head")?)?;
        let (current_source, source_lease) = source.lock_current()?;
        check_selected_record(&source, &current_source)?;
        let source = current_source;
        job.validate_record_target(&source)?;
        context.check(&source)?;
        let stage = job
            .root
            .join("attempts")
            .join(uuid::Uuid::new_v4().to_string());
        let (mut executor, mut overlay) =
            VmExecutor::restore(job.config.vm.clone(), checkpoint.clone(), &stage)?;
        executor = executor.with_features(job.config.features.clone())?;
        if eager_ram {
            executor.materialize_restore_ram(&stage)?;
        }
        preserve_apply_target(&source, &mut overlay);
        let previous = job.clone();
        job.previous_stage = job.active_stage.clone();
        job.active_stage = stage.clone();
        job.state = JobState::Restoring;
        job.resumes.insert(
            request_id.clone(),
            job_execution::ResumeRequest {
                stage: stage.clone(),
                eager_ram,
            },
        );
        job.link_stage(&stage)?;
        job.write()?;
        drop(lease);
        // Keep the previous Attempt's lease until the successor has completed.
        // The new Attempt acquires its own lease through normal runtime admission.
        let result = launch(RestoredAttempt {
            config: job.config.clone(),
            spec: job.spec.clone(),
            stage,
            executor,
            overlay,
            checkpoint,
        })
        .await;
        drop(source_lease);
        if result.is_err() {
            let _lease = job.lock()?;
            let current = job.current()?;
            if owns_restore_transition(&current, &job, &request_id)
                && !crate::runtime::is_live(&job.active_stage)?
                && !job.active_stage.join("run.json").try_exists()?
            {
                // An error can occur after RunHandle acceptance too. Roll back only
                // our unchanged transition with no durable or live successor.
                previous.write()?;
            }
        }
        let exit_code = result?;
        // A launcher returning success without starting/draining the managed run
        // is not a completed restore. Do not turn an empty callback into Finished.
        confirm_restored_completion(&job.active_stage, &job.run_id)?;
        Ok(ResumeResponse::Finished { exit_code })
    }
}

pub(super) fn confirm_restored_completion(
    stage: &std::path::Path,
    job_id: &str,
) -> anyhow::Result<()> {
    let job = crate::runtime::job_execution::Job::read_stage(stage)?
        .context("EXECUTION_UNKNOWN: restored launcher returned without a durable Job")?;
    let _lease = job.lock()?;
    let current = job.current()?;
    let record = RunRecord::read(stage)
        .context("EXECUTION_UNKNOWN: restored launcher returned without a successor record")?;
    anyhow::ensure!(
        current.run_id == job_id
            && record.run_id == job_id
            && current.active_stage == stage
            && !current.active_attempt.is_empty()
            && record.attempt_id.as_deref() == Some(current.active_attempt.as_str())
            && matches!(current.state, JobState::Terminal | JobState::Suspended)
            && record.state.is_stopped()
            && record.finished_at_unix_ms.is_some(),
        "EXECUTION_UNKNOWN: restored launcher returned without confirmed Job completion"
    );
    Ok(())
}

pub(crate) fn owns_restore_transition(
    current: &crate::runtime::job_execution::Job,
    admitted: &crate::runtime::job_execution::Job,
    request_id: &str,
) -> bool {
    current.state == JobState::Restoring
        && current.run_id == admitted.run_id
        && current.root == admitted.root
        && current.active_attempt == admitted.active_attempt
        && current.active_stage == admitted.active_stage
        && current.previous_stage == admitted.previous_stage
        && current.head == admitted.head
        && matches!((serde_json::to_value(current), serde_json::to_value(admitted)),
            (Ok(current), Ok(admitted)) if current == admitted)
        && current
            .resumes
            .get(request_id)
            .is_some_and(|request| request.stage == admitted.active_stage)
}

pub(crate) fn preserve_apply_target(source: &RunRecord, overlay: &mut OverlayHint) {
    if let (Some(original), Some(saved)) = (&source.overlay, &mut overlay.execution_snapshot) {
        saved.target = original.target.clone();
        if saved.baseline_lower.is_none() {
            saved.baseline_lower = overlay.lower_dirs.last().cloned();
        }
        overlay.protect_target = original.protect_target;
    }
}
