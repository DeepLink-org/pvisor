//! Shared control-plane contracts for pVisor, Gateway, and OverlayNet.
//!
//! Runtime values, authorization policies, cooperative AgentCtl messages, and
//! recorded events share this dependency-light crate. Execution, transport
//! servers, and persistence remain owned by the runtime drivers.

pub mod client;
pub mod events;
pub mod policy;
pub mod protocol;
pub mod runtime;
mod time;

pub use client::{AgentCtlClient, AgentCtlClientConfig, AgentCtlResponseError};
pub use events::{EventIdentity, EventRecord, EventValidationError, unix_now_ms};
pub use policy::*;
pub use protocol::*;
pub use runtime::*;
