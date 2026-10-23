//! Local physical encoding for the existing immutable cache index and reader.
//!
//! This is a separate namespace, identified by pool+file:// and pool-format.json.
//! Ordinary filesystem/S3 V1 objects keep their original raw representation.
use super::storage::{Storage, StoredObject};
use super::{CacheClient, Request, Response, backend::RemoteFs, direct::DirectImage};
use crate::image::oci::{ImageStore, PreparedImage};
use anyhow::{Context, ensure};
use object_store::PutMode;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

const MARKER: &[u8] =
    b"{\"version\":1,\"content_encoding\":\"raw-or-zstd-v1\",\"max_decoded_bytes\":1048576}";
const MAGIC: &[u8; 8] = b"PVPOOL1\0";
const HEADER: usize = 13;
// Raw V1 readers/publishers must reject this prefix, even when opened directly
// as file://. The pool adapter supplies the existing logical index contract.
const PHYSICAL_FORMAT: &[u8] = b"{\"format_version\":1,\"encoding\":\"pvisor-local-pool-v1\",\"logical_metadata\":\"pvisor-paged-v1\",\"chunk_bytes\":1048576}";

pub(crate) struct PoolStorage {
    files: Storage,
}
impl PoolStorage {
    pub(super) fn open(root: PathBuf, create: bool) -> anyhow::Result<Self> {
        let files = Storage::filesystem(root, create)?;
        if files.get("pool-format.json")?.is_none() && create {
            ensure!(
                files.get("format.json")?.is_none()
                    || files.get("pool-format.json")?.as_deref() == Some(MARKER),
                "cannot convert a raw cache into a local pool"
            );
            files.put("pool-format.json", MARKER.to_vec(), true)?;
        }
        ensure!(
            files.get("pool-format.json")?.as_deref() == Some(MARKER),
            "unsupported local image pool format"
        );
        Ok(Self { files })
    }
    pub(super) fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
    ) -> anyhow::Result<Option<StoredObject>> {
        if key == "format.json" {
            ensure!(range.is_none(), "local pool format is a control object");
            let Some(mut object) = self.files.read(key, None, limit)? else {
                return Ok(None);
            };
            ensure!(
                object.bytes == PHYSICAL_FORMAT,
                "unsupported local pool index format"
            );
            object.bytes = super::portable::FORMAT.to_vec();
            ensure!(
                object.bytes.len() <= limit,
                "local pool format exceeds read limit"
            );
            return Ok(Some(object));
        }
        if !key.starts_with("data/") {
            return self.files.read(key, range, limit);
        }
        let Some(mut object) = self
            .files
            .read(key, None, super::MAX_READ as usize + HEADER)?
        else {
            return Ok(None);
        };
        let bytes = &object.bytes;
        ensure!(
            bytes.len() >= HEADER && &bytes[..8] == MAGIC,
            "invalid pool object header"
        );
        let length = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        ensure!(
            length > 0 && length <= super::MAX_READ as usize,
            "invalid pool decoded length"
        );
        let decoded = match bytes[8] {
            0 => bytes[HEADER..].to_vec(),
            1 => zstd::bulk::decompress(&bytes[HEADER..], length)
                .context("decode image pool object")?,
            _ => anyhow::bail!("unsupported pool object codec"),
        };
        ensure!(
            decoded.len() == length
                && super::hash(&decoded)[7..] == *key.rsplit('/').next().unwrap_or(""),
            "pool object digest or length mismatch"
        );
        object.bytes = if let Some(range) = range {
            ensure!(
                range.start <= range.end && range.end <= length as u64,
                "invalid pool object range"
            );
            decoded[range.start as usize..range.end as usize].to_vec()
        } else {
            decoded
        };
        ensure!(
            object.bytes.len() <= limit,
            "pool object exceeds read limit"
        );
        Ok(Some(object))
    }
    pub(super) fn write(&self, key: &str, bytes: Vec<u8>, mode: PutMode) -> anyhow::Result<()> {
        if key == "format.json" {
            ensure!(
                bytes == super::portable::FORMAT,
                "unsupported logical pool index format"
            );
            return self.files.write(key, PHYSICAL_FORMAT.to_vec(), mode);
        }
        if !key.starts_with("data/") {
            return self.files.write(key, bytes, mode);
        }
        ensure!(mode == PutMode::Create, "pool content is immutable");
        ensure!(
            !bytes.is_empty() && bytes.len() <= super::MAX_READ as usize,
            "invalid pool content length"
        );
        ensure!(
            super::hash(&bytes)[7..] == *key.rsplit('/').next().unwrap_or(""),
            "pool content digest mismatch"
        );
        if let Some(existing) = self.read(key, None, super::MAX_READ as usize)? {
            ensure!(
                existing.bytes == bytes,
                "existing pool object differs from source"
            );
            return Err(super::storage::conflict());
        }
        let compressed = zstd::bulk::compress(&bytes, 3)?;
        let (codec, payload) = if compressed.len() + 64 < bytes.len() {
            (1, compressed.as_slice())
        } else {
            (0, bytes.as_slice())
        };
        let mut encoded = Vec::with_capacity(HEADER + payload.len());
        encoded.extend_from_slice(MAGIC);
        encoded.push(codec);
        encoded.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        encoded.extend_from_slice(payload);
        self.files.write(key, encoded, mode)
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
