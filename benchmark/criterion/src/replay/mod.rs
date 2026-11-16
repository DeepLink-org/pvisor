//! B-REPLAY: native preparation fidelity and command-only CLI durations.
//!
//! Criterion owns sampling; every CLI invocation, including calibration and the
//! independent preflight, retains its raw output, state, duration and audit.
//! No model-free workload runner or Python process participates.

mod checks;
mod fixtures;

use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn save(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

struct Batch {
    root: PathBuf,
    binary: PathBuf,
    binary_sha: String,
}

impl Batch {
    fn create() -> Result<Self> {
        let binary = PathBuf::from(
            env::var_os("PVISOR_BENCH_BINARY")
                .context("replay requires PVISOR_BENCH_BINARY (prebuilt pvisor-replay)")?,
        )
        .canonicalize()?;
        let output = env::var_os("PVISOR_BENCH_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../replay/.data")
                    .join(format!("criterion-{}-{}", now_ns(), std::process::id()))
            });
        Self::create_at(&binary, &output)
    }

    fn create_at(binary: &Path, output: &Path) -> Result<Self> {
        ensure!(binary.is_file(), "replay binary is not a regular file");
        if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        // Never reuse a batch or overwrite evidence.
        fs::create_dir(output)
            .with_context(|| format!("create fresh batch {}", output.display()))?;
        let root = output.canonicalize()?;
        let frozen = root.join("pvisor-replay");
        let binary_sha = checks::digest(binary)?;
        fs::copy(binary, &frozen)?;
        ensure!(
            checks::digest(&frozen)? == binary_sha,
            "binary changed while freezing"
        );
        let batch = Self {
            root,
            binary: frozen,
            binary_sha,
        };
        let source_dir = batch.root.join("harness");
        fs::create_dir(&source_dir)?;
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("checks.rs", include_str!("checks.rs")),
            ("fixtures.rs", include_str!("fixtures.rs")),
        ];
        let mut harness = serde_json::Map::new();
        for (name, text) in sources {
            let path = source_dir.join(name);
            fs::write(&path, text)?;
            harness.insert(name.into(), json!(checks::digest(&path)?));
        }
        save(
            &batch.root.join("manifest.json"),
            &json!({
                "schema":"pvisor.criterion.replay/v1", "benchmark":"B-REPLAY",
                "created_unix_ns":now_ns().to_string(),
                "binary_source_path":binary, "binary_sha256":batch.binary_sha,
                "binary_source_build_receipt":null,
                "source_build_claim":"byte identity only; no claim this binary was built from current source",
                "harness_sha256":harness,
                "fixture_scope":"one fixed synthetic task 0 per adapter, not historical twenty-task fidelity",
                "agents":fixtures::AGENTS,
                "criterion_sample_size":60, "criterion_sampling_mode":"flat",
                "operation_count":"adaptive; all preflight/calibration/measurement commands archived",
                "timing_scope":"Instant around direct command spawn through wait; setup, validation and archival excluded",
                "preflight_timeout_s":30, "operation_timeout_s":120,
                "host_os":env::consts::OS, "host_arch":env::consts::ARCH,
                "host_kernel":fs::read_to_string("/proc/sys/kernel/osrelease").ok(),
                "host_cpu":fs::read_to_string("/proc/cpuinfo").ok(),
                "process_status":fs::read_to_string("/proc/self/status").ok(),
                "criterion_cohort":"distinct from historical hyperfine and Python perf_counter_ns",
            }),
        )?;
        eprintln!("B-REPLAY raw evidence: {}", batch.root.display());
        Ok(batch)
    }

    fn fixture(&self, agent: &'static str) -> Result<Fixture<'_>> {
        let root = self.root.join("fixtures").join(agent);
        fs::create_dir_all(root.join("workspace"))?;
        fs::write(root.join("workspace/existing"), fixtures::SENTINEL)?;
        fs::write(root.join("trajectory.json"), fixtures::encoded(agent))?;
        let source_sha = checks::digest(&root.join("trajectory.json"))?;
        save(
            &root.join("protocol.json"),
            &json!({
                "agent":agent, "task":0, "command":fixtures::COMMAND,
                "next_command":fixtures::NEXT_COMMAND, "sha256":source_sha
            }),
        )?;
        let argv = vec![
            self.binary.to_string_lossy().into_owned(),
            "--agent".into(),
            agent.into(),
            "--trajectory".into(),
            root.join("trajectory.json").to_string_lossy().into_owned(),
            "--after-step".into(),
            "1".into(),
            "--prepare-only".into(),
            "--workspace".into(),
            root.join("workspace").to_string_lossy().into_owned(),
            "--state-dir".into(),
            root.join("state").to_string_lossy().into_owned(),
            "--output-dir".into(),
            root.join("output").to_string_lossy().into_owned(),
        ];
        save(
            &root.join("command.json"),
            &json!({"argv":argv, "cwd":self.root}),
        )?;
        Ok(Fixture {
            batch: self,
            root,
            agent,
            source_sha,
            argv,
            operation: 0,
        })
    }
}

struct Fixture<'a> {
    batch: &'a Batch,
    root: PathBuf,
    agent: &'static str,
    source_sha: String,
    argv: Vec<String>,
    operation: u64,
}

impl Fixture<'_> {
    fn invoke(&mut self, preflight: bool) -> Result<Duration> {
        self.operation += 1;
        let trial = self
            .batch
            .root
            .join("trials")
            .join(self.agent)
            .join(format!("{:08}", self.operation));
        fs::create_dir_all(&trial)?;
        // Write intent before any checks or process creation so failures are visible.
        let mut record = json!({
            "operation":self.operation, "kind":if preflight {"preflight"} else {"criterion"},
            "argv":self.argv, "cwd":self.batch.root, "started_unix_ns":now_ns().to_string(),
            "correctness":"pending", "elapsed_ns":null,
            "timeout_s":if preflight {30} else {120},
        });
        save(&trial.join("operation.json"), &record)?;
        let run = (|| -> Result<Duration> {
            checks::prepare(&self.root, &self.source_sha)?;
            ensure!(
                checks::digest(&self.batch.binary)? == self.batch.binary_sha,
                "frozen binary changed"
            );
            let mut command = Command::new(&self.argv[0]);
            command
                .args(&self.argv[1..])
                .current_dir(&self.batch.root)
                .stdin(Stdio::null())
                .stdout(fs::File::create(trial.join("stdout.log"))?)
                .stderr(fs::File::create(trial.join("stderr.log"))?);
            let invocation = command_duration(
                command,
                Duration::from_secs(if preflight { 30 } else { 120 }),
            )?;
            record["elapsed_ns"] = json!(invocation.elapsed.as_nanos().to_string());
            record["exit_code"] = json!(invocation.status.code());
            record["exit_status"] = json!(invocation.status.to_string());
            record["timed_out"] = json!(invocation.timed_out);
            // Persist exit and time even if JSON validation fails.
            save(&trial.join("operation.json"), &record)?;
            ensure!(!invocation.timed_out, "replay command timed out");
            ensure!(
                invocation.status.success(),
                "replay command failed: {}",
                invocation.status
            );
            let profile = checks::verify(&self.root, self.agent, &self.source_sha)?;
            ensure!(
                checks::digest(&self.batch.binary)? == self.batch.binary_sha,
                "frozen binary changed"
            );
            save(
                &trial.join("audit.json"),
                &json!({
                    "profile":profile, "correctness":"passed", "executed_tools":0,
                    "agent_started":false, "workspace_unchanged":true,
                    "source_sha256":self.source_sha, "binary_sha256":self.batch.binary_sha,
                    "complete_tool_batch":true, "after_step":1,
                }),
            )?;
            Ok(invocation.elapsed)
        })();
        // Archive partial output/state on failure too; never delete failed evidence.
        let archive = (|| -> Result<()> {
            for name in ["output", "state"] {
                let path = self.root.join(name);
                if path.try_exists()? {
                    fs::rename(path, trial.join(name))?;
                }
            }
            Ok(())
        })();
        record["finished_unix_ns"] = json!(now_ns().to_string());
        record["correctness"] = json!(if run.is_ok() && archive.is_ok() {
            "passed"
        } else {
            "failed"
        });
        if let Err(error) = &run {
            record["error"] = json!(format!("{error:#}"));
        }
        if let Err(error) = &archive {
            record["archive_error"] = json!(format!("{error:#}"));
        }
        save(&trial.join("operation.json"), &record)?;
        archive?;
        run.with_context(|| {
            format!(
                "{} operation {}; evidence {}",
                self.agent,
                self.operation,
                trial.display()
            )
        })
    }
}

struct Invocation {
    elapsed: Duration,
    status: ExitStatus,
    timed_out: bool,
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    // The child starts a new process group. Kill only that invocation's group,
    // including descendants left behind after its direct child exits. The
    // caller must keep the leader unreaped until this signal has been sent.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(unix)]
fn wait_for_exit_without_reaping(pid: u32) -> std::io::Result<()> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
        // Observe exit without releasing the leader's PID/PGID identity. The
        // blocking wait includes no polling delay; Child::wait reaps later.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
fn command_duration(mut command: Command, timeout: Duration) -> Result<Invocation> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let (start_tx, start_rx) = mpsc::channel::<u32>();
    let (done_tx, done_rx) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timed_out_thread = Arc::clone(&timed_out);
    // Supervisor creation is untimed. The measured path uses blocking wait,
    // not polling/sleeping, which would inflate short CLI measurements.
    let supervisor = std::thread::spawn(move || {
        if let Ok(pid) = start_rx.recv()
            && matches!(
                done_rx.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            )
        {
            timed_out_thread.store(true, Ordering::SeqCst);
            kill_group(pid);
        }
    });
    let started = Instant::now();
    let child = command.spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            drop(start_tx);
            supervisor.join().expect("replay supervisor panicked");
            return Err(error).context("spawn replay command");
        }
    };
    let pid = child.id();
    let _ = start_tx.send(pid);
    let observed_exit = wait_for_exit_without_reaping(pid);
    let elapsed = started.elapsed();
    let _ = done_tx.send(());
    supervisor.join().expect("replay supervisor panicked");
    // Stop all signalling before reaping: an exited but unreaped leader keeps
    // its PID (and therefore this PGID) from being recycled. Cleanup/reaping
    // remain outside elapsed. ECHILD means the identity is no longer ours;
    // never signal a group by a potentially recycled number in that case.
    if !matches!(&observed_exit, Err(error) if error.raw_os_error() == Some(libc::ECHILD)) {
        kill_group(pid);
    }
    let status = child.wait();
    observed_exit.context("observe replay command exit without reaping")?;
    Ok(Invocation {
        elapsed,
        status: status?,
        timed_out: timed_out.load(Ordering::SeqCst),
    })
}

#[cfg(not(unix))]
fn command_duration(_command: Command, _timeout: Duration) -> Result<Invocation> {
    anyhow::bail!("B-REPLAY requires Unix process-group supervision; unsupported on this platform")
}

/// Register the seven B-REPLAY native preparation adapters.
///
/// Requires a prebuilt `PVISOR_BENCH_BINARY`. `PVISOR_BENCH_OUTPUT`, when set,
/// must name a fresh evidence directory. Panics on setup, command, timeout or
/// fidelity failure, retaining the batch for inspection; never invokes Python.
pub fn register(criterion: &mut Criterion) {
    let batch = Batch::create().expect("B-REPLAY batch preparation failed");
    let mut fixtures = fixtures::AGENTS
        .iter()
        .map(|&agent| {
            batch
                .fixture(agent)
                .expect("B-REPLAY fixture preparation failed")
        })
        .collect::<Vec<_>>();
    for fixture in &mut fixtures {
        fixture.invoke(true).expect("B-REPLAY preflight failed");
    }
    let mut group = criterion.benchmark_group("replay");
    group.sample_size(60).sampling_mode(SamplingMode::Flat);
    for fixture in &mut fixtures {
        group.bench_function(fixture.agent, |bencher| {
            bencher.iter_custom(|iterations| {
                let mut total = Duration::ZERO;
                for _ in 0..iterations {
                    total += fixture.invoke(false).expect("B-REPLAY operation failed");
                }
                total
            });
        });
    }
    group.finish();
}

#[cfg(test)]
mod tests;
