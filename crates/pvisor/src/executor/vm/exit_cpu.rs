//! One event-driven pidfd per observed VM. It never reaps the child; Tokio
//! retains exit status and process ownership until final counters are captured.
use anyhow::{Context, ensure};
use pvisor_core::cpu::TerminalCpuUsage;
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::process::ExitStatusExt,
    sync::Arc,
};
use tokio::io::unix::AsyncFd;

pub(super) struct Observer {
    pidfd: AsyncFd<File>,
    proc: Arc<File>,
    pid: u32,
    start: u64,
}
impl Observer {
    pub fn new(proc: Arc<File>, pid: u32, start: u64) -> anyhow::Result<Self> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        ensure!(
            fd >= 0,
            "open native CPU exit pidfd: {}",
            std::io::Error::last_os_error()
        );
        let pidfd = unsafe { File::from_raw_fd(i32::try_from(fd)?) };
        Ok(Self {
            pidfd: AsyncFd::new(pidfd)?,
            proc,
            pid,
            start,
        })
    }
    pub async fn ready(&self) -> anyhow::Result<std::process::ExitStatus> {
        let _ready = self
            .pidfd
            .readable()
            .await
            .context("wait for native CPU exit")?;
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // WNOWAIT preserves the exact child for Tokio's authoritative reap.
        ensure!(
            unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.pidfd.get_ref().as_raw_fd() as u32,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            } == 0,
            "observe native CPU exit: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { info.si_pid() } == self.pid as i32
                && matches!(
                    info.si_code,
                    libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
                ),
            "native CPU exit identity/state mismatch"
        );
        let status = unsafe { info.si_status() };
        let raw = if info.si_code == libc::CLD_EXITED {
            status << 8
        } else {
            status
                | if info.si_code == libc::CLD_DUMPED {
                    0x80
                } else {
                    0
                }
        };
        Ok(std::process::ExitStatus::from_raw(raw))
    }
    pub async fn sample(&self) -> TerminalCpuUsage {
        let proc = self.proc.clone();
        let (pid, start) = (self.pid, self.start);
        match tokio::task::spawn_blocking(move || super::cpu::sample_exited(&proc, pid, start))
            .await
        {
            Ok(Ok(usage)) => TerminalCpuUsage::Measured { usage },
            Ok(Err(error)) => TerminalCpuUsage::unavailable(format!("{error:#}")),
            Err(error) => TerminalCpuUsage::unavailable(error),
        }
    }
    /// Cancellation/deadline cannot use a helper that reaps first. Observe and
    /// sample the leader's exit, then let the existing tree cleanup reap it and
    /// terminate any remaining group members.
    pub async fn terminate(&self, group: Option<u32>, grace_ms: u64) -> TerminalCpuUsage {
        let target = group.map_or(self.pid as i32, |pid| -(pid as i32));
        unsafe {
            libc::kill(target, libc::SIGTERM);
        }
        let ready =
            match tokio::time::timeout(std::time::Duration::from_millis(grace_ms), self.ready())
                .await
            {
                Ok(ready) => ready,
                Err(_) => {
                    unsafe {
                        libc::kill(target, libc::SIGKILL);
                    }
                    self.ready().await
                }
            };
        match ready {
            Ok(_) => self.sample().await,
            Err(error) => TerminalCpuUsage::unavailable(format!("{error:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn observer(pid: u32) -> Observer {
        let proc = Arc::new(File::open(format!("/proc/{pid}")).unwrap());
        let start = super::super::memory::identity(
            &std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap(),
            pid,
        )
        .unwrap();
        Observer::new(proc, pid, start).unwrap()
    }

    #[tokio::test]
    async fn normal_exit_counters_are_stable_before_reap() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args([
                "-c",
                "read line; i=0; while [ \"$i\" -lt 30000 ]; do i=$((i+1)); done; exit 7",
            ])
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let observer = observer(pid);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"go\n")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), observer.ready())
            .await
            .unwrap()
            .unwrap();
        let TerminalCpuUsage::Measured { usage } = observer.sample().await else {
            panic!("expected final CPU counters")
        };
        assert!(usage.total_ticks().unwrap() > 0);
        // Repeated WNOWAIT proves the observation did not consume exit status.
        observer.ready().await.unwrap();
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<_> = text
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        assert_eq!(fields[0], "Z");
        assert_eq!(
            usage.user_time_ticks,
            fields[14 - 3].parse::<u64>().unwrap()
        );
        assert_eq!(
            usage.system_time_ticks,
            fields[15 - 3].parse::<u64>().unwrap()
        );
        assert!(super::super::cpu::sample(&observer.proc, pid, observer.start).is_err());
        assert_eq!(child.wait().await.unwrap().code(), Some(7));
        assert!(observer.ready().await.is_err());
        assert!(matches!(
            observer.sample().await,
            TerminalCpuUsage::Unavailable { .. }
        ));
    }

    #[tokio::test]
    async fn termination_escalates_and_captures_before_tokio_reaps() {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; printf x; while :; do :; done"])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let observer = observer(pid);
        child
            .stdout
            .take()
            .unwrap()
            .read_exact(&mut [0u8; 1])
            .await
            .unwrap();
        let before = super::super::cpu::sample(&observer.proc, pid, observer.start).unwrap();
        let final_cpu = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            observer.terminate(Some(pid), 30),
        )
        .await
        .unwrap();
        let TerminalCpuUsage::Measured { usage } = final_cpu else {
            panic!("expected signal-terminated CPU counters")
        };
        usage.interval_since(&before).unwrap();
        assert_eq!(child.wait().await.unwrap().signal(), Some(libc::SIGKILL));
    }
}
