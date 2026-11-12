//! Private detached-owner signal handling. Never installed in the user CLI or
//! an embedded caller; SIGKILL cannot be handled and still needs external audit.
use std::io;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};

static REQUESTED: AtomicI32 = AtomicI32::new(0);
extern "C" fn request(signal: libc::c_int) {
    REQUESTED.store(signal, Ordering::Relaxed);
}

pub(super) struct Termination {
    previous: Vec<(i32, libc::sigaction)>,
}
impl Termination {
    pub(super) fn install() -> io::Result<Self> {
        REQUESTED.store(0, Ordering::Relaxed);
        let mut guard = Self {
            previous: Vec::new(),
        };
        for signal in [libc::SIGTERM, libc::SIGINT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = request as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.previous.push((signal, previous));
        }
        Ok(guard)
    }
}
impl Drop for Termination {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.iter().rev() {
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}
pub(super) fn requested() -> Option<i32> {
    match REQUESTED.load(Ordering::Relaxed) {
        0 => None,
        signal => Some(signal),
    }
}

// The caller holds the backing owner throughout this wait. A kill request is
// not an exit receipt; never return success before wait/try_wait has reaped it.
pub(super) fn terminate_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    child.kill()?;
    child.wait()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    #[test]
    fn owner_signal_child_helper() {
        let Some(root) = std::env::var_os("PVISOR_TEST_OWNER_SIGNAL_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let _signals = Termination::install().unwrap();
        // A dependent process and its backing lifetime are independent. Marker
        // removal must happen only after the child has actually been reaped.
        fs::write(root.join("backing-live"), "live").unwrap();
        let fuse = std::env::var_os("PVISOR_TEST_OWNER_FUSE").is_some();
        let (backing_owner, backing_channel, backing_path) = if fuse {
            let (owner, mut channel) = super::super::managed::ParentBacking::prepare(
                &root,
                super::super::managed::Target {
                    container_id: "a".repeat(64),
                    generation: "b".repeat(32),
                },
            )
            .unwrap();
            let handoff: serde_json::Value =
                crate::host_transport::read_host_frame_sync(&mut channel).unwrap();
            (
                Some(owner),
                Some(channel),
                handoff["path"].as_str().unwrap().to_owned(),
            )
        } else {
            (None, None, String::new())
        };
        let mut command = Command::new(if fuse { "python3" } else { "sh" });
        if fuse {
            command.args(["-c", "import sys,time; f=open(sys.argv[1],'r+b'); f.truncate(4096); f.write(b'owner-lifetime'); f.flush(); open(sys.argv[2],'w').write('ready'); time.sleep(60)", &backing_path, root.join("fd-ready").to_str().unwrap()]);
        } else {
            command.args(["-c", "exec sleep 60"]);
        }
        let mut child = super::super::ChildGuard(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let open_deadline = Instant::now() + Duration::from_secs(5);
        while fuse && !root.join("fd-ready").exists() {
            assert!(Instant::now() < open_deadline);
            assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(5));
        }
        fs::write(root.join("ready"), child.id().to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while requested().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(root.join("backing-live").exists());
        let status = terminate_and_reap(&mut child).unwrap();
        assert!(!status.success());
        fs::write(root.join("reaped"), "yes").unwrap();
        drop(child);
        drop(backing_channel);
        drop(backing_owner);
        if fuse {
            assert!(!std::path::Path::new(&backing_path).exists());
            assert!(
                !fs::read_to_string("/proc/self/mountinfo")
                    .unwrap()
                    .contains(root.to_str().unwrap())
            );
        }
        fs::remove_file(root.join("backing-live")).unwrap();
    }

    fn signal_case(fuse: bool) {
        for signal in [libc::SIGTERM, libc::SIGINT] {
            let root = tempfile::tempdir().unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "docker::owner_shutdown::tests::owner_signal_child_helper",
                    "--nocapture",
                ])
                .env("PVISOR_TEST_OWNER_SIGNAL_ROOT", root.path());
            if fuse {
                command.env("PVISOR_TEST_OWNER_FUSE", "1");
            } else {
                command.env_remove("PVISOR_TEST_OWNER_FUSE");
            }
            let mut owner = super::super::ChildGuard(
                command
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while !root.path().join("ready").exists() {
                assert!(Instant::now() < deadline);
                assert!(owner.try_wait().unwrap().is_none());
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(unsafe { libc::kill(owner.id() as i32, signal) }, 0);
            loop {
                if let Some(status) = owner.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(root.path().join("reaped").exists());
            assert!(!root.path().join("backing-live").exists());
        }
    }

    #[test]
    fn owner_signal_keeps_backing_until_runner_reaped() {
        signal_case(false);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires Linux FUSE mount permission; no VM is started"]
    fn owner_signal_with_fuse_keeps_mount_until_runner_exit() {
        signal_case(true);
    }

    #[test]
    fn already_reaped_child_is_not_signalled_again() {
        let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        assert_eq!(terminate_and_reap(&mut child).unwrap().code(), Some(7));
    }
}
