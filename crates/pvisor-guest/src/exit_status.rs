//! PID-1-only virtio-fs exit protocol shared with the VMM.
//!
//! Claim once on the root device before starting workloads. The returned token
//! stays in PID 1 memory, never in launch JSON or workload environment. Report
//! once after reaping and filesystem synchronization. Both peers require a
//! trusted guest kernel and supervisor; this cannot attest a compromised PID 1.

use crate::api::{EXIT_REPORT_BYTES, EXIT_TOKEN_BYTES, GuestExitReport};
impl GuestExitReport for [u8; EXIT_TOKEN_BYTES] {
    fn report(&self, code: i32) -> std::io::Result<[u8; EXIT_REPORT_BYTES]> {
        let token = self;
        if !(0..=255).contains(&code) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "guest exit code must be in 0..=255",
            ));
        }
        let mut bytes = [0; EXIT_REPORT_BYTES];
        bytes[..EXIT_TOKEN_BYTES].copy_from_slice(token);
        bytes[EXIT_TOKEN_BYTES..].copy_from_slice(&(code as u32).to_le_bytes());
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guest_exit_report_is_bounded_and_little_endian() {
        let token = [0xa5; super::EXIT_TOKEN_BYTES];
        for code in [0, 7, 125, 255] {
            let bytes = super::GuestExitReport::report(&token, code).unwrap();
            assert_eq!(&bytes[..16], &token);
            assert_eq!(&bytes[16..], &(code as u32).to_le_bytes());
        }
        for code in [-1, 256, i32::MIN, i32::MAX] {
            assert!(super::GuestExitReport::report(&token, code).is_err());
        }
    }
}
