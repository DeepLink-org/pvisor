//! Rust implementation of agent-native sandbox replay.
//!
//! The default execution model assumes pVisor is already running inside a
//! fresh sandbox. Replay therefore touches only the selected workspace and
//! connects live Agents directly to their configured model endpoint.
//! Agent-specific bridges handle replay and continuation protocol adjustments.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod adapter;
pub mod api;
mod bridge;
mod comparison;
mod config;
mod engine;
mod error;
mod io;
mod journal;
mod model;
mod process;
