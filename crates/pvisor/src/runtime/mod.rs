//! Run lifecycle, durable records, and pVisor-owned runtime driver coordination.
//!
//! pVisor assembles the optional Gateway/OverlayNet driver, network policy, and
//! embedded OverlayFS before the Agent process starts.

pub(crate) mod agentctl;
pub(crate) mod agentctl_client;
mod attempt;
#[cfg(unix)]
pub(crate) mod audit;
pub(crate) mod bundle;
pub(crate) mod checkpoint;
pub(crate) mod event;
#[cfg(unix)]
pub mod host_transport;
mod implant;
#[cfg(unix)]
pub(crate) mod instance_control;
pub(crate) mod job_execution;
pub mod job_service;
pub(crate) mod operation;
mod overlay;
mod proxy;
mod registry;
pub(crate) mod run;
mod supervisor;
pub(crate) mod zcode;

pub(crate) use attempt::VmNetworkAttachment;
pub(crate) use attempt::{AttemptSession, AttemptTeardown};
pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::RuntimeSupervisorBuilder;

/// Apply application-specific process compatibility policies before the
/// Run's capabilities are validated and the executor prepares its sandbox.
pub(crate) fn apply_process_policies(
    spec: &mut pvisor_core::RunSpec,
    executor: &pvisor_core::ExecutorPlan,
) -> anyhow::Result<()> {
    if executor.kind == pvisor_core::ExecutorKind::Process
        && executor.isolation == pvisor_core::IsolationKind::RootlessProcess
    {
        zcode::apply_host_process_policy(spec)?;
    }
    Ok(())
}

pub use implant::{ExecutionOverlayHint, ImplantPlan, OverlayHint};
#[cfg(all(test, target_os = "macos"))]
pub use overlay::apply_overlay;
pub use overlay::{
    ApplySelection, ChangeEntry, ChangeEntryType, ChangeKind, OverlayState, ReadOnlyOverlayMount,
    apply_overlay_selected, discard_overlay, load_apply_records, mount_overlay_record_read_only,
    overlay_changes, overlay_status, restore_overlay_upper, snapshot_overlay_upper,
};
#[cfg(test)]
pub use overlay::{OverlayRecord, OverlayUpper};
#[cfg(target_os = "linux")]
pub(crate) use registry::LEASE_FILENAME;
#[cfg(test)]
pub use registry::RunLease;
pub use registry::control_observations;
pub use registry::{
    EnvironmentProjection, RunLineage, RunRecord, RunRecordState, control_mount_inspect,
    control_overlay_status, control_ping, control_unmount_inspect, default_run_home, is_live,
    resolve_run,
};
pub use supervisor::RuntimeCapabilities;
