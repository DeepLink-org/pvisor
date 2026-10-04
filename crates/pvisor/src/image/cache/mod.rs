//! Shared OCI cache with server, filesystem, and S3 storage backends.
//! See docs/src/zh/reference/shared-image-cache.md for storage layout and lifecycle.

mod cli;
mod client;
mod config;
mod portable;
pub mod progress;
mod protocol;
mod server;
mod storage;
pub(crate) use config::scrub_guest_environment;
mod transport;

pub use cli::{CacheArgs, run};
pub use client::CacheClient;
pub use config::{BACKEND_ENV, CacheBackend, CacheConfig, LOCATION_ENV, READ_ONLY_ENV};
pub use progress::ImageTotals;
use protocol::hash;
pub use protocol::{MAX_READ, Request, Response};
pub use transport::default_endpoint;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod lazy;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use lazy::{LazyMount, prepare_image};
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use lazy::{MountedImage, mount_image_handle};

pub const SERVER_ENV: &str = "PVISOR_CACHE_SERVER";

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}
