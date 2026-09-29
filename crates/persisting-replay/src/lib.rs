//! Rust implementation of agent-native sandbox replay.
//!
//! The default execution model assumes pVisor is already running inside a
//! fresh sandbox. Replay therefore touches only the selected workspace and
//! connects live Agents directly to their configured model endpoint.
//! Agent-specific bridges handle replay and continuation protocol adjustments.

mod adapter;
mod bridge;
mod comparison;
mod config;
mod engine;
mod error;
mod io;
mod journal;
mod model;
mod process;

pub use config::{
    OverlayFsConfig, OverlayNetConfig, ReplayConfig, ReplayToml, RunConfig, request_from_json,
};
pub use engine::execute;
pub use error::{ReplayError, ReplayErrorKind};
pub use model::{
    AgentKind, AgentStatus, ExecutionReport, PlaybackRequest, RESULT_SCHEMA_VERSION, ReplayFailure,
    ReplayMode, ReplayPhase, ReplayQuality, ReplayResult,
};
