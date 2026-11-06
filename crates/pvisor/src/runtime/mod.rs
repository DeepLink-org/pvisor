//! Run lifecycle, durable records, and pVisor-owned runtime driver coordination.
//!
//! pVisor assembles the optional Gateway/OverlayNet driver, network policy, and
//! embedded OverlayFS before the Agent process starts.

pub(crate) mod agentctl;
pub(crate) mod agentctl_client;
pub(crate) mod bundle;
pub mod checkpoint;
pub(crate) mod completion;
mod drivers;
pub(crate) mod event;
pub(crate) mod execution;
#[cfg(unix)]
pub mod host_transport;
mod implant;
#[cfg(unix)]
pub(crate) mod instance_control;
#[cfg(feature = "gateway")]
mod model_wait;
pub(crate) mod operation;
mod overlay;
pub(crate) mod owned_workspace;
mod proxy;
mod registry;
pub(crate) mod run;
pub mod session_execution;
pub mod session_service;
mod supervisor;
#[cfg(feature = "gateway")]
mod wait_coordinator;

pub(crate) use drivers::VmNetworkAttachment;
pub(crate) use drivers::{ExecutionTeardown, SessionDrivers};
pub(crate) use overlay::{
    prepare_overlay_record_mountless, save_lower_mutability, stage_overlay_record,
};
pub(crate) use registry::RunLease as StagedRunLease;
pub(crate) use supervisor::RuntimeSupervisor;
pub(crate) use supervisor::RuntimeSupervisorBuilder;

pub use implant::{ExecutionOverlayHint, ImplantPlan, OverlayHint};
#[cfg(all(test, target_os = "macos"))]
pub use overlay::apply_overlay;
pub use overlay::{
    ApplySelection, ChangeEntry, ChangeEntryType, ChangeKind, OverlayState, ReadOnlyOverlayMount,
    discard_overlay, load_apply_records, mount_overlay_record_read_only, overlay_changes,
    overlay_status, restore_overlay_upper, snapshot_overlay_upper,
};
#[cfg(test)]
pub use overlay::{OverlayRecord, OverlayUpper};
#[cfg(target_os = "linux")]
pub use registry::LEASE_FILENAME;
#[cfg(target_os = "linux")]
pub use registry::RunLease;
pub use registry::control_observations;
pub use registry::{
    EnvironmentProjection, RunLineage, RunLookupError, RunRecord, SessionDirectoryLock,
    control_mount_inspect, control_overlay_status, control_ping, control_unmount_inspect,
    default_run_home, is_live, lookup_run_id, resolve_run,
};
pub use supervisor::RuntimeCapabilities;

pub use execution::RunRecordState;
