//! Linux event observer. The start precedes spawn; Ready is timestamped when
//! its complete newline-delimited record is read, Exit when waitid observes
//! normal child termination after both pipes drain, matching the original
//! observer's completion boundary. Post-observation validation is excluded.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub(super) struct Observation {
    pub ready_ns: u64,
    pub exit_ns: u64,
    pub exit_code: i32,
    pub events: Vec<Event>,
}

#[derive(Debug, Serialize)]
pub(super) struct Event {
    stream: &'static str,
    observed_ns: u64,
    offset: usize,
    bytes: usize,
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        // The child created its own process group before exec. Never target
        // the observer's group, even when spawn failed or the child exited.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn elapsed(start: Instant) -> Result<u64> {
    start
        .elapsed()
        .as_nanos()
        .try_into()
        .context("event clock overflow")
}

fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0,
        "fcntl GETFL: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "fcntl SETFL: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

/// Inspect a terminated child without releasing its PID. The unreaped child
/// pins the numeric process-group identity until the guard kills descendants
/// and reaps it, so delayed evidence writes cannot target a reused PGID.
pub(super) fn exited(child: &Child) -> Result<Option<ExitStatus>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error.into());
    }
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    let status = unsafe { info.si_status() };
    let raw = match info.si_code {
        libc::CLD_EXITED => status << 8,
        libc::CLD_KILLED => status,
        libc::CLD_DUMPED => status | 0x80,
        _ => bail!("unexpected child wait event"),
    };
    Ok(Some(ExitStatus::from_raw(raw)))
}

pub(super) fn capture(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    directory: &Path,
    marker: &[u8],
    timeout: Duration,
) -> Result<Observation> {
    ensure!(!argv.is_empty(), "empty subject command");
    fs::write(
        directory.join("environment.json"),
        serde_json::to_vec_pretty(env)?,
    )?;
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(cwd).envs(env);
    observe(command, directory, marker, timeout)
}

pub(crate) fn guest_command_start(cmd: Command, dir: &Path) -> Result<Duration> {
    const START: &str = "VM_GUEST_COMMAND_STARTED";
    const OK: &str = "VM_GUEST_COMMAND_OK";
    let sample = observe(cmd, dir, START.as_bytes(), Duration::from_secs(30))?;
    let stdout = fs::read_to_string(dir.join("stdout"))?;
    ensure!(
        stdout
            .lines()
            .filter(|line| *line == START || *line == OK)
            .eq([START, OK]),
        "expected guest command entry followed by successful validation"
    );
    Ok(Duration::from_nanos(sample.ready_ns))
}

fn observe(
    mut command: Command,
    directory: &Path,
    marker: &[u8],
    timeout: Duration,
) -> Result<Observation> {
    ensure!(!marker.contains(&b'\n'), "marker must exclude newline");
    let argv: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy())
        .collect();
    fs::write(
        directory.join("argv.json"),
        serde_json::to_vec_pretty(&argv)?,
    )?;
    let mut outlog = File::create(directory.join("stdout"))?;
    let mut errlog = File::create(directory.join("stderr"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // No allocation, logging or fixture setup between this timestamp and spawn.
    let start = Instant::now();
    let mut process = Process(command.spawn().context("spawn measured subject")?);
    let mut stdout = process.0.stdout.take().context("stdout pipe")?;
    let mut stderr = process.0.stderr.take().context("stderr pipe")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let mut out_open = true;
    let mut err_open = true;
    let mut exit = None;
    let mut ready = Vec::new();
    let mut line = Vec::new();
    let mut events = Vec::new();
    let mut offsets = [0, 0];
    let result = (|| {
        while exit.is_none() || out_open || err_open {
            ensure!(
                start.elapsed() < timeout,
                "subject exceeded {}s timeout",
                timeout.as_secs_f64()
            );
            let mut polls = [
                libc::pollfd {
                    fd: if out_open { stdout.as_raw_fd() } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if err_open { stderr.as_raw_fd() } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let polled = unsafe { libc::poll(polls.as_mut_ptr(), 2, 1) };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            for index in 0..2 {
                if polls[index].revents == 0 {
                    continue;
                }
                let stream: &mut dyn Read = if index == 0 { &mut stdout } else { &mut stderr };
                let log: &mut dyn Write = if index == 0 { &mut outlog } else { &mut errlog };
                let mut buffer = [0; 8192];
                // One bounded read per ready stream prevents a noisy stderr
                // producer from starving stdout/timeout/exit observation.
                match stream.read(&mut buffer) {
                    Ok(0) => {
                        if index == 0 {
                            out_open = false;
                        } else {
                            err_open = false;
                        }
                    }
                    Ok(count) => {
                        let observed = elapsed(start)?;
                        events.push(Event {
                            stream: if index == 0 { "stdout" } else { "stderr" },
                            observed_ns: observed,
                            offset: offsets[index],
                            bytes: count,
                        });
                        offsets[index] += count;
                        // Bound runaway output, but retain every byte read.
                        log.write_all(&buffer[..count])?;
                        ensure!(
                            offsets[index] <= 64 * 1024 * 1024,
                            "subject output exceeded 64 MiB"
                        );
                        if index == 0 {
                            for byte in &buffer[..count] {
                                if *byte == b'\n' {
                                    // Guest serial consoles use CRLF. Match
                                    // the historical observer's line framing,
                                    // not a marker with extra content.
                                    while line.last() == Some(&b'\r') {
                                        line.pop();
                                    }
                                    if line == marker {
                                        ready.push(observed);
                                    }
                                    line.clear();
                                } else {
                                    line.push(*byte);
                                    ensure!(line.len() <= 1024 * 1024, "stdout line exceeds 1 MiB");
                                }
                            }
                        }
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
            // Like the original protocol, observe normal completion after
            // draining both child pipes. A short-lived child may have buffered
            // multiple reads of boot output before Ready; do not incorrectly
            // timestamp Exit before that marker is observable.
            if exit.is_none() && !out_open && !err_open {
                if let Some(status) = exited(&process.0)? {
                    exit = Some((elapsed(start)?, status));
                }
            }
        }
        let (exit_ns, status) = exit.context("missing normal exit")?;
        ensure!(
            status.success(),
            "subject did not exit normally with zero: {status}"
        );
        ensure!(
            ready.len() == 1,
            "expected one exact Ready line, got {}",
            ready.len()
        );
        // Never silently clamp or reorder the two observed clocks.
        let ready_ns = ready[0];
        ensure!(
            ready_ns > 0 && ready_ns <= exit_ns,
            "Ready was not observed before Exit"
        );
        Ok(Observation {
            ready_ns,
            exit_ns,
            exit_code: 0,
            events: std::mem::take(&mut events),
        })
    })();
    outlog.flush()?;
    errlog.flush()?;
    match result {
        Ok(observation) => {
            fs::write(
                directory.join("timing.json"),
                serde_json::to_vec_pretty(&observation)?,
            )?;
            Ok(observation)
        }
        Err(error) => {
            fs::write(
                directory.join("observer-failure.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"error":format!("{error:#}"),"events":events,
                    "elapsed_ns":elapsed(start)?,"ready_observations_ns":ready}),
                )?,
            )?;
            bail!("{error:#}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observe(script: &str, timeout: Duration) -> Result<Observation> {
        let directory = tempfile::tempdir()?;
        capture(
            &["/bin/sh".into(), "-c".into(), script.into()],
            directory.path(),
            &BTreeMap::new(),
            directory.path(),
            b"REFERENCE_READY",
            timeout,
        )
    }
    #[test]
    fn ready_precedes_normal_exit() {
        let value = observe(
            "printf 'REFERENCE_READY\\n'; sleep .02",
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(value.ready_ns < value.exit_ns);
    }
    #[test]
    fn command_start_is_between_vmm_ready_and_command_completion() {
        let launch = |script: String| {
            let directory = tempfile::tempdir().unwrap();
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", &script]);
            let measured = guest_command_start(cmd, directory.path())?;
            let timing: serde_json::Value =
                serde_json::from_slice(&fs::read(directory.path().join("timing.json"))?)?;
            Ok::<_, anyhow::Error>((
                measured,
                Duration::from_nanos(timing["exit_ns"].as_u64().unwrap()),
            ))
        };
        let (entry, exit) = launch("echo 'VMM running'; sleep .1; echo VM_GUEST_COMMAND_STARTED; sleep .1; echo VM_GUEST_COMMAND_OK".into()).unwrap();
        assert!(entry >= Duration::from_millis(80));
        assert!(exit - entry >= Duration::from_millis(80));
        for script in [
            "echo 'VMM running'; echo VM_GUEST_COMMAND_OK",
            "echo VM_GUEST_COMMAND_STARTED >&2; echo VM_GUEST_COMMAND_OK",
            "echo VM_GUEST_COMMAND_STARTED; echo VM_GUEST_COMMAND_STARTED; echo VM_GUEST_COMMAND_OK",
            "echo VM_GUEST_COMMAND_STARTED",
            "echo VM_GUEST_COMMAND_OK; echo VM_GUEST_COMMAND_STARTED",
            "echo VM_GUEST_COMMAND_STARTED; echo VM_GUEST_COMMAND_OK; exit 1",
        ] {
            assert!(launch(script.into()).is_err());
        }
    }
    #[test]
    fn serial_crlf_and_buffered_boot_output_preserve_ready_before_exit() {
        observe("i=0; while test $i -lt 3000; do printf 'boot line\\n'; i=$((i+1)); done; printf 'REFERENCE_READY\\r\\n'",
            Duration::from_secs(3)).unwrap();
    }
    #[test]
    fn duplicate_nonzero_and_partial_markers_fail() {
        for script in [
            "printf 'REFERENCE_READY\\nREFERENCE_READY\\n'; sleep .01",
            "printf 'REFERENCE_READY\\n'; exit 9",
            "printf REFERENCE_READY",
            "printf 'REFERENCE_READY extra\\n'",
        ] {
            assert!(observe(script, Duration::from_secs(1)).is_err(), "{script}");
        }
    }
    #[test]
    fn timeout_kills_pipe_holding_descendants() {
        assert!(observe("sleep 10 & wait", Duration::from_millis(20)).is_err());
    }
    #[test]
    fn exit_observation_pins_child_pid_until_owned_group_cleanup() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let process = Process(child);
        let deadline = Instant::now() + Duration::from_secs(1);
        while exited(&process.0).unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let stat = fs::read_to_string(format!("/proc/{}/stat", process.0.id())).unwrap();
        assert_eq!(
            stat.rsplit_once(')').unwrap().1.split_whitespace().next(),
            Some("Z")
        );
        // A second inspection still owns the same child, not ECHILD after reap.
        assert!(exited(&process.0).unwrap().unwrap().success());
        drop(process);
    }
    #[test]
    fn drains_both_pipes_without_deadlock() {
        observe("i=0; while test $i -lt 4000; do printf 'stderr data\\n' >&2; i=$((i+1)); done; printf 'REFERENCE_READY\\n'; sleep .02",
            Duration::from_secs(3)).unwrap();
    }
}
