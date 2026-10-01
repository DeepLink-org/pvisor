//! OCI preparation and the shared image file cache.

#[cfg(unix)]
pub mod cache;
pub(crate) mod oci;
