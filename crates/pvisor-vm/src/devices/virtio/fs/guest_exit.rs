//! Root-device, kernel-identified PID 1 exit authority. Never accept legacy arg ioctls.
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};

use pvisor_guest::exit_status::{CLAIM_IOCTL, REPORT_IOCTL, TOKEN_BYTES};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) enum GuestExit {
    #[default]
    Unclaimed,
    Armed([u8; TOKEN_BYTES]),
    Submitted,
}

impl std::fmt::Debug for GuestExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unclaimed => "Unclaimed",
            Self::Armed(_) => "Armed(<redacted>)",
            Self::Submitted => "Submitted",
        })
    }
}

impl GuestExit {
    pub(super) fn request(
        &mut self,
        pid: u32,
        uid: u32,
        cmd: u32,
        body: &[u8],
        status: &AtomicI32,
    ) -> io::Result<Vec<u8>> {
        if pid != 1 || uid != 0 {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        match cmd {
            CLAIM_IOCTL if body.is_empty() && matches!(self, Self::Unclaimed) => {
                let mut token = [0; TOKEN_BYTES];
                rand::rngs::OsRng
                    .try_fill_bytes(&mut token)
                    .map_err(io::Error::other)?;
                *self = Self::Armed(token);
                Ok(token.to_vec())
            }
            REPORT_IOCTL if body.len() == pvisor_guest::exit_status::REPORT_BYTES => {
                let Self::Armed(token) = self else {
                    return Err(io::Error::from_raw_os_error(libc::EPERM));
                };
                let different = token
                    .iter()
                    .zip(body)
                    .fold(0u8, |diff, (a, b)| diff | (a ^ b));
                if different != 0 {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
                let code = u32::from_le_bytes(body[TOKEN_BYTES..].try_into().unwrap());
                if code > 255 {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL));
                }
                status
                    .compare_exchange(i32::MAX, code as i32, Ordering::SeqCst, Ordering::SeqCst)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))?;
                *self = Self::Submitted;
                Ok(Vec::new())
            }
            _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_exit_requires_pid1_token_range_and_single_submission() {
        let status = AtomicI32::new(i32::MAX);
        let mut channel = GuestExit::default();
        assert!(channel.request(2, 0, CLAIM_IOCTL, &[], &status).is_err());
        assert!(channel.request(1, 1, CLAIM_IOCTL, &[], &status).is_err());
        let token: [u8; TOKEN_BYTES] = channel
            .request(1, 0, CLAIM_IOCTL, &[], &status)
            .unwrap()
            .try_into()
            .unwrap();
        assert!(channel.request(1, 0, CLAIM_IOCTL, &[], &status).is_err());
        let report = pvisor_guest::exit_status::report(&token, 7).unwrap();
        assert!(channel
            .request(2, 0, REPORT_IOCTL, &report, &status)
            .is_err());
        let mut forged = report;
        forged[0] ^= 1;
        assert!(channel
            .request(1, 0, REPORT_IOCTL, &forged, &status)
            .is_err());
        let mut invalid = report;
        invalid[TOKEN_BYTES..].copy_from_slice(&256u32.to_le_bytes());
        assert!(channel
            .request(1, 0, REPORT_IOCTL, &invalid, &status)
            .is_err());
        assert_eq!(status.load(Ordering::SeqCst), i32::MAX);
        assert!(channel
            .request(1, 0, REPORT_IOCTL, &report[..19], &status)
            .is_err());
        channel
            .request(1, 0, REPORT_IOCTL, &report, &status)
            .unwrap();
        assert_eq!(status.load(Ordering::SeqCst), 7);
        assert!(channel
            .request(1, 0, REPORT_IOCTL, &report, &status)
            .is_err());
        assert_eq!(status.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn guest_exit_rejects_foreign_boot_and_preserves_snapshot_authority() {
        let status = AtomicI32::new(i32::MAX);
        let mut first = GuestExit::default();
        let token = first.request(1, 0, CLAIM_IOCTL, &[], &status).unwrap();
        let token: [u8; TOKEN_BYTES] = token.try_into().unwrap();
        assert!(!format!("{first:?}").contains(&format!("{token:?}")));
        let mut restored: GuestExit =
            serde_json::from_slice(&serde_json::to_vec(&first).unwrap()).unwrap();
        let mut other = GuestExit::default();
        other.request(1, 0, CLAIM_IOCTL, &[], &status).unwrap();
        let report = pvisor_guest::exit_status::report(&token, 0).unwrap();
        assert!(other.request(1, 0, REPORT_IOCTL, &report, &status).is_err());
        restored
            .request(1, 0, REPORT_IOCTL, &report, &status)
            .unwrap();
        assert_eq!(status.load(Ordering::SeqCst), 0);
    }
}
