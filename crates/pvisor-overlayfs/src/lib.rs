//! Embeddable cross-platform FUSE overlay for pVisor.
//! All public contracts are exposed through [`api`]; FUSE adapters stay private.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod api;
mod cache;
mod dispatch;
mod fs;
mod mount;
mod observation;
