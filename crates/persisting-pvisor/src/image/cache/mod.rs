//! Shared read-only OCI cache: CLI, client, wire protocol, and server.
//! See docs/shared-image-cache.md for storage layout and lifecycle.

mod cli;
mod client;
pub mod progress;
mod protocol;
mod server;
mod transport;

pub use cli::{CacheArgs, run};
pub use client::CacheClient;
pub use progress::ImageTotals;
use protocol::hash;
pub use protocol::{MAX_READ, Request, Response};
pub use transport::default_endpoint;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod lazy;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use lazy::{LazyMount, prepare_image};

pub const SERVER_ENV: &str = "PERSISTING_PVISOR_CACHE_SERVER";

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}
