//! Native container catalog and lifecycle; the only public surface is [`api`].
#![deny(missing_docs)]
pub mod api;
mod images;
#[cfg(windows)]
mod native;
mod store;
#[cfg(any(windows, test))]
mod transport;
