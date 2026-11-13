//! Opt-in scheduling of ordinary OCI executions. Image preparation and VM
//! construction remain with their existing owner; this module only issues the
//! existing offload/resume controls at explicit safe request boundaries.
//!
//! The trusted owner must serialize every tool request through this scheduler,
//! keep automatic guest-idle controls disabled, and retain cleanup ownership.
//! A lost control acknowledgement retains both memory and writeback reservations.
pub mod api;
mod policy;
mod service;

pub use service::{exchange, serve};

#[cfg(test)]
mod tests;
