//! Identity-anchored Unix child/group ownership, shared with input preparation.
use anyhow::{Context, Result, ensure};
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

/// The unreaped leader pins its PID/process-group identity until group cleanup.
/// Callers may access stdio and `id` through `.0`, but must not reap `.0` directly.
pub struct OwnedChild(pub Child, Option<ExitStatus>);
impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        Ok(Self(command.process_group(0).spawn()?, None))
    }
    /// Observe exit without reaping, including readiness-failure paths.
    pub fn running(&self) -> Result<bool> {
        if self.1.is_some() {
            return Ok(false);
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.0.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("child identity anchor unavailable");
        }
        #[cfg(target_os = "linux")]
        let pid = unsafe { info.si_pid() };
        #[cfg(not(target_os = "linux"))]
        let pid = info.si_pid;
        Ok(pid == 0)
    }
    /// No signal is sent after reaping. WNOWAIT pins even an exited leader while
    /// its descendants are signaled, preventing signaling a recycled PID/PGID.
    pub fn signal(&self, signal: i32) -> Result<()> {
        if self.1.is_some() {
            return Ok(());
        }
        let _ = self.running()?;
        let result = unsafe { libc::kill(-(self.0.id() as i32), signal) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }
    pub fn wait(&mut self, seconds: u64) -> Result<ExitStatus> {
        if let Some(status) = self.1 {
            return Ok(status);
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if !self.running()? {
                // A successful leader may leave children holding pipes. Stop
                // those children before releasing the identity anchor.
                self.signal(libc::SIGKILL)?;
                let status = self.0.wait()?;
                self.1 = Some(status);
                return Ok(status);
            }
            ensure!(
                Instant::now() < deadline,
                "process {} timed out",
                self.0.id()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn kill_and_wait(&mut self) -> Result<ExitStatus> {
        self.signal(libc::SIGKILL)?;
        self.wait(5)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.1.is_none() {
            // If another owner illegally reaped the leader, fail closed: never
            // signal a numeric PID/PGID after loss of its waitable identity.
            let _ = self.signal(libc::SIGKILL);
            self.1 = self.0.wait().ok();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
    };
    fn gone_or_zombie(pid: u32) -> bool {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return true;
        };
        text.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z")
    }
    fn assert_stopped(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone_or_zombie(pid) {
            assert!(
                Instant::now() < deadline,
                "owned descendant {pid} survived cleanup"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn subreaper() {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
    }
    fn reap_descendant(pid: u32) {
        let mut status = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let reaped = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
            if reaped == pid as i32 {
                return;
            }
            if reaped < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                // The shell's `wait` may have reaped its own child while group
                // termination was delivered. Subreaping only adopts children
                // that remain: accept ECHILD only after proving this PID gone.
                if error.raw_os_error() == Some(libc::ECHILD) {
                    if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                        return;
                    }
                } else {
                    panic!("cannot reap owned descendant {pid}: {error}");
                }
            }
            assert!(Instant::now() < deadline, "descendant {pid} was not reaped");
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn readiness_exit_retains_anchor_until_cleanup_and_reap() {
        let mut child = OwnedChild::spawn(Command::new("sh").args(["-c", "exit 7"])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.running().unwrap() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let pid = child.0.id();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        child.signal(libc::SIGTERM).unwrap();
        assert_eq!(child.wait(1).unwrap().code(), Some(7));
        // A post-reap stop is a no-op, not a raw numeric-PID signal.
        child.signal(libc::SIGTERM).unwrap();
    }
    #[test]
    fn exited_leader_cleans_descendants_before_reaping() {
        subreaper();
        let mut child = OwnedChild::spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & echo $!; exit 0"])
                .stdout(Stdio::piped()),
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        assert!(child.wait(2).unwrap().success());
        assert_stopped(descendant);
        reap_descendant(descendant);
    }
    #[test]
    fn timeout_drop_cleans_leader_and_descendant_group() {
        subreaper();
        let mut child = OwnedChild::spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & echo $!; wait"])
                .stdout(Stdio::piped()),
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        let leader = child.0.id();
        assert!(child.wait(0).is_err());
        drop(child);
        assert_stopped(leader);
        assert_stopped(descendant);
        reap_descendant(descendant);
    }
}
