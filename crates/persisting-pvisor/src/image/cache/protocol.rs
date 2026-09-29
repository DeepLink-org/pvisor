//! Versioned requests, responses, and length-prefixed wire frames.
use super::ImageTotals;
use anyhow::ensure;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub(super) const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_READ: u32 = 1024 * 1024;

/// One request per connection. All paths are Unix bytes, relative to image root.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Ping,
    Prepare {
        image: String,
        architecture: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refresh: bool,
    },
    List {
        digest: String,
        path: Vec<u8>,
        offset: usize,
    },
    Stat {
        digest: String,
        path: Vec<u8>,
    },
    Read {
        digest: String,
        path: Vec<u8>,
        offset: u64,
        length: u32,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub(super) version: u32,
    pub(super) token: Option<String>,
    pub(super) request: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ready,
    Prepared {
        #[serde(default)]
        metadata_generation: Option<String>,
        #[serde(default)]
        totals: Option<ImageTotals>,
        digest: String,
        architecture: String,
        env: std::collections::BTreeMap<String, String>,
        entrypoint: Vec<String>,
        cmd: Vec<String>,
    },
    Entries {
        names: Vec<Vec<u8>>,
        /// Attributes aligned with names; absent on older servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Vec<Response>>,
        next_offset: Option<usize>,
    },
    Metadata {
        kind: String,
        size: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        inode: u64,
        nlink: u64,
        mtime: i64,
        mtime_nsec: i64,
        target: Option<Vec<u8>>,
    },
    Data {
        length: u32,
        sha256: String,
    },
    Error {
        code: String,
        message: String,
    },
}

pub(super) fn read_frame<T: DeserializeOwned>(stream: &mut impl Read) -> anyhow::Result<T> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid cache frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) fn write_frame(stream: &mut impl Write, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "cache response exceeds frame limit"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        crate::image::oci::encode_hex(&Sha256::digest(bytes))
    )
}
