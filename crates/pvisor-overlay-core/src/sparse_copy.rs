//! Hole-preserving copies of regular files.
//!
//! Two independent gates must both be on. The compile-time
//! `sparse-checkpoint-copy` Cargo feature must be enabled, and the
//! run-time selector `PVISOR_SPARSE_CHECKPOINT_COPY` must name this copy site.
//! With either gate off the caller keeps its original byte-for-byte copy, so a
//! default build and a default run behave exactly like upstream `main`.
//!
//! The candidate is deliberately narrow: it only changes how a *regular file's*
//! bytes are produced. Hard links, symlinks, special files, directory metadata,
//! the hardlink table, xattr/ACL/ownership/time installation and the caller's
//! durability protocol are untouched.
//!
//! Contract of [`copy_regular_preserving_holes`]:
//!
//! * The destination is a freshly created, independently owned inode. It is
//!   never a hard link and never a reflink, so a later branch write cannot alias
//!   another branch. No source extent is shared.
//! * The destination length equals the source length observed on the very inode
//!   that is read (after `open`), and every byte outside a source data extent
//!   stays a hole. Source data extents are enumerated with
//!   `lseek(SEEK_DATA)`/`lseek(SEEK_HOLE)`; only those ranges are copied, using
//!   `copy_file_range` with a `pread`/`pwrite` fallback.
//! * Metadata is installed by the caller through `copy_snapshot_metadata`
//!   exactly as on the byte-copy path; this function sets no mode, owner, xattr
//!   or timestamp itself.
//! * Durability stays the caller's contract. This function performs no `fsync`,
//!   so the number and order of sync points do not change.
//! * Source stability is the caller's existing requirement: one sequential pass
//!   over a quiescent source. This helper neither strengthens nor weakens it; it
//!   only reads fewer bytes when the source is sparse.
//! * A hole query the filesystem does not support degrades to a dense copy of
//!   the same logical bytes, and a `copy_file_range` refusal degrades to
//!   `pread`/`pwrite`. Genuine I/O, space and quota failures (`ENOSPC`, `EDQUOT`,
//!   `EIO`, ...) are returned unchanged, so the caller's existing failure
//!   handling and unpublished-destination cleanup still apply.
//! * `ENXIO` normally means there is no later data. Before accepting it as EOF,
//!   the extent bytes already found are compared with the opened inode's
//!   allocated blocks. If allocation remains unaccounted for, the copy falls
//!   back to every logical byte instead of silently treating the anomaly as a
//!   trailing hole.

use crate::apply::UpperCopySite;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Run-time selector. Unset, empty or `0` keeps the byte-copy path.
const ENV_SELECT: &str = "PVISOR_SPARSE_CHECKPOINT_COPY";
/// Opt-in diagnostic sink: one JSON line per regular file copied by this helper.
const ENV_TRACE: &str = "PVISOR_SPARSE_COPY_TRACE";
/// Per-call `copy_file_range` count cap; large counts are not universally safe.
const COPY_RANGE_CHUNK: u64 = 1 << 30;
/// Reused `pread`/`pwrite` buffer when `copy_file_range` is unavailable.
const FALLBACK_CHUNK: usize = 1 << 20;

/// Stable selector token for one copy site.
fn site_name(site: UpperCopySite) -> &'static str {
    match site {
        UpperCopySite::CheckpointSnapshot => "checkpoint",
        UpperCopySite::RestoreFork => "restore",
    }
}

/// Bit for one copy site inside the parsed selector.
const fn site_bit(site: UpperCopySite) -> u8 {
    match site {
        UpperCopySite::CheckpointSnapshot => 1,
        UpperCopySite::RestoreFork => 2,
    }
}

fn site_by_name(name: &str) -> Option<UpperCopySite> {
    [
        UpperCopySite::CheckpointSnapshot,
        UpperCopySite::RestoreFork,
    ]
    .into_iter()
    .find(|site| site_name(*site) == name)
}

/// Parse the run-time selector into a set of enabled sites.
///
/// `1`/`on`/`all` enables every site, a comma/space/plus separated list enables
/// the named sites, and anything unrecognised disables the candidate instead of
/// enabling it by accident.
fn selection() -> u8 {
    let Some(raw) = std::env::var_os(ENV_SELECT) else {
        return 0;
    };
    let raw = raw.to_string_lossy();
    let mut selected = 0u8;
    for token in raw.split(|byte: char| byte == ',' || byte == '+' || byte.is_whitespace()) {
        if token.is_empty() {
            continue;
        }
        match token {
            "0" | "off" | "none" => return 0,
            "1" | "on" | "all" => return u8::MAX,
            other => match site_by_name(other) {
                Some(site) => selected |= site_bit(site),
                None => return 0,
            },
        }
    }
    selected
}

/// True only when this site is selected at run time.
pub(crate) fn preserve_holes(site: UpperCopySite) -> bool {
    selection() & site_bit(site) != 0
}

/// Where a regular file's bytes came from, for the opt-in trace only.
struct Plan {
    ranges: Vec<(u64, u64)>,
    holes_supported: bool,
}

fn dense_plan(logical: u64) -> Plan {
    Plan {
        ranges: vec![(0, logical)],
        holes_supported: false,
    }
}

fn inconsistent_hole_plan(logical: u64, start: u64, data: u64, hole: u64) -> Plan {
    // A filesystem that reports a data extent behind the requested cursor, or a
    // hole end at/before its data start, has not supplied enough information to
    // copy only selected ranges safely. Copy every logical byte instead of
    // treating the anomaly as end-of-data and silently dropping bytes.
    log::warn!(
        "inconsistent SEEK_DATA/SEEK_HOLE result at {start}: data={data}, hole={hole}; \
         falling back to a dense copy"
    );
    dense_plan(logical)
}

/// Enumerate source data extents. A filesystem that rejects `SEEK_DATA` falls
/// back to one whole-file range, which is the original dense copy.
fn data_extents(input: &File, logical: u64) -> io::Result<Plan> {
    let descriptor = input.as_raw_fd();
    let allocated = input.metadata()?.blocks().saturating_mul(512);
    data_extents_with(logical, allocated, |offset, whence| {
        // SAFETY: `descriptor` is an open read descriptor and the offsets are
        // plain `off_t` values that stay inside the file's supported range.
        let result = unsafe { libc::lseek(descriptor, offset as libc::off_t, whence) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as u64)
        }
    })
}

fn data_extents_with<F>(logical: u64, allocated: u64, mut seek: F) -> io::Result<Plan>
where
    F: FnMut(u64, libc::c_int) -> io::Result<u64>,
{
    let mut ranges = Vec::new();
    let mut position = 0u64;
    while position < logical {
        let data = match seek(position, libc::SEEK_DATA) {
            Ok(data) => data,
            Err(error) => {
                return match error.raw_os_error() {
                    // ENXIO: no further data before the end of the file.
                    Some(libc::ENXIO) => {
                        let covered = ranges
                            .iter()
                            .map(|(start, end)| end - start)
                            .fold(0u64, u64::saturating_add);
                        if covered < allocated {
                            log::warn!(
                                "SEEK_DATA returned ENXIO with {} allocated bytes but only \
                                 {} bytes enumerated; falling back to a dense copy",
                                allocated,
                                covered
                            );
                            Ok(dense_plan(logical))
                        } else {
                            Ok(Plan {
                                ranges,
                                holes_supported: true,
                            })
                        }
                    }
                    // The hole query itself is unsupported: copy every byte.
                    Some(libc::EINVAL) | Some(libc::ENOSYS) => Ok(dense_plan(logical)),
                    _ => Err(error),
                };
            }
        };
        if data < position {
            return Ok(inconsistent_hole_plan(logical, position, data, data));
        }
        if data >= logical {
            break;
        }
        let hole = match seek(data, libc::SEEK_HOLE) {
            Ok(hole) => hole,
            Err(error) => {
                return match error.raw_os_error() {
                    Some(libc::EINVAL) | Some(libc::ENOSYS) => Ok(dense_plan(logical)),
                    _ => Err(error),
                };
            }
        };
        let end = hole.min(logical);
        if end <= data {
            return Ok(inconsistent_hole_plan(logical, position, data, hole));
        }
        ranges.push((data, end));
        position = end;
    }
    Ok(Plan {
        ranges,
        holes_supported: true,
    })
}

fn pwrite_some(descriptor: RawFd, buffer: &[u8], offset: u64) -> io::Result<usize> {
    // SAFETY: the buffer is valid for `buffer.len()` bytes and the offset is a
    // plain `off_t` value. The destination descriptor remains owned by `File`.
    let written = unsafe {
        libc::pwrite(
            descriptor,
            buffer.as_ptr().cast(),
            buffer.len(),
            offset as libc::off_t,
        )
    };
    if written < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(written as usize)
    }
}

fn write_all_at_with<F>(mut buffer: &[u8], mut offset: u64, mut write: F) -> io::Result<()>
where
    F: FnMut(&[u8], u64) -> io::Result<usize>,
{
    while !buffer.is_empty() {
        match write(buffer, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "pwrite made no progress inside a known data extent",
                ));
            }
            Ok(written) if written <= buffer.len() => {
                buffer = &buffer[written..];
                offset += written as u64;
            }
            Ok(written) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "pwrite reported {written} bytes for a {}-byte buffer",
                        buffer.len()
                    ),
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_at(output: &File, buffer: &[u8], offset: u64) -> io::Result<()> {
    write_all_at_with(buffer, offset, |buffer, offset| {
        pwrite_some(output.as_raw_fd(), buffer, offset)
    })
}

/// Copy one source data range into the destination at the same offsets.
///
/// `copy_file_range` is attempted first; a refusal that only means "this kernel
/// or filesystem cannot do it" switches the rest of the copy to `pread`/`pwrite`
/// for the same ranges. A short transfer is retried, `EINTR` is retried, and a
/// zero-length transfer is treated as a refusal rather than a silent success.
fn copy_range(
    input: &File,
    output: &File,
    start: u64,
    length: u64,
    copy_file_range: &mut bool,
    scratch: &mut Vec<u8>,
    logical: u64,
) -> io::Result<()> {
    let mut offset = start;
    let mut remaining = length;
    while remaining > 0 {
        if *copy_file_range {
            let chunk = remaining.min(COPY_RANGE_CHUNK);
            let mut source_offset = offset as libc::off_t;
            let mut destination_offset = offset as libc::off_t;
            // SAFETY: both descriptors are open for the required access and both
            // offsets point at valid positions inside the files.
            let transferred = unsafe {
                libc::copy_file_range(
                    input.as_raw_fd(),
                    &mut source_offset,
                    output.as_raw_fd(),
                    &mut destination_offset,
                    chunk as usize,
                    0,
                )
            };
            if transferred > 0 {
                offset += transferred as u64;
                remaining -= transferred as u64;
                continue;
            }
            if transferred == 0 {
                // No progress is not end of file here: the range is known to
                // contain data, so fall back instead of publishing a short file.
                *copy_file_range = false;
                continue;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EXDEV)
                | Some(libc::EINVAL)
                | Some(libc::ENOSYS)
                | Some(libc::EOPNOTSUPP)
                | Some(libc::EPERM)
                | Some(libc::ETXTBSY) => {
                    *copy_file_range = false;
                    continue;
                }
                _ => return Err(error),
            }
        }
        if scratch.is_empty() {
            let size = (logical.min(FALLBACK_CHUNK as u64) as usize).max(1);
            scratch.resize(size, 0);
        }
        let chunk = remaining.min(scratch.len() as u64) as usize;
        // SAFETY: the buffer is `chunk` bytes long and the offset is a valid
        // position inside the source file.
        let read = unsafe {
            libc::pread(
                input.as_raw_fd(),
                scratch.as_mut_ptr().cast(),
                chunk,
                offset as libc::off_t,
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source shrank inside a known data extent",
            ));
        }
        let read = read as usize;
        write_all_at(output, &scratch[..read], offset)?;
        offset += read as u64;
        remaining -= read as u64;
    }
    Ok(())
}

/// Append one diagnostic line when `PVISOR_SPARSE_COPY_TRACE` names a file.
///
/// Diagnostics are best effort and never change the copy outcome.
fn trace(site: UpperCopySite, destination: &Path, logical: u64, plan: &Plan, ranged: bool) {
    let Some(path) = std::env::var_os(ENV_TRACE) else {
        return;
    };
    let record = serde_json::json!({
        "site": site_name(site),
        "destination": destination.to_string_lossy(),
        "logical_bytes": logical,
        "data_ranges": plan.ranges.len(),
        "data_bytes": plan.ranges.iter().map(|(start, end)| end - start).sum::<u64>(),
        "holes_supported": plan.holes_supported,
        "copy_file_range": ranged,
    });
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| writeln!(file, "{record}"));
}

/// Copy a regular file into a new independent destination, preserving holes.
///
/// See the module documentation for the full contract. The caller must have
/// removed any previous destination and must install metadata and durability
/// afterwards exactly as for a byte copy.
pub(crate) fn copy_regular_preserving_holes(
    source: &Path,
    destination: &Path,
    site: UpperCopySite,
) -> io::Result<()> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    // The length promised to the destination is the one observed on the opened
    // inode, so a concurrent size change cannot publish a mismatched target.
    let logical = input.metadata()?.len();
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    // Create the final logical length first: every range not written below stays
    // a hole instead of being materialised as zeros.
    output.set_len(logical)?;
    let plan = data_extents(&input, logical)?;
    // The pread/pwrite scratch buffer is allocated only if a real fallback
    // happens, so a dense file copied by copy_file_range pays nothing for it.
    let mut scratch: Vec<u8> = Vec::new();
    let mut copy_file_range = plan.holes_supported;
    for (start, end) in plan.ranges.iter().copied() {
        copy_range(
            &input,
            &output,
            start,
            end - start,
            &mut copy_file_range,
            &mut scratch,
            logical,
        )?;
    }
    // Re-assert the observed length: no range can extend past it, so this can
    // only undo an unexpected extension.
    output.set_len(logical)?;
    trace(site, destination, logical, &plan, copy_file_range);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_extents_keeps_multiple_ranges() {
        let plan = data_extents_with(4096, 0, |offset, whence| match (offset, whence) {
            (0, libc::SEEK_DATA) => Ok(100),
            (100, libc::SEEK_HOLE) => Ok(200),
            (200, libc::SEEK_DATA) => Ok(300),
            (300, libc::SEEK_HOLE) => Ok(4096),
            other => panic!("unexpected lseek request: {other:?}"),
        })
        .unwrap();

        assert_eq!(plan.ranges, vec![(100, 200), (300, 4096)]);
        assert!(plan.holes_supported);
    }

    #[test]
    fn data_extents_falls_back_when_hole_does_not_advance() {
        let plan = data_extents_with(4096, 0, |offset, whence| match (offset, whence) {
            (0, libc::SEEK_DATA) => Ok(0),
            (0, libc::SEEK_HOLE) => Ok(0),
            other => panic!("unexpected lseek request: {other:?}"),
        })
        .unwrap();

        assert_eq!(plan.ranges, vec![(0, 4096)]);
        assert!(!plan.holes_supported);
    }

    #[test]
    fn data_extents_falls_back_when_data_moves_backwards() {
        let plan = data_extents_with(4096, 0, |offset, whence| match (offset, whence) {
            (0, libc::SEEK_DATA) => Ok(512),
            (512, libc::SEEK_HOLE) => Ok(1024),
            (1024, libc::SEEK_DATA) => Ok(128),
            other => panic!("unexpected lseek request: {other:?}"),
        })
        .unwrap();

        assert_eq!(plan.ranges, vec![(0, 4096)]);
        assert!(!plan.holes_supported);
    }

    #[test]
    fn data_extents_rejects_enxio_while_allocated_bytes_are_unaccounted() {
        let plan = data_extents_with(4096, 4096, |_, _| {
            Err(io::Error::from_raw_os_error(libc::ENXIO))
        })
        .unwrap();

        assert_eq!(plan.ranges, vec![(0, 4096)]);
        assert!(!plan.holes_supported);
    }

    #[test]
    fn data_extents_accepts_enxio_after_all_allocation_is_accounted() {
        let plan = data_extents_with(4096, 4096, |offset, whence| match (offset, whence) {
            (0, libc::SEEK_DATA) => Ok(0),
            (0, libc::SEEK_HOLE) => Ok(4096),
            (4096, libc::SEEK_DATA) => Err(io::Error::from_raw_os_error(libc::ENXIO)),
            other => panic!("unexpected lseek request: {other:?}"),
        })
        .unwrap();

        assert_eq!(plan.ranges, vec![(0, 4096)]);
        assert!(plan.holes_supported);
    }

    #[test]
    fn write_all_at_rejects_zero_progress() {
        let mut calls = 0;
        let error = write_all_at_with(b"payload", 7, |_, _| {
            calls += 1;
            Ok(0)
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!(calls, 1);
    }

    #[test]
    fn write_all_at_retries_interrupts_and_short_writes() {
        let mut calls = 0;
        write_all_at_with(b"payload", 7, |buffer, offset| {
            calls += 1;
            match calls {
                1 => Err(io::Error::from_raw_os_error(libc::EINTR)),
                2 => {
                    assert_eq!(offset, 7);
                    assert_eq!(buffer, b"payload");
                    Ok(3)
                }
                3 => {
                    assert_eq!(offset, 10);
                    assert_eq!(buffer, b"load");
                    Ok(buffer.len())
                }
                other => panic!("unexpected write call {other}"),
            }
        })
        .unwrap();

        assert_eq!(calls, 3);
    }
}
