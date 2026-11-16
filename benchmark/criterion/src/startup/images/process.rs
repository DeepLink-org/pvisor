//! Native subprocess clocks, raw evidence, bounded ownership and scheduling.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}
pub(super) fn load(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path)?).with_context(|| path.display().to_string())
}
pub(super) fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) },
        0
    );
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
pub(super) fn nonce() -> Result<String> {
    let mut bytes = [0u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(super) fn clean_command(argv: &[String]) -> Result<Command> {
    ensure!(!argv.is_empty(), "empty argv");
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PVISOR_") {
            cmd.env_remove(key);
        }
    }
    Ok(cmd)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Timing {
    pub valid: bool,
    pub start_ns: u64,
    pub marker_ns: Option<u64>,
    pub end_ns: u64,
    pub ready_ns: Option<u64>,
    pub completion_ns: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}
pub(super) struct Observed {
    pub timing: Timing,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        // A direct child may have exited while its descendants still hold a
        // pipe. The group belongs to this launch even in that case.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn peek_exit(child: &Child) -> Result<Option<std::process::ExitStatus>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
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
    // WNOWAIT leaves the leader unreaped, reserving its PID/PGID until Guard
    // kills any remaining group members and only then reaps the leader.
    let raw = if info.si_code == libc::CLD_EXITED {
        status << 8
    } else {
        status
    };
    Ok(Some(std::process::ExitStatus::from_raw(raw)))
}

/// The marker includes its newline and must be the entire stdout stream.
/// A callback is used only for untimed live-budget preparation checks.
pub(super) fn observe(
    cmd: &mut Command,
    folder: &Path,
    marker: Option<&[u8]>,
    timeout: Duration,
    mut on_ready: Option<&mut dyn FnMut() -> Result<()>>,
) -> Result<Observed> {
    fs::create_dir(folder)?;
    let argv: Vec<String> = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    save(&folder.join("argv.json"), &argv)?;
    let environment: BTreeMap<String, Option<String>> = cmd
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|v| {
                    if key == "PVISOR_CACHE_TOKEN" {
                        "<generated private token>".into()
                    } else {
                        v.to_string_lossy().into_owned()
                    }
                }),
            )
        })
        .collect();
    save(&folder.join("environment.json"), &environment)?;
    save(
        &folder.join("launch.json"),
        &json!({"cwd":cmd.get_current_dir(),
        "timeout_ns":timeout.as_nanos().to_string(),
        "timeout_cleanup":"owned local process group only; remote Docker state requires explicit audit"}),
    )?;
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let start_ns = now_ns();
    let started = Instant::now();
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            save(
                &folder.join("spawn-error.json"),
                &json!({"valid":false,"error":error.to_string()}),
            )?;
            return Err(error.into());
        }
    };
    let mut child = Guard(child);
    let mut out = child.0.stdout.take().context("missing stdout")?;
    let mut err = child.0.stderr.take().context("missing stderr")?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut pipes = [
        libc::pollfd {
            fd: out.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: err.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    for pipe in &pipes {
        let flags = unsafe { libc::fcntl(pipe.fd, libc::F_GETFL) };
        ensure!(
            flags >= 0
                && unsafe { libc::fcntl(pipe.fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "cannot make observation pipe nonblocking"
        );
    }
    let mut marker_ns = None;
    let mut checked_offset = 0;
    let mut failure = None;
    let mut callback_done = false;
    let mut code = None;
    let mut timed_out = false;
    loop {
        if started.elapsed() >= timeout {
            timed_out = true;
            failure = Some("fixed deadline exceeded; remote state may remain".to_owned());
            break;
        }
        let wait = timeout
            .saturating_sub(started.elapsed())
            .as_millis()
            .min(50) as i32;
        let polled = unsafe { libc::poll(pipes.as_mut_ptr(), 2, wait) };
        if polled < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            failure = Some(std::io::Error::last_os_error().to_string());
            break;
        }
        for (index, pipe) in pipes.iter_mut().enumerate() {
            if pipe.fd < 0 || pipe.revents == 0 {
                continue;
            }
            loop {
                let mut block = [0u8; 65536];
                let read = if index == 0 {
                    out.read(&mut block)
                } else {
                    err.read(&mut block)
                };
                match read {
                    Ok(0) => {
                        pipe.fd = -1;
                        break;
                    }
                    Ok(length) => {
                        let received = now_ns();
                        if index == 0 {
                            stdout.extend_from_slice(&block[..length]);
                            if let Some(marker) = marker {
                                while let Some(newline) =
                                    stdout[checked_offset..].iter().position(|b| *b == b'\n')
                                {
                                    let end = checked_offset + newline + 1;
                                    if &stdout[checked_offset..end] == marker
                                        && marker_ns.replace(received).is_some()
                                    {
                                        failure = Some("duplicate exact Ready marker".into());
                                    }
                                    checked_offset = end;
                                }
                            }
                        } else {
                            stderr.extend_from_slice(&block[..length]);
                        }
                        // Bound work per descriptor so a continuous writer
                        // cannot starve stderr, child reaping or the deadline.
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        failure = Some(error.to_string());
                        pipe.fd = -1;
                        break;
                    }
                }
            }
        }
        if marker_ns.is_some() && !callback_done {
            callback_done = true;
            if let Some(callback) = &mut on_ready {
                if let Err(error) = callback() {
                    failure = Some(format!("live validation: {error:#}"));
                }
            }
        }
        if let Some(status) = peek_exit(&child.0)? {
            code = Some(status);
        }
        if failure.is_some() || (code.is_some() && pipes.iter().all(|p| p.fd < 0)) {
            break;
        }
    }
    let end_ns = now_ns();
    let exit_code = code.and_then(|s| s.code());
    let marker_valid = marker.is_none_or(|expected| stdout == expected && marker_ns.is_some());
    let valid = failure.is_none() && exit_code == Some(0) && marker_valid;
    let timing = Timing {
        valid,
        start_ns,
        marker_ns,
        end_ns,
        ready_ns: marker_ns.map(|n| n - start_ns),
        completion_ns: end_ns - start_ns,
        exit_code,
        timed_out,
    };
    fs::write(folder.join("stdout.bin"), &stdout)?;
    fs::write(folder.join("stderr.bin"), &stderr)?;
    save(&folder.join("timing.json"), &timing)?;
    if !valid {
        save(
            &folder.join("failure.json"),
            &json!({"valid":false,"error":failure,"marker_valid":marker_valid,
            "remote_cleanup_verified":false}),
        )?;
        bail!(
            "process/output/exit validation failed: {}",
            folder.display()
        );
    }
    Ok(Observed {
        timing,
        stdout,
        stderr,
    })
}
pub(super) fn command(argv: &[String], folder: &Path, timeout: u64) -> Result<Vec<u8>> {
    Ok(observe(
        &mut clean_command(argv)?,
        folder,
        None,
        Duration::from_secs(timeout),
        None,
    )?
    .stdout)
}
pub(super) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}
pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub(super) fn shell(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}
pub(super) fn cpus(value: &str) -> Result<Vec<usize>> {
    let mut result = Vec::new();
    for range in value.trim().split(',') {
        let (low, high) = range.split_once('-').unwrap_or((range, range));
        let low: usize = low.parse()?;
        let high: usize = high.parse()?;
        ensure!(
            low <= high && high < libc::CPU_SETSIZE as usize,
            "invalid CPU range"
        );
        for cpu in low..=high {
            ensure!(!result.contains(&cpu), "duplicate CPU");
            result.push(cpu);
        }
    }
    result.sort_unstable();
    Ok(result)
}
pub(super) fn affinity() -> Result<Vec<usize>> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set) } == 0,
        "cannot read affinity"
    );
    Ok((0..libc::CPU_SETSIZE as usize)
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
        .collect())
}
pub(super) fn pin(cpus: &[usize]) -> Result<()> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut set);
    }
    for cpu in cpus {
        unsafe {
            libc::CPU_SET(*cpu, &mut set);
        }
    }
    ensure!(
        unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) } == 0,
        "cannot set affinity"
    );
    ensure!(affinity()? == cpus, "CPU affinity readback mismatch");
    Ok(())
}
/// SplitMix64 and balanced shuffled permutations, independent of iteration count.
pub(super) struct Schedule {
    state: u64,
    pending: Vec<Vec<usize>>,
    size: usize,
}
impl Schedule {
    pub(super) fn new(size: usize, seed: u64) -> Self {
        assert!((2..=3).contains(&size));
        Self {
            state: seed,
            pending: Vec::new(),
            size,
        }
    }
    fn random(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    pub(super) fn next(&mut self) -> Vec<usize> {
        if self.pending.is_empty() {
            self.pending = if self.size == 2 {
                vec![vec![0, 1], vec![1, 0]]
            } else {
                vec![
                    vec![0, 1, 2],
                    vec![0, 2, 1],
                    vec![1, 0, 2],
                    vec![1, 2, 0],
                    vec![2, 0, 1],
                    vec![2, 1, 0],
                ]
            };
            for i in (1..self.pending.len()).rev() {
                let j = (self.random() % (i as u64 + 1)) as usize;
                self.pending.swap(i, j);
            }
        }
        self.pending.pop().unwrap()
    }
}
pub(super) fn snapshot(owned: &[u32]) -> Result<Value> {
    let mut rows = BTreeMap::new();
    for path in fs::read_dir("/proc")? {
        let path = path?.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let row = (|| -> Result<Value> {
            let raw = fs::read_to_string(path.join("stat"))?;
            let stat: Vec<_> = raw
                .rsplit_once(')')
                .context("invalid stat")?
                .1
                .split_whitespace()
                .collect();
            let command =
                String::from_utf8_lossy(&fs::read(path.join("cmdline"))?).replace('\0', " ");
            let comm = fs::read_to_string(path.join("comm"))?.trim().to_owned();
            let status = fs::read_to_string(path.join("status"))?;
            let allowed = status
                .lines()
                .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
                .context("no affinity")?
                .trim();
            Ok(
                json!({"ppid":stat[1].parse::<u32>()?,"ticks":stat[11].parse::<u64>()?+stat[12].parse::<u64>()?,
                "starttime":stat[19].parse::<u64>()?,"last_cpu":stat[36].parse::<usize>()?,
                "comm":comm,"command":command,"allowed_cpus":allowed}),
            )
        })();
        if let Ok(row) = row {
            rows.insert(pid.to_string(), row);
        }
    }
    let mut descendants = vec![std::process::id()];
    descendants.extend_from_slice(owned);
    let mut tagged = Vec::new();
    for (pid, row) in &rows {
        if is_builder(row["comm"].as_str().unwrap_or(""))
            || row["command"].as_str().unwrap_or("").contains("benchmark/")
        {
            tagged.push(pid.parse::<u32>()?);
        }
    }
    for set in [&mut descendants, &mut tagged] {
        loop {
            let old = set.len();
            for (pid, row) in &rows {
                let pid = pid.parse::<u32>()?;
                if set.contains(&(row["ppid"].as_u64().unwrap() as u32)) && !set.contains(&pid) {
                    set.push(pid);
                }
            }
            if old == set.len() {
                break;
            }
        }
    }
    rows.retain(|pid, _| {
        let pid = pid.parse::<u32>().unwrap();
        tagged.contains(&pid) && !descendants.contains(&pid)
    });
    Ok(serde_json::to_value(rows)?)
}
fn is_builder(name: &str) -> bool {
    [
        "cargo",
        "rustc",
        "ninja",
        "make",
        "pytest",
        "cargo-nextest",
        "nextest",
        "hyperfine",
    ]
    .contains(&name)
}
pub(super) fn background(before: &Value, after: &Value, cpus: &[usize], path: &Path) -> Result<()> {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    ensure!(ticks > 0, "invalid clock ticks");
    let threshold = (ticks as u64 / 5).max(1);
    let mut flagged = Vec::new();
    for (pid, row) in after.as_object().context("invalid process snapshot")? {
        let old = &before[pid];
        let used = if old["starttime"] == row["starttime"] {
            row["ticks"]
                .as_u64()
                .unwrap()
                .saturating_sub(old["ticks"].as_u64().unwrap_or(0))
        } else {
            row["ticks"].as_u64().unwrap()
        };
        if (is_builder(row["comm"].as_str().unwrap()) && used > 0)
            || (cpus.contains(&(row["last_cpu"].as_u64().unwrap() as usize)) && used >= threshold)
        {
            flagged.push(json!({"pid":pid,"used_ticks":used,"process":row}));
        }
    }
    save(
        path,
        &json!({"before":before,"after":after,"flagged":flagged}),
    )?;
    ensure!(
        flagged.is_empty(),
        "foreign build/benchmark interference invalidates entire cohort"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedules_balance_and_replay() {
        for size in [2, 3] {
            let mut a = Schedule::new(size, 20261018);
            let mut b = Schedule::new(size, 20261018);
            let count = if size == 2 { 2 } else { 6 };
            let mut block = Vec::new();
            for _ in 0..count {
                let next = a.next();
                assert_eq!(next, b.next());
                block.push(next);
            }
            block.sort();
            block.dedup();
            assert_eq!(block.len(), count);
        }
    }
    #[test]
    fn exact_marker_and_normal_exit() {
        let root = tempfile::tempdir().unwrap();
        for (name, script, valid) in [
            ("good", "printf 'READY\\n'", true),
            ("duplicate", "printf 'READY\\nREADY\\n'", false),
            ("exit", "printf 'READY\\n';exit 2", false),
        ] {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            assert_eq!(
                observe(
                    &mut cmd,
                    &root.path().join(name),
                    Some(b"READY\n"),
                    Duration::from_secs(2),
                    None
                )
                .is_ok(),
                valid
            );
            assert!(root.path().join(name).join("timing.json").exists());
        }
    }
    #[test]
    fn timeout_is_invalid() {
        let root = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 10"]);
        assert!(
            observe(
                &mut cmd,
                &root.path().join("timeout"),
                Some(b"READY\n"),
                Duration::from_millis(20),
                None
            )
            .is_err()
        );
        assert_eq!(
            load(&root.path().join("timeout/timing.json")).unwrap()["timed_out"],
            true
        );
    }
}
