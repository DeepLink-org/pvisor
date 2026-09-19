//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Hosts call [`PVisor::run`] directly; pVisor assembles execution, control,
//! network, filesystem, and the optional internal Gateway driver. Durable
//! EventRecord output uses local JSONL when recording is enabled.

pub mod cli;
pub mod core;
mod runtime;
pub mod trace;

mod agentctl;
mod artifact;
mod bundle;
mod checkpoint;
mod config;
mod container;
mod control;
mod delegated;
mod event;
mod executor;
mod firmware;
mod oci;
mod process;
mod pvisor;
#[doc(hidden)]
pub mod sandbox;
mod util;
mod vm;

pub use agentctl::{
    AGENTCTL_MAX_SESSIONS, AgentClientSnapshot, AgentCtlControl, AgentCtlServer, AgentCtlSnapshot,
};
pub use bundle::{
    BundleArtifact, BundleRun, FilesystemSummary, NetworkSummary, RUN_BUNDLE_FILENAME,
    RUN_BUNDLE_SCHEMA_VERSION, ResourceSummary, RunBundle, SafetySummary,
};
pub use checkpoint::{
    CHECKPOINTS_DIR, CheckpointConsistency, LogicalCheckpoint, create_logical_checkpoint,
    latest_logical_checkpoint, restore_logical_checkpoint,
};
pub use config::{
    ContainerMount, ContainerNetwork, ContainerPlatform, ContainerSettings, GatewayDriverConfig,
    GatewayMode, GatewaySettings, NetworkDriverConfig, OverlayFsBackend, OverlayFsCommit,
    OverlayFsSettings, OverlayNetMode, OverlayNetPolicy, OverlayNetSettings, PVisorConfig,
    RecordSettings, RunConfig, RunExecutorKind, RunPolicy, RunSettings, RunStdio, VmSettings,
};
pub use container::ContainerExecutor;
pub use control::{
    ControlController, ControlEffect, ControlMachine, ControlReason, ControlRequest, ControlState,
    ControlTransition, NetworkGuard, NetworkHostRule, NetworkRule, PolicyControlController,
    host_matches, is_public_egress_ip, normalize_host, parse_network_rule,
};
pub use event::{
    EventAppendErrorKind, EventSink, MemoryEventSink, NoopEventSink, RunEventPublisher,
};
pub use executor::{AttemptContext, RunExecutor};
pub use persisting_control::{
    AGENTCTL_ENDPOINT_ENV, AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_TOKEN_ENV, AGENTCTL_TRANSPORT_ENV,
    AGENTCTL_VERSION, AGENTCTL_VERSION_ENV, AgentDirective, AgentErrorCode, AgentRequest,
    AgentResponse, AgentState,
};
pub use persisting_gateway::sink::CaptureEventSink as TrajectoryEventSink;
pub use process::ProcessExecutor;
pub use pvisor::{PVisor, PVisorBuilder, PVisorError, RunCancellation, RunEventStream, RunHandle};
pub use runtime::{
    ChangeEntry, ChangeEntryType, ChangeKind, ImplantPlan, OverlayHint, RunLineage,
    RuntimeCapabilities,
};
pub use util::unix_now_ms;
pub use vm::VmExecutor;
pub use vm::run_internal_if_requested as run_krun_internal_if_requested;
