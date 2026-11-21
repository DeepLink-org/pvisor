//! Benchmark: B-PROCESS, engineering A/B. Detect minimal Session/Bundle read
//! regressions; independent sequential conditions, not causal paired speedups.
use crate::command::{measured_total, observe, require_success};
use anyhow::{Context, Result, bail, ensure};
use criterion::{Criterion, SamplingMode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PROCESS_SAMPLE_SIZE: usize = 50;
const PROCESS_MEASUREMENT_TIME: Duration = Duration::from_secs(5);

fn read_observation(path: &Path) -> Value {
    match fs::read_to_string(path) {
        Ok(text) => json!({"text":text}),
        Err(error) => json!({"error":error.to_string()}),
    }
}

fn resource_snapshot_at(proc: &Path, cgroups: &Path) -> Value {
    let mut snapshot = json!({
        "unix_ns":SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_nanos()),
        "boot_id":read_observation(&proc.join("sys/kernel/random/boot_id")),
        "uptime":read_observation(&proc.join("uptime")),
        "meminfo":read_observation(&proc.join("meminfo")),
    });
    for resource in ["cpu", "memory", "io"] {
        snapshot["pressure"][resource] = read_observation(&proc.join("pressure").join(resource));
    }
    snapshot["vmstat"] = match fs::read_to_string(proc.join("vmstat")) {
        Ok(text) => json!({"text":text.lines().filter(|line| {
            matches!(line.split_whitespace().next(),
                Some("pswpin" | "pswpout" | "pgmajfault" | "nr_dirty" | "nr_writeback"))
        }).collect::<Vec<_>>().join("\n")}),
        Err(error) => json!({"error":error.to_string()}),
    };
    snapshot["cgroup"] = match fs::read_to_string(proc.join("self/cgroup")) {
        Ok(text) => match text.lines().find_map(|line| line.strip_prefix("0::")) {
            Some(path)
                if path.starts_with('/')
                    && Path::new(path).components().all(|part| {
                        matches!(
                            part,
                            std::path::Component::RootDir | std::path::Component::Normal(_)
                        )
                    }) =>
            {
                let root = cgroups.join(path.trim_start_matches('/'));
                let mut group = json!({"path":path});
                for name in [
                    "cpu.max",
                    "cpu.stat",
                    "cpuset.cpus.effective",
                    "memory.current",
                    "memory.high",
                    "memory.max",
                    "memory.events",
                    "memory.swap.current",
                    "memory.swap.max",
                    "pids.events",
                ] {
                    group[name] = read_observation(&root.join(name));
                }
                group
            }
            _ => json!({"error":"unified cgroup path unavailable"}),
        },
        Err(error) => json!({"error":error.to_string()}),
    };
    snapshot
}

fn resource_snapshot() -> Value {
    resource_snapshot_at(Path::new("/proc"), Path::new("/sys/fs/cgroup"))
}

fn bundle_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    if root.exists() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            let bundle = path.join("run-bundle.json");
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("run-"))
                && bundle.is_file()
            {
                paths.insert(bundle);
            }
        }
    }
    Ok(paths)
}

fn validate_bundle(path: &Path) -> Result<()> {
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        value["run"]["state"] == "completed" && value["run"]["exit_code"] == 0,
        "Bundle must describe successful completion: {}",
        path.display()
    );
    Ok(())
}

struct Suite {
    binary: PathBuf,
    binary_digest: String,
    batch: PathBuf,
    entry: String,
    session: PathBuf,
    checked: BTreeSet<PathBuf>,
    sequence: u64,
    phase: &'static str,
}

impl Suite {
    fn command(&self, condition: &str) -> Result<Command> {
        let mut command = match condition {
            "native" => Command::new("/usr/bin/true"),
            "host-run" => {
                let mut c = Command::new(&self.binary);
                c.args([
                    &self.entry,
                    "--no-agent-defaults",
                    "--overlaynet",
                    "off",
                    "--stdio",
                    "capture",
                    "--",
                    "/usr/bin/true",
                ]);
                c
            }
            "status" | "review" => {
                let mut c = Command::new(&self.binary);
                c.args([condition, "--json"]).arg(&self.session);
                c
            }
            _ => bail!("unknown process condition"),
        };
        command
            .current_dir(self.batch.join("workspace"))
            .env("PVISOR_RUN_HOME", self.batch.join("runs"))
            .env("PVISOR_STARTUP_PROFILE", "0")
            .env("PVISOR_STARTUP_TIMING", "0")
            .env("PVISOR_FS_PROFILE", "0");
        Ok(command)
    }

    fn operation(&mut self, condition: &str) -> Result<Duration> {
        ensure!(
            format!("{:x}", Sha256::digest(fs::read(&self.binary)?)) == self.binary_digest,
            "measured binary changed"
        );
        ensure!(
            fs::read(self.batch.join("workspace/sentinel"))? == b"pvisor benchmark sentinel\n",
            "workspace sentinel changed"
        );
        let mut command = self.command(condition)?;
        let before = resource_snapshot();
        let observation = observe(&mut command, Duration::from_secs(120));
        let after = resource_snapshot();
        self.sequence += 1;
        fs::write(
            self.batch
                .join(format!("resources-{:08}.json", self.sequence)),
            serde_json::to_vec_pretty(&json!({
                "condition":condition, "phase":self.phase,
                "scope":"host and observer cgroup; boundary snapshots, not per-thread attribution",
                "before":before, "after":after,
            }))?,
        )?;
        let record = observation?;
        // Persist before checking: failed invocations remain evidence.
        fs::write(
            self.batch
                .join(format!("command-{:08}.json", self.sequence)),
            serde_json::to_vec_pretty(&record)?,
        )?;
        require_success(&record)?;
        match condition {
            "native" => {
                ensure!(
                    record.stdout.is_empty() && record.stderr.is_empty(),
                    "native true produced output"
                );
            }
            "host-run" => {
                let current = bundle_paths(&self.batch.join("runs"))?;
                ensure!(
                    self.checked.is_subset(&current),
                    "previous Bundle disappeared"
                );
                let fresh: Vec<_> = current.difference(&self.checked).collect();
                ensure!(
                    fresh.len() == 1,
                    "expected exactly one new completed Bundle"
                );
                validate_bundle(fresh[0])?;
                self.checked = current;
            }
            "status" | "review" => {
                let value: Value = serde_json::from_slice(&record.stdout)
                    .context("status/review must emit valid JSON")?;
                ensure!(value.is_object(), "status/review output must be an object");
                validate_bundle(&self.session.join("run-bundle.json"))?;
                ensure!(
                    bundle_paths(&self.batch.join("runs"))? == self.checked,
                    "read-only command changed Bundle set"
                );
            }
            _ => unreachable!(),
        }
        ensure!(
            format!("{:x}", Sha256::digest(fs::read(&self.binary)?)) == self.binary_digest,
            "measured binary changed during operation"
        );
        ensure!(
            fs::read(self.batch.join("workspace/sentinel"))? == b"pvisor benchmark sentinel\n",
            "workspace sentinel changed during operation"
        );
        let ns = u64::try_from(record.elapsed_ns).context("command duration overflow")?;
        let mut ledger = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.batch.join("operations.jsonl"))?;
        writeln!(
            ledger,
            "{}",
            json!({
                "sequence":self.sequence, "condition":condition, "duration_ns":ns,
                "phase":self.phase,
                "validated":true, "scope":"direct-command-spawn-through-pipe-eof"
            })
        )?;
        Ok(Duration::from_nanos(ns))
    }
}

fn prepare(binary: &Path, batch: &Path) -> Result<Suite> {
    fs::create_dir(batch).context("output must be a fresh directory")?;
    let batch = batch.canonicalize()?;
    fs::create_dir(batch.join("workspace"))?;
    fs::write(
        batch.join("workspace/sentinel"),
        b"pvisor benchmark sentinel\n",
    )?;
    let binary = binary.canonicalize()?;
    let help = observe(Command::new(&binary).arg("--help"), Duration::from_secs(30))?;
    fs::write(batch.join("help.json"), serde_json::to_vec_pretty(&help)?)?;
    require_success(&help)?;
    let text = String::from_utf8(help.stdout)?;
    let entry = ["spawn", "run"]
        .into_iter()
        .find(|candidate| {
            text.lines().any(|line| {
                line.starts_with("  ") && line.split_whitespace().next() == Some(candidate)
            })
        })
        .context("binary does not list Session spawn or run")?
        .to_owned();
    let digest = format!("{:x}", Sha256::digest(fs::read(&binary)?));
    let mut suite = Suite {
        binary,
        binary_digest: digest.clone(),
        batch: batch.to_owned(),
        entry,
        session: PathBuf::new(),
        checked: BTreeSet::new(),
        sequence: 0,
        phase: "preflight",
    };
    let harness = batch.join("harness");
    fs::create_dir(&harness)?;
    for (name, bytes) in [
        ("process.rs", include_bytes!("process.rs").as_slice()),
        ("command.rs", include_bytes!("command.rs").as_slice()),
        ("main.rs", include_bytes!("main.rs").as_slice()),
        ("Cargo.toml", include_bytes!("../Cargo.toml").as_slice()),
        ("Cargo.lock", include_bytes!("../Cargo.lock").as_slice()),
    ] {
        fs::write(harness.join(name), bytes)?;
    }
    let kernel = observe(Command::new("uname").arg("-a"), Duration::from_secs(5))?;
    require_success(&kernel)?;
    let affinity = fs::read_to_string("/proc/self/status").ok().map(|status| {
        status
            .lines()
            .filter(|line| line.starts_with("Cpus_allowed_list:"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    fs::write(
        batch.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "benchmark_id":"B-PROCESS", "criterion":"0.7.0",
            "status":"prepared",
            "binary":suite.binary, "binary_sha256":digest,
            "timer":"Rust Instant, self-reported operation total",
            "scope":"direct-command-spawn-through-pipe-eof",
            "sampling":"Criterion Flat; operation records include preflight/calibration/warmup",
            "statistical_samples":PROCESS_SAMPLE_SIZE,
            "target_measurement_seconds":PROCESS_MEASUREMENT_TIME.as_secs(),
            "source_build_claim":false, "os":std::env::consts::OS,
            "arch":std::env::consts::ARCH, "kernel":String::from_utf8_lossy(&kernel.stdout),
            "affinity":affinity,
        }))?,
    )?;
    suite.operation("host-run")?;
    suite.session = suite
        .checked
        .iter()
        .next()
        .context("missing preflight Bundle")?
        .parent()
        .unwrap()
        .to_owned();
    let mut commands = serde_json::Map::new();
    for condition in ["native", "host-run", "status", "review"] {
        let command = suite.command(condition)?;
        commands.insert(
            condition.into(),
            json!({
                "argv": std::iter::once(command.get_program()).chain(command.get_args())
                    .map(|v| v.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "cwd": command.get_current_dir(),
                "env": {
                    "PVISOR_RUN_HOME": suite.batch.join("runs"),
                    "PVISOR_STARTUP_PROFILE":"0",
                    "PVISOR_STARTUP_TIMING":"0", "PVISOR_FS_PROFILE":"0"
                }
            }),
        );
    }
    fs::write(
        batch.join("commands.json"),
        serde_json::to_vec_pretty(&commands)?,
    )?;
    Ok(suite)
}

fn diagnostic_command(spec: &Value, output: &Path, condition: &str, mode: &str) -> Result<Command> {
    let argv: Vec<&str> = spec["argv"]
        .as_array()
        .context("argv")?
        .iter()
        .map(|v| v.as_str().context("argv string"))
        .collect::<Result<_>>()?;
    let program = argv.first().context("empty argv")?;
    let mut command = if mode == "strace" {
        let mut c = Command::new("strace");
        c.args(["-f", "-ttt", "-T", "-yy", "-o"])
            .arg(output.join(format!("{condition}.strace")))
            .arg("--")
            .arg(program);
        c
    } else {
        Command::new(program)
    };
    command
        .args(&argv[1..])
        .current_dir(spec["cwd"].as_str().context("cwd")?);
    for (key, value) in spec["env"].as_object().context("env")? {
        command.env(key, value.as_str().context("env string")?);
    }
    command
        .env("PVISOR_STARTUP_PROFILE", "1")
        .env("PVISOR_STARTUP_TIMING", "1");
    if condition == "host-run" {
        command.env("PVISOR_RUN_HOME", output.join("host-run-runs"));
    }
    Ok(command)
}

/// Re-execute retained argv solely for failure diagnosis, never as samples.
pub(crate) fn diagnose(batch: &Path) -> Result<()> {
    let commands: Value = serde_json::from_slice(&fs::read(batch.join("commands.json"))?)?;
    let output = batch.join("diagnostics");
    fs::create_dir(&output).context("diagnostics must not already exist")?;
    for condition in ["host-run", "status", "review"] {
        let spec = &commands[condition];
        for mode in ["profile", "strace"] {
            let mut command = diagnostic_command(spec, &output, condition, mode)?;
            let before = resource_snapshot();
            let observation = observe(&mut command, Duration::from_secs(30));
            let after = resource_snapshot();
            fs::write(
                output.join(format!("{condition}.{mode}.resources.json")),
                serde_json::to_vec_pretty(&json!({
                    "condition":condition, "mode":mode,
                    "scope":"diagnostic only; profile precedes strace in the same private store",
                    "before":before, "after":after,
                }))?,
            )?;
            match observation {
                Ok(record) => fs::write(
                    output.join(format!("{condition}.{mode}.json")),
                    serde_json::to_vec_pretty(&record)?,
                )?,
                Err(error) => fs::write(
                    output.join(format!("{condition}.{mode}.error")),
                    format!("{error:#}\n"),
                )?,
            }
        }
    }
    Ok(())
}

pub(crate) fn register(c: &mut Criterion, nightly: bool) -> Result<()> {
    let binary = std::env::var_os("PVISOR_BENCH_BINARY").context("--binary required")?;
    let batch = std::env::var_os("PVISOR_BENCH_OUTPUT").context("output directory required")?;
    let mut suite = prepare(Path::new(&binary), Path::new(&batch))?;
    let measurement = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let mut group = c.benchmark_group(if nightly {
            "process/nightly"
        } else {
            "process/smoke"
        });
        group
            .sampling_mode(SamplingMode::Flat)
            .sample_size(PROCESS_SAMPLE_SIZE)
            .warm_up_time(Duration::from_millis(100))
            .measurement_time(PROCESS_MEASUREMENT_TIME);
        for condition in ["native", "host-run", "status", "review"] {
            // Minimum explicit validated operations before adaptive Criterion warmup.
            suite.phase = "correctness-warmup";
            for _ in 0..if nightly { 10 } else { 2 } {
                suite.operation(condition)?;
            }
            // Criterion deliberately exposes no public calibration/formal phase
            // tag here. Keep its invocation observations distinct without guessing.
            suite.phase = "criterion";
            group.bench_function(condition, |b| {
                b.iter_custom(|iters| {
                    measured_total(iters, || suite.operation(condition)).unwrap_or_else(|error| {
                        panic!("B-PROCESS failed; evidence retained: {error:#}")
                    })
                })
            });
        }
        group.finish();
        Ok(())
    }));
    let result = match measurement {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "Criterion process operation failed; inspect raw command evidence"
        )),
    };
    let manifest_path = suite.batch.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    manifest["status"] = json!(if result.is_ok() { "complete" } else { "failed" });
    manifest["operation_records"] = json!(suite.sequence);
    if let Err(error) = &result {
        fs::write(
            suite.batch.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"error":format!("{error:#}")}))?,
        )?;
    }
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_snapshots_preserve_epoch_pressure_and_quota_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let proc = directory.path().join("proc");
        let cgroups = directory.path().join("cgroups");
        fs::create_dir_all(proc.join("sys/kernel/random")).unwrap();
        fs::create_dir_all(proc.join("self")).unwrap();
        fs::create_dir_all(proc.join("pressure")).unwrap();
        fs::create_dir_all(cgroups.join("owned")).unwrap();
        fs::write(proc.join("sys/kernel/random/boot_id"), "fixture-boot\n").unwrap();
        fs::write(proc.join("uptime"), "42.0 1.0\n").unwrap();
        fs::write(proc.join("meminfo"), "MemAvailable: 8192 kB\n").unwrap();
        fs::write(
            proc.join("vmstat"),
            "pswpin 3\npswpout 7\npgmajfault 11\nnr_dirty 2\nnr_writeback 1\nother 99\n",
        )
        .unwrap();
        fs::write(proc.join("self/cgroup"), "0::/owned\n").unwrap();
        fs::write(cgroups.join("owned/cpu.max"), "100000 100000\n").unwrap();
        fs::write(
            cgroups.join("owned/cpu.stat"),
            "nr_throttled 4\nthrottled_usec 1234\n",
        )
        .unwrap();
        fs::write(
            proc.join("pressure/io"),
            "full avg10=2.00 avg60=0.00 avg300=0.00 total=10\n",
        )
        .unwrap();
        let snapshot = resource_snapshot_at(&proc, &cgroups);
        assert_eq!(snapshot["boot_id"]["text"], "fixture-boot\n");
        assert_eq!(snapshot["uptime"]["text"], "42.0 1.0\n");
        assert_eq!(
            snapshot["vmstat"]["text"],
            "pswpin 3\npswpout 7\npgmajfault 11\nnr_dirty 2\nnr_writeback 1"
        );
        assert_eq!(snapshot["cgroup"]["path"], "/owned");
        assert_eq!(snapshot["cgroup"]["cpu.max"]["text"], "100000 100000\n");
        assert_eq!(
            snapshot["cgroup"]["cpu.stat"]["text"],
            "nr_throttled 4\nthrottled_usec 1234\n"
        );
        assert!(
            snapshot["pressure"]["io"]["text"]
                .as_str()
                .unwrap()
                .contains("avg10=2.00")
        );
        assert!(snapshot["cgroup"]["memory.max"]["error"].is_string());

        fs::write(proc.join("self/cgroup"), "0::/../outside\n").unwrap();
        let invalid = resource_snapshot_at(&proc, &cgroups);
        assert_eq!(
            invalid["cgroup"]["error"],
            "unified cgroup path unavailable"
        );
        let missing = resource_snapshot_at(&directory.path().join("absent"), &cgroups);
        assert!(missing["boot_id"]["error"].is_string());
        assert!(missing["vmstat"]["error"].is_string());
    }

    #[test]
    fn diagnostics_override_retained_disabled_timing_without_mutating_spec() {
        let spec = json!({
            "argv":["/prepared/pvisor", "spawn", "--", "/usr/bin/true"],
            "cwd":"/workspace",
            "env":{
                "PVISOR_STARTUP_PROFILE":"0", "PVISOR_STARTUP_TIMING":"0",
                "PVISOR_FS_PROFILE":"0", "PVISOR_RUN_HOME":"/original/runs"
            }
        });
        for mode in ["profile", "strace"] {
            let command =
                diagnostic_command(&spec, Path::new("/diagnostics"), "host-run", mode).unwrap();
            let environment: std::collections::BTreeMap<_, _> = command
                .get_envs()
                .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
                .collect();
            assert_eq!(environment["PVISOR_STARTUP_PROFILE"], "1");
            assert_eq!(environment["PVISOR_STARTUP_TIMING"], "1");
            assert_eq!(environment["PVISOR_FS_PROFILE"], "0");
            assert_eq!(environment["PVISOR_RUN_HOME"], "/diagnostics/host-run-runs");
            assert_eq!(command.get_current_dir(), Some(Path::new("/workspace")));
            let arguments: Vec<_> = command.get_args().collect();
            assert!(arguments.ends_with(&[
                std::ffi::OsStr::new("spawn"),
                std::ffi::OsStr::new("--"),
                std::ffi::OsStr::new("/usr/bin/true")
            ]));
        }
        assert_eq!(spec["env"]["PVISOR_STARTUP_TIMING"], "0");
        assert_eq!(spec["env"]["PVISOR_RUN_HOME"], "/original/runs");
        let mut empty = spec;
        empty["argv"] = json!([]);
        assert!(diagnostic_command(&empty, Path::new("/out"), "host-run", "profile").is_err());
    }

    #[test]
    fn bundle_success_is_not_inferred_from_process_exit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("run-bundle.json");
        fs::write(&path, r#"{"run":{"state":"failed","exit_code":0}}"#).unwrap();
        assert!(validate_bundle(&path).is_err());
        fs::write(&path, r#"{"run":{"state":"completed","exit_code":0}}"#).unwrap();
        validate_bundle(&path).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn relative_output_and_noninteractive_spawn_keep_owned_run_store() {
        use std::os::unix::fs::PermissionsExt;
        let cwd = std::env::current_dir().unwrap();
        let directory = tempfile::tempdir_in(&cwd).unwrap();
        let binary = directory.path().join("fake-pvisor");
        fs::write(
            &binary,
            r#"#!/bin/sh
set -eu
if [ "$1" = --help ]; then printf '  spawn  Start a Session\n'; exit 0; fi
test ! -t 0
if [ "$1" = spawn ]; then
  mkdir -p "$PVISOR_RUN_HOME/run-$$"
  printf '{"run":{"state":"completed","exit_code":0}}' > "$PVISOR_RUN_HOME/run-$$/run-bundle.json"
else
  printf '{}\n'
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let relative = directory.path().strip_prefix(&cwd).unwrap().join("batch");
        let mut suite = prepare(&binary, &relative).unwrap();
        assert!(suite.batch.is_absolute());
        let command = suite.command("host-run").unwrap();
        for key in ["PVISOR_STARTUP_PROFILE", "PVISOR_STARTUP_TIMING"] {
            assert!(
                command.get_envs().any(|(name, value)| {
                    name == key && value == Some(std::ffi::OsStr::new("0"))
                })
            );
        }
        suite.operation("host-run").unwrap();
        suite.operation("status").unwrap();
        suite.operation("review").unwrap();
        assert_eq!(suite.checked.len(), 2);
        assert!(!suite.batch.join("workspace").join(&relative).exists());
        let resources: Value =
            serde_json::from_slice(&fs::read(suite.batch.join("resources-00000002.json")).unwrap())
                .unwrap();
        assert_eq!(resources["condition"], "host-run");
        assert!(resources["before"].is_object());
        assert!(resources["after"].is_object());
    }
}
