//! Durable Session operations shared by embedded and command-line frontends.
//!
//! Transport, terminal ownership and rendering belong to the caller. Context
//! checks run again under mutation leases; receipts remain scoped to operations.
use super::RunRecord;
use crate::Session;
use pvisor_core::host_protocol::{AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlTarget};
use std::path::{Path, PathBuf};

mod apply_command;
pub use apply_command::{
    ApplyCommandExecution, ApplyCommandPicker, ApplyCommandRemaining, ApplyCommandRequest,
    ApplyCommandResponse, ApplyCommandScope, ApplyCommandSource, PROJECT_APPLY_SCOPE_KEY,
    ProjectApplyScope,
};
mod capture;
mod effects;
mod fork;
mod handoff;
pub use fork::{ExecutionForkRequest, ExecutionForkResponse};
pub use handoff::{ExportRequest, ExportResponse};
mod lifecycle;
pub mod paths;
pub mod policy;
#[cfg(test)]
mod tests;
mod views;
pub use capture::require_execution;
pub use capture::{CaptureRequest, CaptureResponse};

pub(crate) use lifecycle::preserve_apply_target;
pub use lifecycle::{RestoredExecution, ResumeRequest, ResumeResponse};
mod managed;
pub use managed::{PreparedSession, SessionRun};
mod restored;
mod workspace;
pub use effects::{DropRequest, MutationOutcome, MutationResponse};
pub use views::{ReviewContext, ReviewRequest, ReviewResponse, ReviewUnavailable};
pub(crate) mod status;
pub use status::{
    DeniedSample, ExecutionSummary, FilesystemActivity, FilesystemSummary, NetworkActivity,
    ObservationSource, StatusExecution, StatusOverlay, StatusRequest, StatusResponse, StatusRun,
};
pub use workspace::{
    WorkspaceCreateRequest, WorkspaceCreateResponse, WorkspaceDeleteRequest, WorkspaceGcRequest,
};

/// Request-local admission check for a selected Run record.
pub type RecordCheck<'a> = dyn Fn(&RunRecord) -> anyhow::Result<()> + Send + Sync + 'a;

/// Request-local admission hooks, never process-global runtime state.
/// `check_record` applies the caller's admission fence in addition to
/// `expected_target`; cancellation hooks must not acquire Session leases.
#[derive(Default)]
pub struct ServiceContext<'a> {
    pub expected_target: Option<&'a AgentCtlTarget>,
    pub check_cancelled: Option<&'a (dyn Fn() -> anyhow::Result<()> + Send + Sync)>,
    pub check_record: Option<&'a RecordCheck<'a>>,
}
impl ServiceContext<'_> {
    pub fn check(&self, record: &RunRecord) -> anyhow::Result<()> {
        crate::docker::Store::check_bound_staged_record(record)?;
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
pub struct SessionSelection {
    pub selector: Option<PathBuf>,
    pub storage: PathBuf,
}

/// Stateless facade. No CLI argument, output or transport dependencies.
pub struct SessionService;
impl SessionService {
    /// Start a bare Execution without installing a durable Session completion owner.
    /// Supervisors retain their own lifecycle; Session frontends use `start_managed`.
    pub async fn start(
        visor: &crate::PVisor,
        spec: pvisor_core::RunSpec,
    ) -> Result<crate::RunHandle, crate::PVisorError> {
        visor.run(spec).await
    }

    pub fn resolve(
        context: &ServiceContext<'_>,
        selection: &SessionSelection,
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
    if target.session_id != record.run_id
        || target.execution_id != record.execution_id
        || target.generation != generation
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Conflict,
            "stale Session, Execution, or workspace generation",
        )
        .into());
    }
    Ok(())
}
pub fn check_selected_record(selected: &RunRecord, current: &RunRecord) -> anyhow::Result<()> {
    check_target(
        &AgentCtlTarget {
            session_id: selected.run_id.clone(),
            execution_id: selected.execution_id.clone(),
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
pub fn lock_selected_session(
    context: &ServiceContext<'_>,
    record: &RunRecord,
) -> anyhow::Result<Option<(Session, impl Send + use<>)>> {
    lock_selected_session_mode(context, record, false)
}

// Preserve mutation lock order: Session directory -> ExecutionSession -> Execution lease.
// Observation takes the identical exclusive flock, but never initializes its file.
fn lock_selected_session_mode(
    context: &ServiceContext<'_>,
    record: &RunRecord,
    observe: bool,
) -> anyhow::Result<Option<(Session, super::registry::RunLease)>> {
    let Some(template) = Session::read(record)? else {
        return Ok(None);
    };
    let lease = if observe {
        super::registry::RunLease::observe_existing(&template.root().join("execution-operation"))?
    } else {
        template.lock()?
    };
    let current = template.current()?;
    current.validate_record_target(record)?;
    let current_record = RunRecord::read(current.active_stage())?;
    check_selected_record(record, &current_record)?;
    context.check(&current_record)?;
    Ok(Some((current, lease)))
}

pub fn execution_blocker(record: &RunRecord) -> Option<String> {
    if let Err(error) = super::owned_workspace::reject_stage_reconstruction(&record.stage_dir()) {
        return Some(format!("{error:#}"));
    }
    let session = match super::session_execution::session(record) {
        Ok(session) => session,
        Err(error) => return Some(format!("{error:#}")),
    };
    if session.config().vm.prepared_rootfs.is_some() {
        return Some(
            "native execution capture/restore is unsupported for prepared rootfs sources".into(),
        );
    }
    if !cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )) {
        return Some("native execution checkpoints are unsupported on this platform".into());
    }
    if session.config().overlaynet.mode != crate::OverlayNetMode::Off {
        return Some("native execution checkpoints require --overlaynet off; the Session's networking is retained".into());
    }
    if session.config().vm.rootfs.as_deref() == Some(Path::new("/")) {
        return Some(
            "native execution checkpoints require an owned rootfs; host root is unsupported".into(),
        );
    }
    if session.config().vm.cold_pager_requested()
        || session.config().vm.ram_backing.is_some()
        || session.config().vm.ram_compression
    {
        return Some("native restore requires private RAM without live writable backing, a memory pool or cold pager".into());
    }
    None
}
