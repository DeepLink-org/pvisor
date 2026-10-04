//! pVisor's owned Rust VM runtime.
//!
//! # Required API boundary
//!
//! * [`api`] is the **only** public module. Every other module is private.
//! * `api` contains interface declarations, exports and documented contracts;
//!   every public method signature is declared in an API trait, whose
//!   implementation lives in a private module. Private inherent impls must not
//!   introduce additional public methods. Opaque fields store state only.
//! * `api` must not use conditional compilation. Public data structures,
//!   fields, methods and signatures are identical across supported platforms,
//!   architectures, hardware backends and feature selections.
//! * Platform differences, backend dispatch, backend traits/generics and target-specific
//!   code remain internal. Consumers inspect uniform capabilities; unsupported
//!   operations report explicit errors instead of disappearing from the API.
//! * Document ownership, ordering, lifetime, threading, synchronization and
//!   failure-state guarantees, including snapshot publication/restore duties.
//! * Hardware/device tests requiring private access belong inside this crate;
//!   never expose implementation modules to accommodate an external test.
//!
//! # Core interface model
//!
//! [`api::VmConfiguration`] and [`api::VmRuntime`] are implemented by
//! [`api::VmBuilder`]. [`api::VmControl`] and [`api::SnapshotControl`] describe
//! [`api::VmmHandle`]; [`api::SnapshotCapture`] describes the lifetime-bounded
//! [`api::FrozenMachine`]. [`api::VmConfig`], [`api::Capabilities`] and the
//! snapshot/overlay records are defined in `api` with the same shape everywhere.
//!
//! See the crate README and [`api`] for the complete calling contract.
#[macro_use]
extern crate log;

include!("runtime_modules.rs");

#[cfg(test)]
mod contract_tests;
