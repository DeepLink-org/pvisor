//! V2 independent Zstandard frames, authenticated references and seekable footer.
//! The root authenticates encoded bytes before decoding; all sizes are bounded.
use super::*;
use bincode::{Decode, Encode};

pub(super) const PAGE_BYTES: usize = 64 * 1024;
pub(super) const MAX_FRAME_RAW: usize = MAX_READ as usize + 128;
pub(super) const MAX_FRAME_ENCODED: usize = MAX_FRAME_RAW + 4096;
pub(super) type Digest = [u8; 32];

/// Bounded per-key miss owners; the table lock never covers storage or decoding.
#[derive(Default)]
pub(super) struct Flights(Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>);
impl Flights {
    pub(super) fn acquire(&self, key: &str) -> anyhow::Result<Arc<Mutex<()>>> {
        let mut owners = self.0.lock().unwrap();
        if let Some(owner) = owners.get(key).and_then(std::sync::Weak::upgrade) {
            return Ok(owner);
        }
        owners.retain(|_, owner| owner.strong_count() > 0);
        ensure!(owners.len() < 128, "cache frame miss queue saturated");
        let owner = Arc::new(Mutex::new(()));
        owners.insert(key.into(), Arc::downgrade(&owner));
        Ok(owner)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrameRef {
    pub offset: u64,
    pub encoded: u32,
    pub decoded: u32,
    pub digest: Digest,
    pub count: u64,
}

pub(super) fn digest(bytes: &[u8]) -> Digest {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes).into()
}

pub(super) fn digest_string(bytes: &Digest) -> String {
    format!("sha256:{}", crate::util::encode_hex(bytes))
}

pub(super) fn parse_digest(value: &str) -> anyhow::Result<Digest> {
    let hex = crate::image::oci::digest_hex(value)?;
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(bytes)
}

pub(super) fn encode<T: Encode>(value: &T) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::encode_to_vec(value, bincode::config::standard())?)
}

pub(super) fn decode<T: Decode<()>>(bytes: &[u8]) -> anyhow::Result<T> {
    ensure!(bytes.len() <= MAX_FRAME_RAW, "decoded frame exceeds limit");
    let (value, consumed) = bincode::decode_from_slice(
        bytes,
        bincode::config::standard().with_limit::<MAX_FRAME_RAW>(),
    )?;
    ensure!(consumed == bytes.len(), "trailing frame bytes");
    Ok(value)
}

#[derive(Default)]
pub(super) struct Writer {
    pub bytes: Vec<u8>,
    sizes: Vec<(u32, u32)>,
}

impl Writer {
    pub fn push<T: Encode>(&mut self, value: &T, count: u64) -> anyhow::Result<FrameRef> {
        let raw = encode(value)?;
        ensure!(
            raw.len() <= MAX_FRAME_RAW,
            "frame exceeds decoded size limit"
        );
        let encoded = zstd::bulk::compress(&raw, 3)?;
        ensure!(
            encoded.len() <= MAX_FRAME_ENCODED,
            "frame exceeds encoded limit"
        );
        let reference = FrameRef {
            offset: self.bytes.len() as u64,
            encoded: encoded.len() as u32,
            decoded: raw.len() as u32,
            digest: digest(&encoded),
            count,
        };
        self.bytes.extend_from_slice(&encoded);
        self.sizes.push((reference.encoded, reference.decoded));
        ensure!(self.bytes.len() <= MAX_OBJECT, "frame object exceeds limit");
        Ok(reference)
    }

    pub fn finish(mut self) -> anyhow::Result<Vec<u8>> {
        // Standard seekable zstd: independent frames and a final skippable
        // seek table. Cryptographic authentication lives in parent references.
        self.bytes.extend_from_slice(&0x184d_2a5eu32.to_le_bytes());
        self.bytes
            .extend_from_slice(&(self.sizes.len() as u32 * 8 + 9).to_le_bytes());
        for (encoded, decoded) in &self.sizes {
            self.bytes.extend_from_slice(&encoded.to_le_bytes());
            self.bytes.extend_from_slice(&decoded.to_le_bytes());
        }
        self.bytes
            .extend_from_slice(&(self.sizes.len() as u32).to_le_bytes());
        self.bytes.push(0); // no non-cryptographic per-frame checksum
        self.bytes.extend_from_slice(&0x8f92_eab1u32.to_le_bytes());
        ensure!(
            self.bytes.len() <= MAX_OBJECT,
            "seekable object exceeds limit"
        );
        Ok(self.bytes)
    }
}

pub(super) fn validate(reference: &FrameRef, bytes: u64, limit: usize) -> anyhow::Result<()> {
    ensure!(
        reference.encoded > 0
            && reference.encoded as usize <= MAX_FRAME_ENCODED
            && reference.decoded > 0
            && reference.decoded as usize <= limit
            && reference
                .offset
                .checked_add(reference.encoded as u64)
                .is_some_and(|end| end <= bytes)
            && reference.count <= MAX_SPANS as u64,
        "invalid frame bounds"
    );
    Ok(())
}

pub(super) fn unpack<T: Decode<()>>(reference: &FrameRef, encoded: &[u8]) -> anyhow::Result<T> {
    ensure!(
        encoded.len() == reference.encoded as usize && digest(encoded) == reference.digest,
        "cache metadata page digest mismatch"
    );
    let raw = decompress(encoded, reference.decoded as usize)?;
    ensure!(
        raw.len() == reference.decoded as usize,
        "frame decoded length mismatch"
    );
    decode(&raw)
}

pub(super) fn decompress(encoded: &[u8], limit: usize) -> anyhow::Result<Vec<u8>> {
    ensure!(limit <= MAX_FRAME_RAW, "decompression exceeds frame limit");
    let mut decoder = zstd::bulk::Decompressor::new()?;
    decoder.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(21))?;
    Ok(decoder.decompress(encoded, limit)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seekable_footer_and_independent_frames_roundtrip() {
        let mut writer = Writer::default();
        let a = writer.push(&vec![1u8, 2, 3], 1).unwrap();
        let b = writer.push(&vec![4u8, 5], 1).unwrap();
        let bytes = writer.finish().unwrap();
        let footer = &bytes[bytes.len() - 33..];
        assert_eq!(&footer[..4], &0x184d_2a5eu32.to_le_bytes());
        assert_eq!(&footer[4..8], &25u32.to_le_bytes());
        assert_eq!(&footer[24..28], &2u32.to_le_bytes());
        assert_eq!(footer[28], 0);
        assert_eq!(&footer[29..], &0x8f92_eab1u32.to_le_bytes());
        assert_eq!(a.offset + a.encoded as u64, b.offset);
        assert_eq!(
            unpack::<Vec<u8>>(
                &b,
                &bytes[b.offset as usize..(b.offset + b.encoded as u64) as usize]
            )
            .unwrap(),
            [4, 5]
        );
        let raw = zstd::stream::decode_all(bytes.as_slice()).unwrap();
        assert_eq!(
            raw,
            [
                encode(&vec![1u8, 2, 3]).unwrap(),
                encode(&vec![4u8, 5]).unwrap()
            ]
            .concat()
        );
    }
    #[test]
    fn frame_references_authenticate_bounds_lengths_and_digest() {
        let mut writer = Writer::default();
        let reference = writer.push(&vec![7u8; 128], 1).unwrap();
        let bytes = writer.finish().unwrap();
        let mut corrupt = reference.clone();
        corrupt.offset = u64::MAX;
        assert!(validate(&corrupt, bytes.len() as u64, PAGE_BYTES).is_err());
        corrupt = reference.clone();
        corrupt.decoded = MAX_FRAME_RAW as u32 + 1;
        assert!(validate(&corrupt, bytes.len() as u64, MAX_FRAME_RAW).is_err());
        corrupt = reference.clone();
        corrupt.digest[0] ^= 1;
        assert!(unpack::<Vec<u8>>(&corrupt, &bytes[..reference.encoded as usize]).is_err());
        corrupt = reference.clone();
        corrupt.decoded += 1;
        assert!(unpack::<Vec<u8>>(&corrupt, &bytes[..reference.encoded as usize]).is_err());
    }
    #[test]
    fn miss_owners_coalesce_same_keys_and_bound_live_distinct_keys() {
        let flights = Flights::default();
        let first = flights.acquire("same").unwrap();
        assert!(Arc::ptr_eq(&first, &flights.acquire("same").unwrap()));
        assert!(!Arc::ptr_eq(&first, &flights.acquire("other").unwrap()));
        let held = (0..127)
            .map(|i| flights.acquire(&i.to_string()).unwrap())
            .collect::<Vec<_>>();
        assert!(flights.acquire("overflow").is_err());
        drop(held);
        assert!(flights.acquire("recovered").is_ok());
    }
}
