//! Validated build input for the embedded Rust native kernel.

use sha2::{Digest, Sha256};
use std::{io, path::Path};

const MAX_IMAGE_SIZE: u64 = 256 * 1024 * 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kernel: String,
    architecture: String,
    boot_protocol: String,
    guest_addr: u64,
    entry_addr: u64,
    size: u64,
    sha256: String,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn load(directory: &Path, architecture: &str) -> io::Result<(Vec<u8>, u64, u64)> {
    use std::io::Read;

    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut metadata = String::new();
    std::fs::File::open(directory.join("kernel.json"))?
        .take(4097)
        .read_to_string(&mut metadata)?;
    if metadata.len() > 4096 {
        return Err(invalid(
            "Rust native kernel metadata exceeds 4096 bytes".into(),
        ));
    }
    let metadata: Metadata =
        serde_json::from_str(&metadata).map_err(|error| invalid(error.to_string()))?;
    let layout = match architecture {
        "x86_64" => ("linux64", 0x0020_0000, 0x0020_1200),
        "aarch64" => ("linux-arm64", 0x8020_0000, 0x8020_0000),
        _ => {
            return Err(invalid(
                "unsupported Rust native kernel architecture".into(),
            ))
        }
    };
    if metadata.format != 1
        || metadata.kernel != "rust-native"
        || metadata.architecture != architecture
        || metadata.boot_protocol != layout.0
        || !(1..=MAX_IMAGE_SIZE).contains(&metadata.size)
        || !metadata.size.is_multiple_of(4096)
        || metadata.guest_addr != layout.1
        || metadata.entry_addr != layout.2
    {
        return Err(invalid(
            "unsupported Rust native kernel bundle layout or format".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(directory.join("kernel.bin"))?
        .take(metadata.size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.size
        || digest(&bytes) != metadata.sha256
        || metadata.entry_addr >= metadata.guest_addr + metadata.size
    {
        return Err(invalid(
            "Rust native kernel size, entry or SHA-256 mismatch".into(),
        ));
    }
    if architecture == "aarch64" {
        let image_size = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        if bytes[..4] != [0x10, 0, 0, 0x94]
            || bytes[8..16] != 0x20_0000u64.to_le_bytes()
            || !(64..=metadata.size).contains(&image_size)
            || bytes[24..32] != 2u64.to_le_bytes()
            || bytes[56..60] != *b"ARM\x64"
        {
            return Err(invalid("invalid ARM64 Linux Image header".into()));
        }
    }
    Ok((bytes, metadata.guest_addr, metadata.entry_addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(directory: &Path) {
        let bytes = vec![0xab; 8192];
        std::fs::write(directory.join("kernel.bin"), &bytes).unwrap();
        std::fs::write(
            directory.join("kernel.json"),
            serde_json::to_vec(&serde_json::json!({
                "format": 1,
                "kernel": "rust-native",
                "architecture": "x86_64",
                "boot_protocol": "linux64",
                "guest_addr": 0x0020_0000u64,
                "entry_addr": 0x0020_1200u64,
                "size": bytes.len(),
                "sha256": digest(&bytes),
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn bundle_retains_exact_bytes_and_addresses() {
        let directory = tempfile::tempdir().unwrap();
        bundle(directory.path());
        let (bytes, guest, entry) = load(directory.path(), "x86_64").unwrap();
        assert_eq!(bytes, vec![0xab; 8192]);
        assert_eq!(guest, 0x0020_0000);
        assert_eq!(entry, 0x0020_1200);
    }

    #[rstest::rstest]
    #[case::valid(None)]
    #[case::branch(Some(0))]
    #[case::text_offset(Some(8))]
    #[case::image_size(Some(23))]
    #[case::page_size(Some(24))]
    #[case::magic(Some(56))]
    fn arm_image_header_is_validated_even_with_a_matching_digest(#[case] corrupt: Option<usize>) {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = vec![0; 8192];
        bytes[..4].copy_from_slice(&[0x10, 0, 0, 0x94]);
        bytes[8..16].copy_from_slice(&0x20_0000u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&8192u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&2u64.to_le_bytes());
        bytes[56..60].copy_from_slice(b"ARM\x64");
        if let Some(offset) = corrupt {
            bytes[offset] ^= 0xff;
        }
        std::fs::write(directory.path().join("kernel.bin"), &bytes).unwrap();
        std::fs::write(
            directory.path().join("kernel.json"),
            serde_json::to_vec(&serde_json::json!({
                "format": 1,
                "kernel": "rust-native",
                "architecture": "aarch64",
                "boot_protocol": "linux-arm64",
                "guest_addr": 0x8020_0000u64,
                "entry_addr": 0x8020_0000u64,
                "size": bytes.len(),
                "sha256": digest(&bytes),
            }))
            .unwrap(),
        )
        .unwrap();
        let result = load(directory.path(), "aarch64");
        if corrupt.is_none() {
            let (loaded, guest, entry) = result.unwrap();
            assert_eq!(loaded, bytes);
            assert_eq!((guest, entry), (0x8020_0000, 0x8020_0000));
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        assert!(load(directory.path(), "x86_64").is_err());
        assert!(load(directory.path(), "riscv64").is_err());
    }

    #[rstest::rstest]
    #[case::corrupt_payload("kernel.bin", b"changed")]
    #[case::invalid_json("kernel.json", b"not json")]
    #[case::missing_contract("kernel.json", b"{}")]
    fn broken_bundle_never_loads(#[case] file: &str, #[case] content: &[u8]) {
        let directory = tempfile::tempdir().unwrap();
        bundle(directory.path());
        std::fs::write(directory.path().join(file), content).unwrap();
        assert!(load(directory.path(), "x86_64").is_err());
    }

    #[rstest::rstest]
    #[case::version("format", serde_json::json!(2))]
    #[case::linux("kernel", serde_json::json!("linux"))]
    #[case::architecture("architecture", serde_json::json!("aarch64"))]
    #[case::protocol("boot_protocol", serde_json::json!("multiboot2"))]
    #[case::load_address("guest_addr", serde_json::json!(0x1000))]
    #[case::entry_address("entry_addr", serde_json::json!(0x1000))]
    #[case::empty("size", serde_json::json!(0))]
    #[case::unaligned("size", serde_json::json!(8191))]
    #[case::too_large("size", serde_json::json!(MAX_IMAGE_SIZE + 4096))]
    #[case::hash("sha256", serde_json::json!("0".repeat(64)))]
    fn metadata_contract_is_checked_before_installation(
        #[case] field: &str,
        #[case] value: serde_json::Value,
    ) {
        let directory = tempfile::tempdir().unwrap();
        bundle(directory.path());
        let path = directory.path().join("kernel.json");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata[field] = value;
        std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert_eq!(
            load(directory.path(), "x86_64").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
