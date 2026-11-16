//! Shared process supervision and evidence; all paths are experiment-owned.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn save(path: impl AsRef<Path>, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
pub fn load(path: impl AsRef<Path>) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn sha(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut block = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn required(name: &str) -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var(name).with_context(|| format!("{name} is required"))?)
            .canonicalize()?,
    )
}
pub fn output() -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var("PVISOR_BENCH_OUTPUT")
            .or_else(|_| std::env::var("PVISOR_CRITERION_OUTPUT"))
            .context("set PVISOR_BENCH_OUTPUT to a NEW .data directory")?,
    );
    ensure!(
        path.components().any(|c| c.as_os_str() == ".data"),
        "raw evidence must be under .data"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(&path).context("output must not already exist")?;
    Ok(path.canonicalize()?)
}
pub fn clean(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("PVISOR_")
            || name.starts_with("WORKLOADS_V2_")
            || name.to_ascii_lowercase().ends_with("_proxy")
            || ["OPENAI_", "ANTHROPIC_", "CLAUDE_"]
                .iter()
                .any(|p| name.starts_with(p))
        {
            command.env_remove(key);
        }
    }
    command.env("LC_ALL", "C").env("RUST_LOG", "off");
}
/// A process-group owner that pins the leader until group teardown.
///
/// The child is private: callers may observe exit without reaping, but only
/// cleanup may reap the leader and release its PID/PGID for reuse.
pub struct OwnedChild {
    child: std::process::Child,
    cleaned: bool,
}
fn wait_status(pid: u32, nonblocking: bool) -> std::io::Result<Option<std::process::ExitStatus>> {
    use std::os::unix::process::ExitStatusExt;
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let flags = libc::WEXITED | libc::WNOWAIT | if nonblocking { libc::WNOHANG } else { 0 };
        if unsafe { libc::waitid(libc::P_PID, pid, &mut info, flags) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            _ => {
                return Err(std::io::Error::other(
                    "unexpected non-exit waitid notification",
                ));
            }
        };
        return Ok(Some(std::process::ExitStatus::from_raw(raw)));
    }
}
impl OwnedChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }
    pub fn try_status(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        wait_status(self.id(), true)
    }
    pub fn cleanup(&mut self) -> std::io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        // ECHILD means an external reaper broke ownership. Do not signal a
        // potentially recycled PGID when we can no longer prove the pin.
        self.try_status()?;
        self.cleaned = true;
        unsafe {
            libc::kill(-(self.id() as i32), libc::SIGKILL);
        }
        self.child.wait()?;
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
pub fn spawn(command: &mut Command) -> Result<OwnedChild> {
    command.process_group(0);
    Ok(OwnedChild {
        child: command.spawn()?,
        cleaned: false,
    })
}
pub fn builders(names: &[&str]) -> Result<Vec<Value>> {
    let mut found = vec![];
    for item in fs::read_dir("/proc")? {
        let path = item?.path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .parse::<u32>()
            .is_err()
        {
            continue;
        }
        if let Ok(comm) = fs::read_to_string(path.join("comm")) {
            if names.contains(&comm.trim()) {
                found.push(json!({"pid":path.file_name().unwrap().to_string_lossy(), "command":comm.trim()}));
            }
        }
    }
    Ok(found)
}
fn thread_observations(root: u32) -> Vec<Value> {
    let mut pending = vec![root];
    let mut seen = std::collections::BTreeSet::new();
    let mut threads = vec![];
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for task in tasks.flatten() {
            if let Ok(children) = fs::read_to_string(task.path().join("children")) {
                pending.extend(
                    children
                        .split_whitespace()
                        .filter_map(|s| s.parse::<u32>().ok()),
                );
            }
            if let Ok(stat) = fs::read_to_string(task.path().join("stat")) {
                threads
                    .push(json!({"pid":pid,"tid":task.file_name().to_string_lossy(),"stat":stat}));
            }
        }
    }
    threads
}
/// Capture raw logs without pipe deadlocks, bound the process tree, and retain failures.
pub fn run(
    command: &mut Command,
    folder: &Path,
    timeout: Duration,
    gate: &[&str],
) -> Result<(Duration, String)> {
    fs::create_dir_all(folder)?;
    save(
        folder.join("command.json"),
        &json!({"program":command.get_program().to_string_lossy(),
        "argv":command.get_args().map(|s| s.to_string_lossy().into_owned()).collect::<Vec<_>>()}),
    )?;
    command
        .stdin(Stdio::null())
        .stdout(File::create(folder.join("stdout.log"))?)
        .stderr(File::create(folder.join("stderr.log"))?);
    let start = Instant::now();
    let mut process = spawn(command)?;
    let mut observations = vec![];
    let pid = process.id();
    let mut elapsed = Duration::ZERO;
    // The waiter stamps actual completion, not the next monitor tick. Polling
    // would otherwise add up to 50ms to every tiny native probe.
    let outcome = std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let status = wait_status(pid, false).and_then(|status| {
                status.ok_or_else(|| std::io::Error::other("blocking waitid returned no exit"))
            });
            let _ = tx.send((status, start.elapsed()));
        });
        let outcome = loop {
            if !gate.is_empty() {
                match builders(gate) {
                    Ok(active) if !active.is_empty() => {
                        break Err(anyhow::anyhow!(
                            "external build/test interference: {active:?}"
                        ));
                    }
                    Err(error) => break Err(error),
                    _ => {}
                }
            }
            observations.push(json!({"elapsed_ns":start.elapsed().as_nanos(),
                "loadavg":fs::read_to_string("/proc/loadavg").ok(),
                "status":fs::read_to_string(format!("/proc/{pid}/status")).ok(),
                "stat":fs::read_to_string(format!("/proc/{pid}/stat")).ok(),
                "threads":thread_observations(pid)}));
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((status, wall)) => {
                    elapsed = wall;
                    break match status {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => Err(anyhow::anyhow!("subject failed: {status}")),
                        Err(error) => Err(error.into()),
                    };
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!("waiter disconnected"));
                }
                Err(_) => {}
            }
            if start.elapsed() >= timeout {
                break Err(anyhow::anyhow!("subject timeout"));
            }
        };
        if outcome.is_err() {
            let _ = process.cleanup();
        }
        outcome
    });
    if elapsed.is_zero() {
        elapsed = start.elapsed();
    }
    drop(process);
    save(folder.join("observations.json"), &json!(observations))?;
    save(
        folder.join("exit.json"),
        &json!({"elapsed_ns":elapsed.as_nanos(),"valid":outcome.is_ok(),"error":outcome.as_ref().err().map(ToString::to_string)}),
    )?;
    outcome?;
    Ok((elapsed, fs::read_to_string(folder.join("stdout.log"))?))
}
pub fn checked(command: &mut Command) -> Result<String> {
    // Even preparation/export commands are bounded and their descendants reaped.
    let folder = tempfile::tempdir()?;
    match run(command, folder.path(), Duration::from_secs(600), &[]) {
        Ok((_, text)) => Ok(text),
        Err(error) => {
            let retained = folder.keep();
            Err(error.context(format!(
                "preparation failure logs retained at {}",
                retained.display()
            )))
        }
    }
}
pub fn records(text: &str, prefix: &str) -> Result<Vec<Value>> {
    text.lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
pub fn unique(text: &str, prefix: &str) -> Result<Value> {
    let mut rows = records(text, prefix)?;
    ensure!(rows.len() == 1, "expected unique {prefix} marker");
    Ok(rows.remove(0))
}
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.next() as usize % (i + 1));
        }
    }
}
pub fn budget(root: Option<&Path>, nofile: bool) -> Result<Value> {
    let relative = fs::read_to_string("/proc/self/cgroup")?;
    let own = PathBuf::from("/sys/fs/cgroup").join(
        relative
            .trim()
            .split_once("::")
            .context("cgroup v2 required")?
            .1
            .trim_start_matches('/'),
    );
    let root = root.unwrap_or(&own);
    ensure!(own.starts_with(root), "observer escaped parent budget");
    let cpu = fs::read_to_string(root.join("cpu.max"))?;
    let mem = fs::read_to_string(root.join("memory.max"))?;
    let swap = fs::read_to_string(root.join("memory.swap.max"))?;
    ensure!(
        cpu.trim() == "200000 100000" && mem.trim() == "2147483648" && swap.trim() == "0",
        "two CPU/2GiB/swap0 parent required"
    );
    let mut mask = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    ensure!(
        unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask) } == 0,
        "getaffinity"
    );
    let cpus: Vec<_> = (0..libc::CPU_SETSIZE as usize)
        .filter(|i| unsafe { libc::CPU_ISSET(*i, &mask) })
        .collect();
    ensure!(cpus == [0, 1], "CPU affinity must be 0,1");
    if nofile {
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        ensure!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } == 0
                && limits.rlim_cur == 65536
                && limits.rlim_max == 65536,
            "nofile 65536 required"
        );
    }
    Ok(
        json!({"cgroup":root,"cpu.max":cpu.trim(),"memory.max":mem.trim(),"memory.swap.max":swap.trim(),
        "memory.events":fs::read_to_string(root.join("memory.events"))?,"affinity":cpus}),
    )
}
pub fn unchanged_events(before: &Value, after: &Value) -> Result<()> {
    let parse = |v: &Value| {
        v["memory.events"]
            .as_str()
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let a = parse(before);
    let b = parse(after);
    for name in ["oom ", "oom_kill ", "oom_group_kill "] {
        ensure!(
            a.iter().find(|s| s.starts_with(name)) == b.iter().find(|s| s.starts_with(name)),
            "OOM events occurred"
        );
    }
    Ok(())
}
pub fn verify_hashes(root: &Path, hashes: &Value) -> Result<()> {
    let map = hashes
        .as_object()
        .context("hash inventory must be object")?;
    for (relative, digest) in map {
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            bail!("artifact path escaped batch");
        }
        ensure!(
            sha(&root.join(path))? == digest.as_str().context("digest")?,
            "artifact changed: {relative}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leader_exit_status_is_nonreaping_until_idempotent_group_cleanup() {
        let mut child = spawn(Command::new("sh").args(["-c", "exit 7"])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let observed = loop {
            if let Some(status) = child.try_status().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(observed.code(), Some(7));
        assert_eq!(child.try_status().unwrap().unwrap().code(), Some(7));
        assert!(!child.cleaned);
        let stat = fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
        assert!(stat.rsplit_once(") ").unwrap().1.starts_with("Z "));
        child.cleanup().unwrap();
        assert!(child.cleaned);
        assert_eq!(
            wait_status(child.id(), true).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        // No second signal is sent after wait() released the leader's PID.
        child.cleanup().unwrap();
    }
    #[test]
    fn nonreaping_status_preserves_signal_exit() {
        use std::os::unix::process::ExitStatusExt;
        let child = spawn(Command::new("sh").args(["-c", "kill -TERM $$"])).unwrap();
        let status = wait_status(child.id(), false).unwrap().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(!status.success());
    }
    #[test]
    fn successful_leader_exit_does_not_leave_background_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let (_, text) = run(
            Command::new("sh").args(["-c", "sleep 30 & echo $!; exit 0"]),
            temp.path(),
            Duration::from_secs(1),
            &[],
        )
        .unwrap();
        let pid: u32 = text.trim().parse().unwrap();
        for _ in 0..100 {
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Ok(stat)
                    if stat.rsplit_once(") ").is_some_and(|(_, rest)| {
                        rest.starts_with("Z ") || rest.starts_with("X ")
                    }) =>
                {
                    return;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("background descendant {pid} survived successful leader teardown");
    }
    #[test]
    fn timeout_reaps_owned_process_group_and_keeps_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let start = Instant::now();
        assert!(
            run(
                Command::new("sh").args(["-c", "sleep 30 & wait"]),
                temp.path(),
                Duration::from_millis(100),
                &[]
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(load(temp.path().join("exit.json")).unwrap()["valid"], false);
        assert!(temp.path().join("command.json").is_file());
        assert!(temp.path().join("stderr.log").is_file());
    }
    #[test]
    fn completion_clock_matches_retained_exit_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let (duration, _) = run(
            Command::new("true").args(std::iter::empty::<&str>()),
            temp.path(),
            Duration::from_secs(1),
            &[],
        )
        .unwrap();
        assert!(duration > Duration::ZERO);
        assert_eq!(
            load(temp.path().join("exit.json")).unwrap()["elapsed_ns"],
            json!(duration.as_nanos())
        );
    }
}
