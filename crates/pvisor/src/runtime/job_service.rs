//! Durable Job operations shared by embedded and command-line frontends.
//!
//! Transport, terminal ownership and rendering belong to the caller. Context
//! checks run again under mutation leases; receipts remain scoped to operations.
use super::{RunRecord, job_execution::Job};
use pvisor_core::host_protocol::{AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlTarget};
use std::path::{Path, PathBuf};

mod capture;
mod effects;
mod fork;
pub use fork::{ExecutionForkRequest, ExecutionForkResponse};
mod lifecycle;
#[cfg(test)]
mod tests;
mod views;
pub(crate) use capture::require_execution;
pub use capture::{CaptureRequest, CaptureResponse};
#[cfg(test)]
pub(crate) use lifecycle::owns_restore_transition;
pub(crate) use lifecycle::preserve_apply_target;
pub use lifecycle::{RestoredAttempt, ResumeRequest, ResumeResponse};
mod restored;
pub use restored::ManagedRestoredRun;
mod workspace;
pub use effects::{ApplyRequest, DropRequest, MutationOutcome, MutationResponse};
pub use views::{
    FilesystemSummary, ReviewContext, ReviewRequest, ReviewResponse, StatusRequest, StatusResponse,
};
pub use workspace::{
    WorkspaceCreateRequest, WorkspaceCreateResponse, WorkspaceDeleteRequest, WorkspaceGcRequest,
};

/// Request-local admission hooks, never process-global runtime state.
/// `check_record` allows a frontend to retain its existing admitted-target fence
/// while migrating to `expected_target`; cancellation hooks must not acquire Job leases.
#[derive(Default)]
pub struct ServiceContext<'a> {
    pub expected_target: Option<&'a AgentCtlTarget>,
    pub check_cancelled: Option<&'a (dyn Fn() -> anyhow::Result<()> + Send + Sync)>,
    pub check_record: Option<&'a (dyn Fn(&RunRecord) -> anyhow::Result<()> + Send + Sync)>,
}
impl ServiceContext<'_> {
    pub fn check(&self, record: &RunRecord) -> anyhow::Result<()> {
        if let Some(check) = self.check_cancelled {
            check()?;
        }
        if let Some(target) = self.expected_target {
            check_target(target, record)?;
        }
        if let Some(check) = self.check_record {
            check(record)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct JobSelection {
    pub selector: Option<PathBuf>,
    pub storage: PathBuf,
}

/// Stateless facade. No CLI argument, output or transport dependencies.
pub struct RuntimeJobService;
impl RuntimeJobService {
    pub async fn start(
        visor: &crate::PVisor,
        spec: pvisor_core::RunSpec,
    ) -> Result<crate::RunHandle, crate::PVisorError> {
        visor.run(spec).await
    }

    pub fn resolve(
        context: &ServiceContext<'_>,
        selection: &JobSelection,
    ) -> anyhow::Result<RunRecord> {
        let storage = selection
            .storage
            .canonicalize()
            .unwrap_or_else(|_| selection.storage.clone());
        let record = super::resolve_run(selection.selector.as_deref(), &storage)?;
        context.check(&record)?;
        Ok(record)
    }
}

pub fn check_target(target: &AgentCtlTarget, record: &RunRecord) -> anyhow::Result<()> {
    target.validate()?;
    let generation = record.overlay.as_ref().map(|o| o.generation.to_string());
    if target.job_id != record.run_id
        || target.attempt_id != record.attempt_id
        || target.generation != generation
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Conflict,
            "stale Job, Attempt, or workspace generation",
        )
        .into());
    }
    Ok(())
}
pub fn check_selected_record(selected: &RunRecord, current: &RunRecord) -> anyhow::Result<()> {
    check_target(
        &AgentCtlTarget {
            job_id: selected.run_id.clone(),
            attempt_id: selected.attempt_id.clone(),
            generation: selected.overlay.as_ref().map(|o| o.generation.to_string()),
        },
        current,
    )
}
pub fn validate_request_id(id: &str) -> anyhow::Result<()> {
    pvisor_core::host_protocol::AgentCtlHostRequest {
        version: pvisor_core::host_protocol::AGENTCTL_HOST_VERSION,
        request_id: id.to_owned(),
        target: None,
        command: (),
    }
    .validate()?;
    if id.trim().is_empty() {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::InvalidRequest,
            "durable request id must not be blank",
        )
        .into());
    }
    Ok(())
}
pub(crate) fn lock_selected_job(
    context: &ServiceContext<'_>,
    record: &RunRecord,
) -> anyhow::Result<Option<(Job, impl Send + use<>)>> {
    let Some(template) = Job::read(record)? else {
        return Ok(None);
    };
    let lease = template.lock()?;
    let current = template.current()?;
    current.validate_record_target(record)?;
    let current_record = RunRecord::read(&current.active_stage)?;
    check_selected_record(record, &current_record)?;
    context.check(&current_record)?;
    Ok(Some((current, lease)))
}

pub fn execution_blocker(record: &RunRecord) -> Option<String> {
    let job = match super::job_execution::job(record) {
        Ok(job) => job,
        Err(error) => return Some(format!("{error:#}")),
    };
    if !cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )) {
        return Some("native execution checkpoints are unsupported on this platform".into());
    }
    if job.config.overlaynet.mode != crate::OverlayNetMode::Off {
        return Some("native execution checkpoints require --overlaynet off; the Job's networking is retained".into());
    }
    if job.config.vm.rootfs.as_deref() == Some(Path::new("/")) {
        return Some(
            "native execution checkpoints require an owned rootfs; host root is unsupported".into(),
        );
    }
    if job.config.vm.cold_pager_requested()
        || job.config.vm.ram_backing.is_some()
        || job.config.vm.ram_compression
    {
        return Some("native restore requires private RAM without live writable backing, a memory pool or cold pager".into());
    }
    None
}
