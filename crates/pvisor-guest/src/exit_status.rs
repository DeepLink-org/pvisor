//! PID-1-only virtio-fs exit protocol shared with the VMM.
//!
//! Claim once on the root device before starting workloads. The returned token
//! stays in PID 1 memory, never in launch JSON or workload environment. Report
//! once after reaping and filesystem synchronization. Both peers require a
//! trusted guest kernel and supervisor; this cannot attest a compromised PID 1.

/// Linux `_IOR('v', 3, [u8; 16])`: return a fresh 128-bit token to PID 1.
pub const CLAIM_IOCTL: u32 = 0x8010_7603;
/// Linux `_IOW('v', 4, [u8; 20])`: token followed by a little-endian u32 status.
/// Only statuses in `0..=255` are accepted; duplicate reports are rejected.
pub const REPORT_IOCTL: u32 = 0x4014_7604;
/// Number of token bytes returned by the claim operation.
pub const TOKEN_BYTES: usize = 16;
/// Exact report payload length: token plus four-byte exit code, with no padding.
pub const REPORT_BYTES: usize = TOKEN_BYTES + 4;

/// Encode one report, rejecting negative or out-of-range exit codes before I/O.
/// This does not authenticate the caller or consume the token.
pub fn report(token: &[u8; TOKEN_BYTES], code: i32) -> std::io::Result<[u8; REPORT_BYTES]> {
    if !(0..=255).contains(&code) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "guest exit code must be in 0..=255",
        ));
    }
    let mut bytes = [0; REPORT_BYTES];
    bytes[..TOKEN_BYTES].copy_from_slice(token);
    bytes[TOKEN_BYTES..].copy_from_slice(&(code as u32).to_le_bytes());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn guest_exit_report_is_bounded_and_little_endian() {
        let token = [0xa5; super::TOKEN_BYTES];
        for code in [0, 7, 125, 255] {
            let bytes = super::report(&token, code).unwrap();
            assert_eq!(&bytes[..16], &token);
            assert_eq!(&bytes[16..], &(code as u32).to_le_bytes());
        }
        for code in [-1, 256, i32::MIN, i32::MAX] {
            assert!(super::report(&token, code).is_err());
        }
    }
}
