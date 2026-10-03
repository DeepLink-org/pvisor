//! Cluster services independent of the host-local pVisor execution kernel.
pub mod admission;
pub mod client;
mod journal;
pub mod scheduler;
pub mod server;
pub use pvisor_core::cluster::*;
