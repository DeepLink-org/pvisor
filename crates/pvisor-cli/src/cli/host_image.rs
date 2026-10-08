//! Attest the on-disk Mach-O against the image dyld actually loaded before hashing.
//! Pathname/inode identity alone cannot detect replacement before the first Hello.
use anyhow::{Context, ensure};
use std::io::{Read, Seek, SeekFrom};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const LC_UUID: u32 = 0x1b;

fn word(bytes: &[u8], offset: usize, little: bool) -> anyhow::Result<u32> {
    let value: [u8; 4] = bytes
        .get(offset..offset + 4)
        .context("truncated Mach-O word")?
        .try_into()?;
    Ok(if little {
        u32::from_le_bytes(value)
    } else {
        u32::from_be_bytes(value)
    })
}

fn header(bytes: &[u8]) -> anyhow::Result<(bool, usize)> {
    match bytes.get(..4).context("truncated Mach-O header")? {
        [0xcf, 0xfa, 0xed, 0xfe] => Ok((true, 32)),
        [0xce, 0xfa, 0xed, 0xfe] => Ok((true, 28)),
        [0xfe, 0xed, 0xfa, 0xcf] => Ok((false, 32)),
        [0xfe, 0xed, 0xfa, 0xce] => Ok((false, 28)),
        _ => anyhow::bail!("executable is not a supported Mach-O image"),
    }
}

fn image_uuid(bytes: &[u8], cpu: u32) -> anyhow::Result<[u8; 16]> {
    let (little, size) = header(bytes)?;
    ensure!(
        word(bytes, 4, little)? == cpu,
        "Mach-O CPU differs from loaded image"
    );
    let count = word(bytes, 16, little)? as usize;
    let length = word(bytes, 20, little)? as usize;
    ensure!(
        length <= MAX_COMMAND_BYTES && count <= length / 8,
        "invalid Mach-O command bounds"
    );
    let end = size + length;
    ensure!(bytes.len() >= end, "truncated Mach-O commands");
    let mut offset = size;
    let mut uuid = None;
    for _ in 0..count {
        ensure!(offset + 8 <= end, "truncated Mach-O command");
        let command = word(bytes, offset, little)?;
        let size = word(bytes, offset + 4, little)? as usize;
        ensure!(
            size >= 8 && size <= end - offset,
            "invalid Mach-O command size"
        );
        if command == LC_UUID {
            ensure!(
                size == 24 && uuid.is_none(),
                "invalid or duplicate Mach-O UUID"
            );
            uuid = Some(bytes[offset + 8..offset + 24].try_into()?);
        }
        offset += size;
    }
    ensure!(offset == end, "Mach-O command size mismatch");
    uuid.context("loaded executable has no LC_UUID; refusing unattested Job admission")
}

fn disk_uuid(file: &mut (impl Read + Seek), cpu: u32) -> anyhow::Result<[u8; 16]> {
    file.seek(SeekFrom::Start(0))?;
    let mut prefix = [0; 8];
    file.read_exact(&mut prefix)?;
    let fat = match &prefix[..4] {
        [0xca, 0xfe, 0xba, 0xbe] => Some((false, false)),
        [0xbe, 0xba, 0xfe, 0xca] => Some((true, false)),
        [0xca, 0xfe, 0xba, 0xbf] => Some((false, true)),
        [0xbf, 0xba, 0xfe, 0xca] => Some((true, true)),
        _ => None,
    };
    let (mut start, mut slice_size) = (0u64, u64::MAX);
    if let Some((little, wide)) = fat {
        let count = word(&prefix, 4, little)? as usize;
        ensure!(
            (1..=64).contains(&count),
            "invalid universal Mach-O architecture count"
        );
        let stride = if wide { 32 } else { 20 };
        let mut arches = vec![0; count * stride];
        file.read_exact(&mut arches)?;
        let mut selected = None;
        for arch in arches.chunks_exact(stride) {
            if word(arch, 0, little)? != cpu {
                continue;
            }
            ensure!(selected.is_none(), "ambiguous universal Mach-O CPU");
            let pair = if wide {
                let read = |offset| -> anyhow::Result<u64> {
                    let bytes: [u8; 8] = arch[offset..offset + 8].try_into()?;
                    Ok(if little {
                        u64::from_le_bytes(bytes)
                    } else {
                        u64::from_be_bytes(bytes)
                    })
                };
                (read(8)?, read(16)?)
            } else {
                (
                    word(arch, 8, little)? as u64,
                    word(arch, 12, little)? as u64,
                )
            };
            ensure!(
                pair.0 >= (8 + arches.len()) as u64,
                "Mach-O slice overlaps architecture table"
            );
            selected = Some(pair);
        }
        (start, slice_size) = selected.context("on-disk Mach-O has no loaded CPU slice")?;
    }
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; 28];
    file.read_exact(&mut bytes)?;
    let (little, header_size) = header(&bytes)?;
    let length = word(&bytes, 20, little)? as usize;
    ensure!(
        length <= MAX_COMMAND_BYTES && (header_size + length) as u64 <= slice_size,
        "invalid on-disk Mach-O bounds"
    );
    bytes.resize(header_size + length, 0);
    file.read_exact(&mut bytes[28..])?;
    image_uuid(&bytes, cpu)
}

#[cfg(target_os = "macos")]
pub(super) fn attest(file: &mut std::fs::File) -> anyhow::Result<()> {
    unsafe extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const u8;
    }
    let pointer = unsafe { _dyld_get_image_header(0) };
    ensure!(!pointer.is_null(), "dyld has no loaded executable image");
    // The OS loader owns this mapping for process lifetime. Bound the command
    // region before creating a slice; only read the trusted main-image header.
    let loaded = unsafe { std::slice::from_raw_parts(pointer, 28) };
    let (little, size) = header(loaded)?;
    let command_bytes = word(loaded, 20, little)? as usize;
    ensure!(
        command_bytes <= MAX_COMMAND_BYTES,
        "loaded Mach-O commands exceed bound"
    );
    let bytes = unsafe { std::slice::from_raw_parts(pointer, size + command_bytes) };
    let cpu = word(loaded, 4, little)?;
    ensure!(
        image_uuid(bytes, cpu)? == disk_uuid(file, cpu)?,
        "on-disk executable differs from loaded Mach-O UUID; no Job admitted"
    );
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn thin(uuid: [u8; 16]) -> Vec<u8> {
        let mut bytes = vec![0; 56];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&24u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&LC_UUID.to_le_bytes());
        bytes[36..40].copy_from_slice(&24u32.to_le_bytes());
        bytes[40..56].copy_from_slice(&uuid);
        bytes
    }
    #[test]
    fn loaded_and_disk_uuid_detect_replacement_and_reject_missing_uuid() {
        let loaded = thin([1; 16]);
        let replacement = thin([2; 16]);
        let cpu = 0x0100000c;
        assert_eq!(
            disk_uuid(&mut std::io::Cursor::new(&loaded), cpu).unwrap(),
            [1; 16]
        );
        assert_ne!(
            image_uuid(&loaded, cpu).unwrap(),
            disk_uuid(&mut std::io::Cursor::new(replacement), cpu).unwrap()
        );
        let mut missing = loaded.clone();
        missing[32..36].copy_from_slice(&2u32.to_le_bytes());
        assert!(image_uuid(&missing, cpu).is_err());
        let mut malformed = loaded;
        malformed[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(image_uuid(&malformed, cpu).is_err());
    }
    #[test]
    fn universal_image_selects_loaded_cpu_and_bounds_slice() {
        let thin = thin([3; 16]);
        for (magic, wide, little) in [
            ([0xca, 0xfe, 0xba, 0xbe], false, false),
            ([0xbe, 0xba, 0xfe, 0xca], false, true),
            ([0xca, 0xfe, 0xba, 0xbf], true, false),
            ([0xbf, 0xba, 0xfe, 0xca], true, true),
        ] {
            let start = if wide { 40 } else { 28 };
            let mut bytes = vec![0; start];
            bytes[..4].copy_from_slice(&magic);
            let encode = |value: u32| {
                if little {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                }
            };
            bytes[4..8].copy_from_slice(&encode(1));
            bytes[8..12].copy_from_slice(&encode(0x0100000c));
            if wide {
                let encode = |value: u64| {
                    if little {
                        value.to_le_bytes()
                    } else {
                        value.to_be_bytes()
                    }
                };
                bytes[16..24].copy_from_slice(&encode(start as u64));
                bytes[24..32].copy_from_slice(&encode(thin.len() as u64));
            } else {
                bytes[16..20].copy_from_slice(&encode(start as u32));
                bytes[20..24].copy_from_slice(&encode(thin.len() as u32));
            }
            bytes.extend_from_slice(&thin);
            assert_eq!(
                disk_uuid(&mut std::io::Cursor::new(&bytes), 0x0100000c).unwrap(),
                [3; 16]
            );
            assert!(disk_uuid(&mut std::io::Cursor::new(bytes), 0x01000007).is_err());
        }
    }
}
