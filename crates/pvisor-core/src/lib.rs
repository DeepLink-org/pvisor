//! Operation contracts and external interaction definitions for pVisor, Gateway, OverlayFS, and OverlayNet.
//!
//! Operation requests, placements, outcomes, authorization policies, AgentCtl messages, and
//! recorded events share this dependency-light crate. Execution, transport
//! servers, and persistence remain owned by the runtime drivers.

#[cfg(unix)]
pub mod audit;
pub mod cluster;
pub mod event;
pub mod execution;
mod file_access;
pub mod network;
pub mod operation;
pub mod overlay;
pub mod policy;
pub mod protocol;
pub mod session;
mod time;

pub use execution::*;
pub use overlay::*;
pub use policy::*;
pub use protocol::*;
pub use time::unix_now_ms;

pub use network::{NetworkConfig, NetworkMode, NetworkPolicy};
pub use session::*;

pub mod gateway;

pub use event::{Event, Fact};
pub use operation::{
    Operation, OperationDecision, OperationKind, OperationObservation, Outcome, Placement,
    VmMemory, VmState,
};
