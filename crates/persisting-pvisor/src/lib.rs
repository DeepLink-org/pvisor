//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Hosts call [`PVisor::run`] directly; pVisor assembles execution, control,
//! network, filesystem, and the optional internal Gateway driver. Durable
//! Trace Event output uses the shared Journal when recording is enabled.

#![cfg_attr(all(target_os = "macos", target_arch = "x86_64"), allow(dead_code))]

pub mod cli;
pub mod core;
mod runtime;
pub mod trace;

mod config;
mod diagnostics;
mod executor;
mod image;

#[doc(hidden)]
pub use executor::sandbox;
#[cfg(unix)]
pub use image::cache;
mod util;

pub use config::{
    ContainerMount, ContainerNetwork, ContainerPlatform, ContainerSettings, FilesystemMode,
    GatewayDriverConfig, GatewayMode, GatewaySettings, NetworkDriverConfig, OverlayFsCommit,
    OverlayFsSettings, OverlayNetMode, OverlayNetPolicy, OverlayNetSettings, PVisorConfig,
    RecordSettings, RunConfig, RunExecutorKind, RunPolicy, RunSettings, RunStdio, VmSettings,
};
pub use executor::container::ContainerExecutor;
pub use executor::process::ProcessExecutor;
pub use executor::vm::VmExecutor;
pub use executor::vm::run_internal_if_requested as run_krun_internal_if_requested;
pub use executor::{AttemptContext, RunExecutor};
pub use persisting_control::{
    AGENTCTL_ENDPOINT_ENV, AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_TOKEN_ENV, AGENTCTL_TRANSPORT_ENV,
    AGENTCTL_VERSION, AGENTCTL_VERSION_ENV, AgentDirective, AgentErrorCode, AgentRequest,
    AgentResponse, AgentState, ControlController, ControlEffect, ControlMachine, ControlReason,
    ControlRequest, ControlState, ControlTransition, NetworkGuard, NetworkHostRule, NetworkRule,
    PolicyControlController, host_matches, is_public_egress_ip, normalize_host, parse_network_rule,
};
pub use persisting_gateway::sink::CaptureEventObserver as TrajectoryEventSink;
pub use runtime::agentctl::{
    AGENTCTL_MAX_SESSIONS, AgentClientSnapshot, AgentCtlControl, AgentCtlServer, AgentCtlSnapshot,
};
pub use runtime::bundle::{
    BundleArtifact, BundleRun, FilesystemSummary, NetworkSummary, RUN_BUNDLE_FILENAME,
    RUN_BUNDLE_SCHEMA_VERSION, ResourceSummary, RunBundle, SafetySummary,
};
pub use runtime::checkpoint::{
    CHECKPOINTS_DIR, CheckpointConsistency, LogicalCheckpoint, create_logical_checkpoint,
    latest_logical_checkpoint, restore_logical_checkpoint,
};
pub use runtime::event::{
    EventAppendErrorKind, EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher,
};
pub use runtime::run::{
    PVisor, PVisorBuilder, PVisorError, RunCancellation, RunEventStream, RunHandle,
};
pub use runtime::{
    ChangeEntry, ChangeEntryType, ChangeKind, ImplantPlan, OverlayHint, RunLineage,
    RuntimeCapabilities,
};
pub use util::unix_now_ms;
