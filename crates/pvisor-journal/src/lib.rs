//! Single-writer trace storage; public contracts live exclusively in [`api`].
#![deny(missing_docs)]

pub mod api;
mod journal;
mod persistence;
mod trace;
