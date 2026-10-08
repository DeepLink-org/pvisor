//! Shared OCI cache with server, filesystem, and S3 storage backends.
//! See docs/src/zh/reference/shared-image-cache.md for storage layout and lifecycle.

mod backend;
mod client;
mod config;
mod direct;
mod network;
mod portable;
pub mod progress;
mod protocol;
mod s3_runtime;
mod server;
mod source;
pub(crate) mod storage;
pub(crate) use config::scrub_guest_environment;
mod transport;

pub use client::CacheClient;
pub use config::{BACKEND_ENV, CacheBackend, CacheConfig, LOCATION_ENV, READ_ONLY_ENV};
pub use progress::ImageTotals;
use protocol::hash;
pub use protocol::{MAX_READ, Request, Response};
pub use transport::default_endpoint;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod lazy;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use lazy::prepare_vm_image;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use lazy::{LazyImage, open_image_handle_for_host, open_image_handle_for_vm};

pub const SERVER_ENV: &str = "PVISOR_CACHE_SERVER";

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use direct::DirectImage;
pub(crate) use direct::{attach_runner_lowers, private_owner as direct_image_owner};
pub(crate) use network::run_internal_if_requested as run_image_access_internal;

/// Serve a cache using host-owned OCI staging and an optional transport token.
pub fn serve(
    location: String,
    image_store: Option<std::path::PathBuf>,
    token: Option<String>,
) -> anyhow::Result<()> {
    server::serve(
        location,
        crate::image::oci::ImageStore::new(image_store)?,
        token,
    )
}
