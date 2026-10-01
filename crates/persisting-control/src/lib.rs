//! Shared control-plane contracts for pVisor, Gateway, OverlayFS, and OverlayNet.
//!
//! Runtime values, authorization policies, cooperative AgentCtl messages, and
//! recorded events share this dependency-light crate. Execution, transport
//! servers, and persistence remain owned by the runtime drivers.

#[cfg(unix)]
pub mod audit;
pub mod client;
mod file_access;
pub mod ir;
pub mod overlay;
pub mod policy;
pub mod protocol;
pub mod runtime;
mod time;
pub mod trace;

pub use client::{AgentCtlClient, AgentCtlClientConfig, AgentCtlResponseError};
pub use overlay::*;
pub use policy::*;
pub use protocol::*;
pub use runtime::*;
pub use time::unix_now_ms;
