use super::{
    ServiceContext, SessionSelection, SessionService, check_selected_record, validate_request_id,
};
use crate::SessionState;
use crate::runtime::RunRecord;
use crate::runtime::execution::SessionTransition;
use crate::{OverlayHint, RunConfig, VmExecutor};
use anyhow::Context;
use std::future::Future;
use std::path::PathBuf;

pub struct ResumeRequest {
    pub session: SessionSelection,
    pub request_id: Option<String>,
    pub eager_ram: bool,
}
#[derive(Debug, serde::Serialize)]
pub enum ResumeResponse {
    AlreadyAdmitted { stage: PathBuf },
    Finished { exit_code: i32 },
}
/// Native restore preparation for `start_with`, which owns captured-input
/// projection and durable Session startup/completion. The resume service retains the
/// previous Execution lease through launch completion and owns pre-start rollback.
pub struct RestoredExecution {
    pub config: RunConfig,
    pub spec: pvisor_core::RunSpec,
    pub stage: PathBuf,
    pub executor: VmExecutor,
    pub overlay: OverlayHint,
    pub checkpoint: pvisor_core::operation::ExecutionCheckpoint,
}
impl SessionService {
    pub async fn resume<F, Fut>(
        context: &ServiceContext<'_>,
        request: ResumeRequest,
        launch: F,
    ) -> anyhow::Result<ResumeResponse>
    where
        F: FnOnce(RestoredExecution) -> Fut,
        Fut: Future<Output = anyhow::Result<i32>>,
    {
        use crate::runtime::session_execution;
        let source = SessionService::resolve(context, &request.session)?;
        super::super::owned_workspace::reject_stage_reconstruction(&source.stage_dir())?;
        let request_id = request.request_id;
        let eager_ram = request.eager_ram;
        let template = session_execution::session(&source)?;
        let lease = template.lock()?;
        let mut session = template.current()?;
        session.validate_record_target(&source)?;
        let current_record = RunRecord::read(&session.active_stage)?;
        check_selected_record(&source, &current_record)?;
        context.check(&current_record)?;
        let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_request_id(&request_id)?;
        if let Some(request) = session.resumes.get(&request_id) {
            let stage = &request.stage;
            anyhow::ensure!(
                request.eager_ram == eager_ram,
                "REQUEST_ID_CONFLICT: resume RAM policy changed"
            );
            anyhow::ensure!(
                stage.join("run.json").is_file() && session.state() != SessionState::Restoring,
                "EXECUTION_UNKNOWN: resume request admitted without confirmed successor startup at {}",
                stage.display()
            );
            return Ok(ResumeResponse::AlreadyAdmitted {
                stage: stage.clone(),
            });
        }
        anyhow::ensure!(
            session.state() == SessionState::Suspended,
            "SESSION_BUSY: resume requires a confirmed suspended head (state={})",
            session.state()
        );
        super::require_execution(&source)?;
        let checkpoint =
            session.checkpoint(session.head.as_deref().context("missing suspended head")?)?;
        let (current_source, source_lease) = source.lock_current()?;
        check_selected_record(&source, &current_source)?;
        let source = current_source;
        session.validate_record_target(&source)?;
        context.check(&source)?;
        let stage = session
            .root
            .join("executions")
            .join(uuid::Uuid::new_v4().to_string());
        let (mut executor, mut overlay) =
            VmExecutor::restore(session.config.vm.clone(), checkpoint.clone(), &stage)?;
        executor = executor.with_features(session.config.features.clone())?;
        if eager_ram {
            executor.materialize_restore_ram(&stage)?;
        }
        preserve_apply_target(&source, &mut overlay);
        let previous = session.clone();
        session.apply(SessionTransition::RestoreAdmitted {
            id: &request_id,
            stage: &stage,
            eager_ram,
        })?;
        session.link_stage(&stage)?;
        session.write()?;
        drop(lease);
        // Keep the previous Execution's lease until the successor has completed.
        // The new Execution acquires its own lease through normal runtime admission.
        let result = launch(RestoredExecution {
            config: session.config.clone(),
            spec: session.spec.clone(),
            stage,
            executor,
            overlay,
            checkpoint,
        })
        .await;
        drop(source_lease);
        if result.is_err() {
            let _lease = session.lock()?;
            let mut current = session.current()?;
            let no_successor = current.owns_restore_transition(&session, &request_id)
                && !crate::runtime::is_live(&session.active_stage)?
                && !session.active_stage.join("run.json").try_exists()?;
            if current.apply(SessionTransition::RestoreAborted {
                admitted: &session,
                previous: &previous,
                id: &request_id,
                no_successor,
            })? {
                current.write()?;
            }
        }
        let exit_code = result?;
        // A launcher returning success without starting/draining the managed run
        // is not a completed restore. Do not turn an empty callback into Finished.
        confirm_restored_completion(&session.active_stage, &session.run_id)?;
        Ok(ResumeResponse::Finished { exit_code })
    }
}

pub(super) fn confirm_restored_completion(
    stage: &std::path::Path,
    session_id: &str,
) -> anyhow::Result<()> {
    let session = crate::Session::read_stage(stage)?
        .context("EXECUTION_UNKNOWN: restored launcher returned without a durable Session")?;
    let _lease = session.lock()?;
    let current = session.current()?;
    let record = RunRecord::read(stage)
        .context("EXECUTION_UNKNOWN: restored launcher returned without a successor record")?;
    anyhow::ensure!(
        current.run_id == session_id
            && record.run_id == session_id
            && current.active_stage == stage
            && !current.active_execution.is_empty()
            && record.execution_id.as_deref() == Some(current.active_execution.as_str())
            && matches!(
                current.state(),
                SessionState::Terminal | SessionState::Suspended
            )
            && record.state.is_stopped()
            && record.finished_at_unix_ms.is_some(),
        "EXECUTION_UNKNOWN: restored launcher returned without confirmed Session completion"
    );
    Ok(())
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
