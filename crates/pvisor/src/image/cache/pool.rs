//! Local V2 content pool using the same framed encoding as filesystem/S3.
//!
//! This is a separate namespace, identified by pool+file:// and pool-format.json.
//! V1 pool markers are explicitly rejected without modifying old objects.
use super::storage::{Storage, StoredObject};
use super::{CacheClient, Request, Response, backend::RemoteFs, direct::DirectImage};
use crate::image::oci::{ImageStore, PreparedImage};
use anyhow::ensure;
use object_store::PutMode;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

const MARKER: &[u8] =
    b"{\"version\":2,\"content_encoding\":\"zstd-seekable\",\"index\":\"pvisor-radix-v2\"}";

pub(crate) struct PoolStorage {
    files: Storage,
}
impl PoolStorage {
    pub(super) fn open(root: PathBuf, create: bool) -> anyhow::Result<Self> {
        let files = Storage::filesystem(root, create)?;
        if files.get("pool-format.json")?.is_none() && create {
            ensure!(
                files.get("format.json")?.is_none(),
                "cannot convert an existing cache into a V2 pool; republish into a new prefix"
            );
            files.put("pool-format.json", MARKER.to_vec(), true)?;
        }
        ensure!(
            files.get("pool-format.json")?.as_deref() == Some(MARKER),
            "unsupported local image pool format; V1 is retired, republish into a new V2 pool"
        );
        Ok(Self { files })
    }
    pub(super) fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
    ) -> anyhow::Result<Option<StoredObject>> {
        self.files.read(key, range, limit)
    }
    pub(super) fn write(&self, key: &str, bytes: Vec<u8>, mode: PutMode) -> anyhow::Result<()> {
        self.files.write(key, bytes, mode)
    }
}

pub(crate) fn publish_local_image(
    location: &Path,
    store: &ImageStore,
    image: &PreparedImage,
    reference: &str,
    architecture: &str,
) -> anyhow::Result<String> {
    let cache = super::portable::PortableCache::new(
        Storage::Pool(Arc::new(PoolStorage::open(location.into(), true)?)),
        None,
        false,
    );
    let (canonical, _) = crate::image::oci::cache_reference(reference)?;
    let response = cache.publish_prepared(store, image, architecture, &canonical)?;
    let Response::Prepared { image_handle, .. } = response else {
        anyhow::bail!("expected published image handle")
    };
    Ok(image_handle)
}

/// Look up an already published reference without resolving the registry.
/// Only absence is a miss; malformed or corrupt pool metadata remains an error.
pub(crate) fn published_local_image(
    location: &Path,
    reference: &str,
    architecture: &str,
) -> anyhow::Result<Option<Response>> {
    if !location.try_exists()? {
        return Ok(None);
    }
    super::portable::PortableCache::new(
        Storage::Pool(Arc::new(PoolStorage::open(location.into(), false)?)),
        None,
        true,
    )
    .published(reference, architecture)
}

pub(crate) fn open_local_image(
    location: &Path,
    handle: &str,
    architecture: &str,
    expected_digest: &str,
    scratch: &Path,
) -> anyhow::Result<DirectImage> {
    let client = CacheClient::configured_pool(location, true)?;
    let (response, _) = client.request(Request::Open {
        handle: handle.into(),
        architecture: architecture.into(),
    })?;
    let Response::Prepared {
        image_handle,
        digest,
        ..
    } = response
    else {
        anyhow::bail!("expected pooled image")
    };
    ensure!(
        image_handle == handle,
        "pool changed the pinned image revision"
    );
    ensure!(
        digest == expected_digest,
        "pool manifest does not match the pinned image"
    );
    // There is no persistent decoded-content or metadata mirror for a local pool.
    // Projection owns sparse placeholders and removes them on normal teardown.
    let source = RemoteFs::new(
        client,
        handle.into(),
        scratch.join("unused-block-cache"),
        None,
        false,
    )?;
    DirectImage::new(source, scratch)
}

#[cfg(test)]
mod tests;
