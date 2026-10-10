//! Embeddable cross-platform FUSE overlay for pVisor.
//! All public contracts are exposed through [`api`]; FUSE adapters stay private.
#![deny(missing_docs)]

pub mod api;
mod cache;
mod dispatch;
mod fs;
mod mount;
mod observation;
