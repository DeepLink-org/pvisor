//! Native launch preparation preserves descriptor ownership and resource limits.
#[cfg(target_os = "linux")]
mod linux {
    use std::{fs::File, os::fd::AsRawFd, process::Command};

    fn descriptors() -> Vec<String> {
        let mut entries: Vec<_> = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn preparation_is_repeatable_and_keeps_existing_descriptors_unchanged() {
        let file = File::open("/dev/null").unwrap();
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        let before = descriptors();
        for _ in 0..3 {
            pvisor::VmExecutor::prepare_process().unwrap();
            assert_eq!(descriptors(), before);
            assert_eq!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) },
                flags
            );
        }
    }

    #[test]
    fn preparation_does_not_clobber_existing_high_descriptors() {
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::os::fd::FromRawFd;
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(b"retained control descriptor").unwrap();
        source.seek(SeekFrom::Start(0)).unwrap();
        let fd = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 203) };
        assert!(fd >= 203);
        let mut retained = unsafe { File::from_raw_fd(fd) };
        let before = descriptors();
        pvisor::VmExecutor::prepare_process().unwrap();
        assert_eq!(descriptors(), before);
        let mut content = String::new();
        retained.read_to_string(&mut content).unwrap();
        assert_eq!(content, "retained control descriptor");
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    #[test]
    fn low_descriptor_limit_returns_error_without_leaks_or_limit_changes() {
        const CHILD: &str = "PVISOR_FD_RESERVE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "linux::low_descriptor_limit_returns_error_without_leaks_or_limit_changes",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let mut limits: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
            0
        );
        limits.rlim_cur = limits.rlim_cur.min(64);
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) }, 0);
        let before = descriptors();
        assert!(pvisor::VmExecutor::prepare_process().is_err());
        assert_eq!(descriptors(), before);
        let mut after: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut after) },
            0
        );
        assert_eq!(
            (after.rlim_cur, after.rlim_max),
            (limits.rlim_cur, limits.rlim_max)
        );
    }
}
