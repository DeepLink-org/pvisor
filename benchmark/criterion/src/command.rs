//! Process lifetime and evidence, not a sampling or statistics layer.
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub(crate) struct Observation {
    pub argv: Vec<String>,
    pub elapsed_ns: u128,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

fn wait_without_reaping(pid: u32) -> Result<()> {
    #[cfg(unix)]
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    #[cfg(not(unix))]
    bail!("wait-without-reaping is unsupported for process {pid}");
}

#[cfg(unix)]
fn kill_owned_group(pid: u32) -> bool {
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    // The observing thread never reaps before the supervisor joins. Confirm the
    // anchor still exists even on abnormal wait paths (e.g. inherited SIGCHLD
    // auto-reaping); a cached numeric PID alone never authorizes a signal.
    loop {
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        } == 0
        {
            return unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } == 0;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[cfg(unix)]
fn drain_pipe(
    mut pipe: impl Read + std::os::fd::AsRawFd,
    started: Instant,
    timeout: Duration,
) -> std::io::Result<(Vec<u8>, Instant, bool)> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok((bytes, Instant::now(), true)),
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok((bytes, Instant::now(), false));
        }
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let milliseconds = (remaining.as_millis() + 1).min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut descriptor, 1, milliseconds) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// Run directly, drain both pipes concurrently, and kill/reap the process group
/// on timeout. Successful command timing ends after wait and pipe EOF.
/// The observation is returned even for a nonzero exit so callers can retain it.
pub(crate) fn observe(command: &mut Command, timeout: Duration) -> Result<Observation> {
    #[cfg(not(unix))]
    bail!("owned process-group observation is unsupported on this platform");
    let argv = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    // Benchmarks never consume the frontend's interactive terminal. A new
    // process group must not inherit a tty owned by a different foreground group.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let started = Instant::now();
    let mut child = command.spawn().context("spawn measured command")?;
    let pid = child.id();
    // This supervisor never supplies a timing value. It stays armed until both
    // child exit and pipe EOF, including an exited parent's pipe-holding children.
    let (cancel, cancelled) = mpsc::channel();
    let remaining = timeout.saturating_sub(started.elapsed());
    let supervisor = thread::spawn(move || {
        if cancelled.recv_timeout(remaining).is_ok() {
            return false;
        }
        #[cfg(unix)]
        kill_owned_group(pid);
        true
    });
    let stdout = child.stdout.take().context("stdout pipe")?;
    let stderr = child.stderr.take().context("stderr pipe")?;
    #[cfg(unix)]
    let out = thread::spawn(move || drain_pipe(stdout, started, timeout));
    #[cfg(unix)]
    let err = thread::spawn(move || drain_pipe(stderr, started, timeout));
    if let Err(error) = wait_without_reaping(pid) {
        let _ = cancel.send(());
        let _ = supervisor.join();
        // Without a waitable identity anchor, do not signal a possibly recycled
        // PID. Pipe readers have independent finite deadlines even on this path.
        return Err(error.context("wait measured command"));
    }
    let exited = Instant::now();
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"));
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"));
    let _ = cancel.send(());
    let supervisor_timed_out = supervisor
        .join()
        .map_err(|_| anyhow::anyhow!("supervisor panicked"))?;
    // Keep the exited leader unreaped until the supervisor is disarmed and group
    // cleanup finishes. Its PID/PGID cannot be recycled underneath either kill.
    #[cfg(unix)]
    kill_owned_group(pid);
    let status = child.wait().context("reap measured command")?;
    // Defer reader error propagation until ownership cleanup has completed.
    let (stdout, stdout_eof, stdout_complete) = stdout??;
    let (stderr, stderr_eof, stderr_complete) = stderr??;
    let ended = exited.max(stdout_eof).max(stderr_eof);
    let timed_out = supervisor_timed_out
        || !stdout_complete
        || !stderr_complete
        || ended.duration_since(started) >= timeout;
    Ok(Observation {
        argv,
        elapsed_ns: ended.duration_since(started).as_nanos(),
        exit_code: status.code(),
        stdout,
        stderr,
        timed_out,
    })
}

pub(crate) fn require_success(observation: &Observation) -> Result<()> {
    if observation.timed_out || observation.exit_code != Some(0) {
        bail!(
            "command failed: exit={:?}, timeout={}, argv={:?}",
            observation.exit_code,
            observation.timed_out,
            observation.argv
        );
    }
    Ok(())
}

/// The only arithmetic needed by a time experiment: sum self-reported operation
/// durations. Setup/validation performed by the callback is not part of them.
pub(crate) fn measured_total(
    iters: u64,
    mut operation: impl FnMut() -> Result<Duration>,
) -> Result<Duration> {
    let mut total = Duration::ZERO;
    for _ in 0..iters {
        total = total
            .checked_add(operation()?)
            .context("duration sum overflow")?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_boundary_delay_does_not_pollute_reported_time() {
        let start = Instant::now();
        let total = measured_total(4, || {
            thread::sleep(Duration::from_millis(5));
            Ok(Duration::from_micros(7))
        })
        .unwrap();
        assert_eq!(total, Duration::from_micros(28));
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn errors_stop_iterations_not_zero_samples() {
        let mut calls = 0;
        let result = measured_total(5, || {
            calls += 1;
            if calls == 2 {
                bail!("invalid output");
            }
            Ok(Duration::from_nanos(11))
        });
        assert!(result.is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn zero_iterations_do_not_invoke_operation() {
        assert_eq!(
            measured_total(0, || panic!("not requested")).unwrap(),
            Duration::ZERO
        );
    }

    #[test]
    #[cfg(unix)]
    fn reaped_identity_never_authorizes_a_numeric_group_signal() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        assert!(!kill_owned_group(pid));
    }

    #[test]
    #[cfg(unix)]
    fn pipe_without_eof_has_its_own_finite_deadline() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"partial evidence").unwrap();
        let start = Instant::now();
        let (bytes, _, eof) = drain_pipe(reader, start, Duration::from_millis(30)).unwrap();
        assert_eq!(bytes, b"partial evidence");
        assert!(!eof);
        assert!(start.elapsed() < Duration::from_secs(1));
        // Writer deliberately remains alive through the assertion: neither an
        // escaped pipe holder nor a stalled service can force an unbounded join.
        drop(writer);
    }

    #[test]
    fn failed_commands_are_not_valid_samples() {
        let observed = observe(
            Command::new("sh").args(["-c", "printf bad; exit 19"]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(observed.stdout, b"bad");
        assert!(require_success(&observed).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn timeout_cleans_descendant_pipe_holders() {
        let start = Instant::now();
        let observed = observe(
            Command::new("sh").args(["-c", "sleep 30 & wait"]),
            Duration::from_millis(30),
        )
        .unwrap();
        assert!(observed.timed_out);
        assert!(require_success(&observed).is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    #[cfg(unix)]
    fn exited_parent_does_not_let_descendant_hold_pipe_forever() {
        let start = Instant::now();
        let observed = observe(
            Command::new("sh").args(["-c", "sleep 30 & exit 0"]),
            Duration::from_millis(30),
        )
        .unwrap();
        assert!(observed.timed_out);
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn successful_leader_cleans_background_descendant_without_open_pipes() {
        let record = observe(
            Command::new("sh").args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!"]),
            Duration::from_secs(2),
        )
        .unwrap();
        require_success(&record).unwrap();
        let pid: u32 = String::from_utf8(record.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            let gone = stat
                .map(|s| {
                    s.split(')')
                        .nth(1)
                        .unwrap_or("")
                        .trim_start()
                        .starts_with('Z')
                })
                .unwrap_or(true);
            if gone {
                break;
            }
            assert!(Instant::now() < deadline, "owned descendant remained alive");
            thread::sleep(Duration::from_millis(1));
        }
    }
}
