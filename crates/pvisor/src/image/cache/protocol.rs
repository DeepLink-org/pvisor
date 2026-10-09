//! Versioned requests, responses, and length-prefixed wire frames.
use super::ImageTotals;
use anyhow::ensure;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub(super) const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_READ: u32 = 1024 * 1024;

/// All paths are Unix bytes, relative to image root. Envelope version 1 uses
/// one request per connection; version 2 permits sequential framed exchanges.
/// A `Read` or `Metadata` response frame is followed by exactly `Data.length` raw bytes before
/// the next request/response frame. There is no pipelining or protocol-level replay;
/// private pinned-read clients may retry a transport disconnect once per hop.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Ping,
    /// Open a published immutable revision without reading mutable HEAD.
    Open {
        handle: String,
        architecture: String,
    },
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
    /// Read immutable portable metadata, never file content or arbitrary keys.
    /// Supported in both envelope versions when Prepared advertises metadata_pages.
    /// Length must be 1..=MAX_READ; offset + length must not overflow. Offsets
    /// beyond the object are rejected, and reads crossing EOF are shortened.
    /// The Data frame is followed by its length of SHA-256 authenticated bytes.
    Metadata {
        /// Immutable revision handle; mutable image references are not accepted.
        handle: String,
        /// Exact whitelisted metadata basename, not a storage key or path.
        object_name: String,
        /// Byte offset within the named object, including its EOF boundary.
        offset: u64,
        /// Maximum number of bytes returned in the following Data body.
        length: u32,
    },
    Read {
        digest: String,
        path: Vec<u8>,
        offset: u64,
        length: u32,
    },
}

/// Version 1 closes after one exchange; version 2 retains the connection for
/// sequential exchanges until EOF, idle timeout, or a transport/framing failure.
/// Clients negotiate with version-2 `Ping` exchanges on a fresh connection,
/// confirming persistence before sending any actual request. Authorization is
/// checked on every envelope, and the version cannot change within a connection.
/// Responses keep the v1 wire shape.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    /// Connection semantics: 1 (single exchange) or 2 (persistent exchanges).
    pub(super) version: u32,
    pub(super) token: Option<String>,
    pub(super) request: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ready,
    Prepared {
        /// Immutable image/platform/revision handle used by every reader.
        image_handle: String,
        metadata_generation: String,
        /// This immutable handle supports Metadata reads in either envelope version.
        /// Missing capabilities from older services default to false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        metadata_pages: bool,
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
        /// Attributes aligned with names, required on every directory page.
        metadata: Vec<Response>,
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
    format!("sha256:{}", crate::util::encode_hex(&Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_pages_capability_defaults_false_and_is_optional_on_the_wire() {
        let old = serde_json::json!({
            "status": "prepared", "image_handle": "pvisor-v1:revision",
            "metadata_generation": "sha256:revision", "digest": "sha256:manifest",
            "architecture": "amd64", "env": {}, "entrypoint": [], "cmd": []
        });
        let response: Response = serde_json::from_value(old.clone()).unwrap();
        assert!(matches!(
            &response,
            Response::Prepared {
                metadata_pages: false,
                ..
            }
        ));
        assert!(
            serde_json::to_value(response)
                .unwrap()
                .get("metadata_pages")
                .is_none()
        );
        let mut capable = old;
        capable["metadata_pages"] = serde_json::json!(true);
        let response: Response = serde_json::from_value(capable).unwrap();
        assert!(matches!(
            &response,
            Response::Prepared {
                metadata_pages: true,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(response).unwrap()["metadata_pages"],
            true
        );
    }

    #[test]
    fn metadata_frames_and_data_bodies_work_in_both_envelope_versions() {
        for version in [1, 2] {
            let mut wire = Vec::new();
            write_frame(
                &mut wire,
                &Envelope {
                    version,
                    token: Some("secret".into()),
                    request: Request::Metadata {
                        handle: "pvisor-v1:revision".into(),
                        object_name: "index.bin".into(),
                        offset: 65536,
                        length: 65536,
                    },
                },
            )
            .unwrap();
            let envelope: Envelope = read_frame(&mut wire.as_slice()).unwrap();
            assert_eq!(envelope.version, version);
            assert!(matches!(
                envelope.request,
                Request::Metadata {
                    offset: 65536,
                    length: 65536,
                    ..
                }
            ));

            let body = b"metadata";
            wire.clear();
            write_frame(
                &mut wire,
                &Response::Data {
                    length: body.len() as u32,
                    sha256: hash(body),
                },
            )
            .unwrap();
            wire.extend_from_slice(body);
            write_frame(&mut wire, &Response::Ready).unwrap();
            let mut stream = wire.as_slice();
            let response: Response = read_frame(&mut stream).unwrap();
            assert!(
                matches!(response, Response::Data { length: 8, sha256 } if sha256 == hash(body))
            );
            let mut received = [0; 8];
            stream.read_exact(&mut received).unwrap();
            assert_eq!(&received, body);
            assert!(matches!(
                read_frame::<Response>(&mut stream).unwrap(),
                Response::Ready
            ));
        }
    }

    #[test]
    fn prepared_responses_require_an_immutable_handle_and_generation() {
        let complete = serde_json::json!({
            "status": "prepared", "image_handle": "pvisor-v1:revision",
            "metadata_generation": "sha256:revision", "digest": "sha256:manifest",
            "architecture": "amd64", "env": {}, "entrypoint": [], "cmd": []
        });
        serde_json::from_value::<Response>(complete.clone()).unwrap();
        for field in ["image_handle", "metadata_generation"] {
            let mut missing = complete.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Response>(missing).is_err());
            let mut null = complete.clone();
            null[field] = serde_json::Value::Null;
            assert!(serde_json::from_value::<Response>(null).is_err());
        }
    }
}
