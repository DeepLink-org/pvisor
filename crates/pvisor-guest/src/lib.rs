//! Portable guest contracts; Linux supervisor implementation belongs to the binary.
#![deny(missing_docs)]

pub mod api;
mod control;
mod exit_status;
mod launch;
