//! Shared OCI cache with server, filesystem, S3, static HTTP/HTTPS and IPFS storage.
//! See docs/src/zh/reference/shared-image-cache.md for storage layout and lifecycle.

mod backend;
mod client;
mod config;
mod direct;
mod http;
mod mirrors;
mod network;
mod pool;
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

pub use crate::env::cache::SERVER_ENV;

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use direct::DirectImage;
pub(crate) use direct::{RetainedImage, attach_runner_lowers, private_owner as direct_image_owner};
pub(crate) use network::run_internal_if_requested as run_image_access_internal;
pub(crate) use pool::{open_local_image, publish_local_image, published_local_image};
pub(crate) use portable::export_pool;
pub(crate) use repository::{
    publish_reference, resolve_repository, validate_name as validate_registry_name,
    validate_registries,
};
pub(crate) use repository::{publish_repository, read_repository_template, validate_destination};
mod repository;

/// Serve until process termination, notifying the owner after binding and worker startup.
/// The callback runs once before accepting connections. Its failure stops startup.
/// The caller owns process supervision; in-flight requests are not drained on termination.
pub fn serve(
    location: String,
    image_store: Option<std::path::PathBuf>,
    token: Option<String>,
    ready: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    server::serve(
        location,
        crate::image::store::LocalStore::discover(image_store)?,
        token,
        ready,
    )
}
