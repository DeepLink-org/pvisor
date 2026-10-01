//! Shared control-plane contracts for pVisor, Gateway, OverlayFS, and OverlayNet.
//!
//! Runtime values, authorization policies, cooperative AgentCtl messages, and
//! recorded events share this dependency-light crate. Execution, transport
//! servers, and persistence remain owned by the runtime drivers.

#[cfg(unix)]
pub mod audit;
pub mod client;
mod file_access;
pub mod network;
pub mod overlay;
pub mod policy;
pub mod protocol;
pub mod run_plan;
pub mod runtime;
pub mod session;
mod time;
pub mod trace;

pub use client::{AgentCtlClient, AgentCtlClientConfig, AgentCtlResponseError};
pub use overlay::*;
pub use policy::*;
pub use protocol::*;
pub use runtime::*;
pub use time::unix_now_ms;

pub use network::{NetworkConfig, NetworkMode, NetworkPolicy};
pub use session::*;

pub mod gateway;
