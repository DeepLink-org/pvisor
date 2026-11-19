//! Best-effort cache reclaim; never grants a permit or reports byte savings.
use crate::api::{
    PressureRecoveryBlocked as RecoveryBlocked, PressureRecoveryReclaim as RecoveryReclaim,
};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Retry {
    permit: Option<(String, String)>,
    attempts: u8,
    started: Option<Instant>,
    last: Option<Instant>,
    pub(super) in_flight: bool,
}

impl Retry {
    pub(super) fn begin(
        &mut self,
        blocked: &RecoveryBlocked,
        now: Instant,
    ) -> Option<RecoveryReclaim> {
        if self.in_flight || !blocked.reclaim_eligible || blocked.deficit_bytes == 0 {
            return None;
        }
        let permit = (blocked.id.clone(), blocked.token.clone());
        if self.permit.as_ref() != Some(&permit) {
            self.permit = Some(permit);
            self.attempts = 0;
            self.started = Some(now);
            self.last = None;
        }
        if self.attempts >= 3
            || now.duration_since(self.started.unwrap()) >= Duration::from_secs(10)
            || self
                .last
                .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return None;
        }
        self.attempts += 1;
        self.last = Some(now);
        self.in_flight = true;
        Some(RecoveryReclaim {
            id: blocked.id.clone(),
            token: blocked.token.clone(),
            attempts: self.attempts,
            requested_bytes: blocked
                .deficit_bytes
                .saturating_add(16 * 1024 * 1024)
                .min(256 * 1024 * 1024),
            in_flight: true,
            error: None,
        })
    }
}

// The directory FD pins the validated cgroup through the write. Never write a
// counter fixture or a replaced root, follow a symlink, or enable swapping.
#[cfg(target_os = "linux")]
pub(super) fn write(
    root: &std::path::Path,
    identity: (u64, u64),
    bytes: u64,
) -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
    };
    ensure!(
        bytes > 0 && bytes <= 256 * 1024 * 1024,
        "invalid reclaim request"
    );
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    let m = directory.metadata()?;
    ensure!(
        (m.dev(), m.ino()) == identity,
        "reclaim cgroup identity changed"
    );
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    ensure!(
        unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } == 0,
        "reclaim filesystem inspection failed"
    );
    ensure!(
        i128::from(unsafe { stat.assume_init() }.f_type) == i128::from(libc::CGROUP2_SUPER_MAGIC),
        "reclaim requires cgroup v2"
    );
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"memory.reclaim".as_ptr(),
            libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open memory.reclaim");
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let command = format!("{bytes} swappiness=0");
    // A cgroup write is a command, not a stream. Never replay a partial write.
    let written = file.write(command.as_bytes()).context("memory.reclaim")?;
    ensure!(written == command.len(), "partial memory.reclaim write");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn write(_: &std::path::Path, _: (u64, u64), _: u64) -> anyhow::Result<()> {
    anyhow::bail!("recovery reclaim requires Linux cgroup v2")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blocked() -> RecoveryBlocked {
        RecoveryBlocked {
            id: "vm".into(),
            token: "permit".into(),
            deficit_bytes: 128 * 1024 * 1024,
            reclaim_eligible: true,
        }
    }
    #[test]
    fn retries_are_bounded_and_slow_writes_are_not_duplicated() {
        let mut retry = Retry::default();
        let start = Instant::now();
        let mut b = blocked();
        assert_eq!(
            retry.begin(&b, start).unwrap().requested_bytes,
            144 * 1024 * 1024
        );
        assert!(retry.begin(&b, start + Duration::from_secs(2)).is_none());
        retry.in_flight = false;
        assert!(
            retry
                .begin(&b, start + Duration::from_millis(999))
                .is_none()
        );
        for i in 1..3 {
            assert!(retry.begin(&b, start + Duration::from_secs(i)).is_some());
            retry.in_flight = false;
        }
        assert!(retry.begin(&b, start + Duration::from_secs(3)).is_none());
        b.token = "next-permit".into();
        b.deficit_bytes = u64::MAX;
        assert_eq!(
            retry
                .begin(&b, start + Duration::from_secs(4))
                .unwrap()
                .requested_bytes,
            256 * 1024 * 1024
        );
    }
    #[test]
    fn retry_window_expires_and_ineligible_samples_do_not_start_work() {
        let mut retry = Retry::default();
        let start = Instant::now();
        let mut b = blocked();
        b.reclaim_eligible = false;
        assert!(retry.begin(&b, start).is_none());
        b.reclaim_eligible = true;
        assert!(retry.begin(&b, start).is_some());
        retry.in_flight = false;
        assert!(retry.begin(&b, start + Duration::from_secs(10)).is_none());
    }
    #[test]
    fn ordinary_files_are_never_used_as_reclaim_authority() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("memory.reclaim");
        std::fs::write(&p, "untouched").unwrap();
        let m = tmp.path().metadata().unwrap();
        assert!(write(tmp.path(), (m.dev(), m.ino()), 4096).is_err());
        assert_eq!(std::fs::read_to_string(p).unwrap(), "untouched");
    }
}
