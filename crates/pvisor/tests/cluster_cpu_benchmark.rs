//! Explicit hardware experiment. Correct results and real placement are gates;
//! performance ratios are evidence, never hard-coded passing thresholds.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "common/native_cache.rs"]
mod native_cache;
use native_cache::{copy_program, publish_layer, publish_layer_prepared};
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{CpuQosClass, ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const ADMIN: &str = "cpu-bench-admin-0123456789012345";
const WORKER: &str = "cpu-bench-worker-0123456789012345";
const PROBE: &str = include_str!("fixtures/cluster_cpu_probe.c");

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Solo,
    Unprotected,
    IdleOnly,
    Protected,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Unprotected => "unprotected",
            Self::IdleOnly => "idle_only",
            Self::Protected => "protected",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    sequence: u32,
    intended_ns: u64,
    begin_ns: u64,
    end_ns: u64,
    cpu_ns: u64,
    solutions: u64,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn cpu_list(text: &str) -> BTreeSet<u32> {
    let mut result = BTreeSet::new();
    for part in text.trim().split(',') {
        let (low, high) = part.split_once('-').unwrap_or((part, part));
        let (low, high) = (low.parse::<u32>().unwrap(), high.parse::<u32>().unwrap());
        assert!(low <= high && high < 1_000_000);
        result.extend(low..=high);
    }
    result
}
fn affinity(pid: u32) -> BTreeSet<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    cpu_list(
        status
            .lines()
            .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
            .unwrap(),
    )
}
fn siblings() -> [u32; 2] {
    let allowed = affinity(std::process::id());
    for cpu in allowed.iter().rev() {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
        let pair: Vec<_> = cpu_list(&fs::read_to_string(path).unwrap())
            .intersection(&allowed)
            .copied()
            .collect();
        if pair.len() == 2 {
            for key in ["core_id", "physical_package_id"] {
                let a = fs::read_to_string(format!(
                    "/sys/devices/system/cpu/cpu{}/topology/{key}",
                    pair[0]
                ))
                .unwrap();
                let b = fs::read_to_string(format!(
                    "/sys/devices/system/cpu/cpu{}/topology/{key}",
                    pair[1]
                ))
                .unwrap();
                assert_eq!(a, b);
            }
            return pair.try_into().unwrap();
        }
    }
    panic!("benchmark requires an allowed online SMT sibling pair");
}
fn command_text(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
struct Scope(String);
impl Scope {
    fn property(&self, property: &str) -> String {
        command_text(
            "systemctl",
            &[
                "--user",
                "show",
                &self.0,
                "--value",
                &format!("--property={property}"),
            ],
        )
    }
    fn directory(&self) -> PathBuf {
        let path = self.property("ControlGroup");
        assert!(path.starts_with('/') && path.ends_with(&self.0));
        Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'))
    }
    fn snapshot(&self, cpus: &[u32; 2]) -> Value {
        let root = self.directory();
        let mut cpu = BTreeMap::<String, u64>::new();
        for line in fs::read_to_string(root.join("cpu.stat")).unwrap().lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 2);
            cpu.insert(fields[0].into(), fields[1].parse().unwrap());
        }
        let host_stat = fs::read_to_string("/proc/stat").unwrap();
        let mut host_cpu = BTreeMap::new();
        for id in cpus {
            let prefix = format!("cpu{id} ");
            let ticks: Vec<u64> = host_stat
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap()
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            assert!(ticks.len() >= 10);
            host_cpu.insert(id.to_string(), ticks);
        }
        let frequency: Vec<_> = cpus.iter().map(|id| {
            let base = format!("/sys/devices/system/cpu/cpu{id}/cpufreq");
            json!({"cpu":id,"governor":fs::read_to_string(format!("{base}/scaling_governor")).ok().map(|s|s.trim().to_owned()),
                "instantaneous_frequency_khz":fs::read_to_string(format!("{base}/scaling_cur_freq")).ok().and_then(|s|s.trim().parse::<u64>().ok())})
        }).collect();
        json!({"cpu_stat":cpu,"cpu_pressure":fs::read_to_string(root.join("cpu.pressure")).unwrap(),
            "host_selected_cpu_ticks":host_cpu,"host_cpu_pressure":fs::read_to_string("/proc/pressure/cpu").unwrap(),
            "host_io_pressure":fs::read_to_string("/proc/pressure/io").unwrap(),"host_memory_pressure":fs::read_to_string("/proc/pressure/memory").unwrap(),
            "host_frequency":frequency,
            "memory_current_bytes":fs::read_to_string(root.join("memory.current")).unwrap().trim().parse::<u64>().unwrap(),
            "memory_peak_bytes":fs::read_to_string(root.join("memory.peak")).unwrap().trim().parse::<u64>().unwrap()})
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn cookie(pid: u32) -> u64 {
    let mut result = 0u64;
    assert_eq!(
        unsafe {
            libc::prctl(
                62,
                0usize,
                pid as usize,
                0usize,
                &mut result as *mut u64 as usize,
            )
        },
        0
    );
    result
}
fn native_identity(pid: u32, run_id: &pvisor_core::RunId) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let env = fs::read(format!("/proc/{pid}/environ")).unwrap();
    let launch = env
        .split(|b| *b == 0)
        .find_map(|s| s.strip_prefix(b"PVISOR_KRUN_RUNNER_SPEC="))
        .unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(Path::new(std::ffi::OsStr::from_bytes(launch))).unwrap())
            .unwrap();
    assert_eq!(launch["run_id"].as_str(), Some(run_id.as_str()));
    let fd = env
        .split(|b| *b == 0)
        .find_map(|s| s.strip_prefix(b"PVISOR_KRUN_RAM_FD="))
        .unwrap();
    let fd = std::str::from_utf8(fd).unwrap().parse::<u32>().unwrap();
    let ram = fs::metadata(format!("/proc/{pid}/fd/{fd}")).unwrap();
    assert!(ram.len() >= 256 * 1024 * 1024);
    let mapped = fs::read_to_string(format!("/proc/{pid}/maps"))
        .unwrap()
        .lines()
        .any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let (major, minor) = fields[3].split_once(':').unwrap();
            fields[4].parse::<u64>().unwrap() == ram.ino()
                && u64::from_str_radix(major, 16).unwrap() == u64::from(libc::major(ram.dev()))
                && u64::from_str_radix(minor, 16).unwrap() == u64::from(libc::minor(ram.dev()))
        });
    assert!(
        mapped,
        "native VMM must map its actual launch RAM descriptor"
    );
}
fn threads(
    pid: u32,
    policy: i32,
    expected_cookie: u64,
    cpus: &[u32; 2],
    install_idle: bool,
) -> Value {
    let mut evidence = Vec::new();
    for entry in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let tid = entry
            .unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        if install_idle {
            let parameters: libc::sched_param = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sched_setscheduler(tid as i32, libc::SCHED_IDLE, &parameters) },
                0
            );
        }
        let actual = unsafe { libc::sched_getscheduler(tid as i32) };
        assert_eq!(actual, policy);
        assert_eq!(cookie(tid), expected_cookie);
        assert_eq!(affinity(tid), cpus.iter().copied().collect());
        evidence.push(json!({"tid":tid,"scheduler_policy":actual,"core_cookie":expected_cookie}));
    }
    assert!(
        evidence.len() >= 2,
        "native VM must have vCPU/device threads"
    );
    assert!(
        fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .any(
                |entry| fs::read_to_string(entry.unwrap().path().join("comm"))
                    .unwrap()
                    .contains("vcpu")
            ),
        "must inspect actual native vCPU owners"
    );
    json!(evidence)
}
fn task(id: &str, environment: &str, ls: bool, mode: Mode, steps: u32) -> TaskSpec {
    let mut run = RunSpec::process(id, "cpu-contention-nqueens", "/usr/bin/cpu-probe");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    process.args = vec![if ls { "ls" } else { "be" }.into(), steps.to_string()];
    run.runtime.timeout_ms = Some(90_000);
    run.runtime.max_output_bytes = 4096;
    TaskSpec {
        retain_artifacts: None,
        gateway: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "cpu-bench".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        },
        resources: Resources {
            slots: 1,
            cpu_millis: 1000,
            memory_bytes: 256 * 1024 * 1024,
        },
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: false,
        environment: Some(environment.into()),
        restore: None,
        cpu_qos: (mode == Mode::Protected).then_some(if ls {
            CpuQosClass::LatencySensitive
        } else {
            CpuQosClass::BestEffort
        }),
    }
}
async fn marker(client: &Client, id: &str, root: &Path, name: &str, log: &Path) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if root.join(name).exists() {
                return;
            }
            let task = client.task(id).await.unwrap();
            assert!(
                !task.phase.terminal(),
                "{task:?}\n{}",
                fs::read_to_string(log).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "guest marker {id}/{name}: {e}\n{}",
            fs::read_to_string(log).unwrap()
        )
    });
}
async fn sampled(client: &Client, id: &str) -> RunCpuSample {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = client.task(id).await.unwrap();
            assert!(!task.phase.terminal(), "{task:?}");
            if let Some(sample) = task.cpu_sample
                && sample.report.sample.usage.is_some()
            {
                return sample.report.sample;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
use pvisor_core::cpu::RunCpuSample;
async fn finished(client: &Client, id: &str, log: &Path) -> TaskRecord {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let task = client.task(id).await.unwrap();
            if task.phase.terminal() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|e| panic!("{id}: {e}\n{}", fs::read_to_string(log).unwrap()))
}
fn records(root: &Path) -> Vec<Step> {
    fs::read_to_string(root.join("steps.jsonl"))
        .unwrap()
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let row: Step = serde_json::from_str(line).unwrap();
            assert_eq!(row.sequence as usize, i);
            assert_eq!(row.solutions, 73712);
            assert!(
                row.begin_ns >= row.intended_ns && row.end_ns >= row.begin_ns && row.cpu_ns > 0
            );
            row
        })
        .collect()
}
fn percentile(values: &[u64], percent: usize) -> u64 {
    assert!(!values.is_empty() && (1..=100).contains(&percent));
    let mut values = values.to_vec();
    values.sort_unstable();
    values[(values.len() * percent).div_ceil(100) - 1]
}
fn summary(rows: &[Step]) -> Value {
    let response: Vec<_> = rows.iter().map(|r| r.end_ns - r.intended_ns).collect();
    let service: Vec<_> = rows.iter().map(|r| r.end_ns - r.begin_ns).collect();
    let elapsed = rows.last().unwrap().end_ns - rows[0].intended_ns;
    json!({"completed_solutions":rows.len(),"elapsed_ns":elapsed,
        "solutions_per_second":rows.len() as f64 * 1e9 / elapsed as f64,
        "response_ns":{"p50":percentile(&response,50),"p95":percentile(&response,95),"p99":percentile(&response,99)},
        "service_ns":{"p50":percentile(&service,50),"p95":percentile(&service,95),"p99":percentile(&service,99)}})
}
fn save(path: &Path, report: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(report).unwrap()).unwrap();
    fs::rename(temporary, path).unwrap();
}

fn aggregate(trials: &[Value]) -> Value {
    let mut output = serde_json::Map::new();
    for mode in [
        Mode::Solo,
        Mode::Unprotected,
        Mode::IdleOnly,
        Mode::Protected,
    ] {
        let selected: Vec<_> = trials
            .iter()
            .filter(|t| t["mode"].as_str() == Some(mode.name()))
            .collect();
        let mut rows = Vec::new();
        let mut host_ns = 0u64;
        let mut guest_ns = 0u64;
        let mut be = 0u64;
        let mut cpu_us = 0u64;
        let mut throttled = 0u64;
        for trial in &selected {
            rows.extend(serde_json::from_value::<Vec<Step>>(trial["ls_steps"].clone()).unwrap());
            host_ns += trial["host_measurement_elapsed_ns"].as_u64().unwrap();
            guest_ns += trial["ls_summary"]["elapsed_ns"].as_u64().unwrap();
            be += trial["be_completed_steps"].as_u64().unwrap();
            cpu_us += trial["worker_cpu_usage_usec"].as_u64().unwrap();
            throttled += trial["scope_after"]["cpu_stat"]["nr_throttled"]
                .as_u64()
                .unwrap()
                .checked_sub(
                    trial["scope_before"]["cpu_stat"]["nr_throttled"]
                        .as_u64()
                        .unwrap(),
                )
                .unwrap();
        }
        let response: Vec<_> = rows.iter().map(|r| r.end_ns - r.intended_ns).collect();
        let service: Vec<_> = rows.iter().map(|r| r.end_ns - r.begin_ns).collect();
        output.insert(mode.name().into(),json!({"trials":selected.len(),"ls_completed_steps":rows.len(),
            "response_ns":{"p50":percentile(&response,50),"p95":percentile(&response,95),"p99":percentile(&response,99)},
            "service_ns":{"p50":percentile(&service,50),"p95":percentile(&service,95),"p99":percentile(&service,99)},
            "ls_steps_per_second":rows.len() as f64*1e9/guest_ns as f64,
            "be_steps_per_second":be as f64*1e9/host_ns as f64,"worker_cpu_millis":cpu_us as f64*1e6/host_ns as f64,
            "worker_cpu_time_usec":cpu_us,"quota_throttled_periods":throttled}));
    }
    output.into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit CPU experiment; run just bench-cluster-cpu; requires SMT/KVM/FUSE/user-systemd/cc"]
async fn matched_native_cpu_contention_experiment() {
    let output = std::env::var_os("PVISOR_CPU_BENCH_OUT")
        .map(PathBuf::from)
        .expect("set PVISOR_CPU_BENCH_OUT");
    assert!(output.is_absolute(), "benchmark output must be absolute");
    let rounds: usize = std::env::var("PVISOR_CPU_BENCH_ROUNDS")
        .unwrap_or("3".into())
        .parse()
        .unwrap();
    assert!((1..=5).contains(&rounds));
    let steps = 60;
    let firmware_directory = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR")
        .map(PathBuf::from)
        .expect("set PVISOR_TEST_LIBKRUNFW_DIR to a directory containing libkrunfw.so.5");
    let firmware_sha256 = digest(&fs::read(firmware_directory.join("libkrunfw.so.5")).unwrap());
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let cpus = siblings();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let compiler = command_text("cc", &["--version"])
        .lines()
        .next()
        .unwrap()
        .to_owned();
    let program = root.join("cpu-probe");
    let source_file = root.join("probe.c");
    fs::write(&source_file, PROBE).unwrap();
    let compilation = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&source_file)
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let binary = root.join("pvisor-worker");
    fs::copy(env!("CARGO_BIN_EXE_pvisor-worker"), &binary).unwrap();
    let source = root.join("source");
    let cache = root.join("cache");
    let base = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        let program = program.clone();
        move || {
            publish_layer_prepared(&source, &cache, "cpu-probe-base", &[], true, |root| {
                copy_program(root, program.to_str().unwrap());
                fs::create_dir_all(root.join("usr/bin")).unwrap();
                fs::rename(
                    root.join(program.strip_prefix("/").unwrap()),
                    root.join("usr/bin/cpu-probe"),
                )
                .unwrap();
            })
        }
    })
    .await
    .unwrap();
    let toolkit = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "cpu-probe-answer",
                &[("env/expected", "73712\n")],
                false,
            )
        }
    })
    .await
    .unwrap();
    let scheduler = Scheduler::open(
        &root.join("journal"),
        SchedulerConfig {
            lease_duration_ms: 3000,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let environment = admin
        .publish_environment(&EnvironmentTemplate {
            version: CLUSTER_VERSION,
            architecture: "amd64".into(),
            base,
            workspace: None,
            toolkits: vec![toolkit],
        })
        .await
        .unwrap();
    let mut report = json!({"schema_version":1,"complete":false,"started_at_unix_ms":pvisor_core::unix_now_ms(),
        "kernel":command_text("uname",&["-r"]),"cpu_model":fs::read_to_string("/proc/cpuinfo").unwrap().lines().find_map(|l|l.strip_prefix("model name\t: ")).unwrap(),
        "smt_cpus":cpus,"cpu_quota_millis":2000,"reservation_overcommit_bps":20000,"vm_memory_bytes":256*1024*1024u64,
        "source_sha256":digest(PROBE.as_bytes()),"guest_binary_sha256":digest(&fs::read(&program).unwrap()),
        "worker_binary_sha256":digest(&fs::read(&binary).unwrap()),"compiler":compiler,"compiler_flags":["-std=c11","-O2","-Wall","-Wextra","-Werror"],
        "firmware_sha256":firmware_sha256,"backend":"native libkrun VM",
        "environment":environment,"steps_per_trial":steps,"arrival_period_ns":100_000_000,"warmup_solves":5,"rounds":rounds,
        "clock":"guest CLOCK_MONOTONIC; response includes intended-arrival backlog; nearest-rank percentiles",
        "scope":"single-host synthetic result-checked N=13 searches; not agent application or production density evidence",
        "host_isolation":"Worker affinity is pinned; selected CPUs are not reserved against unrelated host tasks",
        "host_clock_ticks_per_second":unsafe { libc::sysconf(libc::_SC_CLK_TCK) },
        "trials":[]});
    save(&output, &report);
    let orders = [
        [
            Mode::Solo,
            Mode::Unprotected,
            Mode::IdleOnly,
            Mode::Protected,
        ],
        [
            Mode::Protected,
            Mode::IdleOnly,
            Mode::Unprotected,
            Mode::Solo,
        ],
        [
            Mode::Unprotected,
            Mode::Solo,
            Mode::Protected,
            Mode::IdleOnly,
        ],
    ];
    for round in 0..rounds {
        for mode in orders[round % orders.len()] {
            let trial = trial(
                &admin,
                &url,
                &binary,
                root,
                &cache,
                report["environment"]["digest"].as_str().unwrap(),
                cpus,
                round,
                mode,
                steps,
            )
            .await;
            eprintln!(
                "CPU trial round={round} mode={} response={} BE_steps={} quota_usage_us={}",
                mode.name(),
                trial["ls_summary"]["response_ns"],
                trial["be_completed_steps"],
                trial["worker_cpu_usage_usec"]
            );
            report["trials"].as_array_mut().unwrap().push(trial);
            save(&output, &report);
        }
    }
    report["complete"] = true.into();
    report["summary"] = aggregate(report["trials"].as_array().unwrap());
    report["finished_at_unix_ms"] = pvisor_core::unix_now_ms().into();
    save(&output, &report);
    server.abort();
}

#[allow(clippy::too_many_arguments)]
async fn trial(
    admin: &Client,
    url: &str,
    binary: &Path,
    root: &Path,
    cache: &Path,
    environment: &str,
    cpus: [u32; 2],
    round: usize,
    mode: Mode,
    steps: u32,
) -> Value {
    let directory = root.join(format!("round-{round}-{}", mode.name()));
    fs::create_dir(&directory).unwrap();
    let mut profile="[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n[cpu_sampling]\nenabled = true\ninterval_ms = 1000\n[admission]\nmode = 'linux_pressure'\nmemory_reserve_bytes = 67108864\ncpu_overcommit_bps = 20000\ncpu_some_avg10_limit_bps = 10000\n".to_owned();
    if mode == Mode::Protected {
        profile += "[cpu_qos]\nenabled = true\n";
    }
    if let Some(fw) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        profile += &format!(
            "[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&fw.to_string_lossy()).unwrap()
        );
    }
    let config = directory.join("worker.toml");
    fs::write(&config, profile).unwrap();
    let log = directory.join("worker.log");
    let scope = Scope(format!(
        "pvisor-test-worker-cpu-{}.service",
        uuid::Uuid::new_v4()
    ));
    let mut process = Guard(
        Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--service-type=exec",
                "--property=CPUQuota=200%",
                "--property=MemoryMax=2147483648",
                "--property=MemoryHigh=1610612736",
                "--property=MemorySwapMax=0",
                "--property=OOMPolicy=kill",
                "--property=KillMode=mixed",
                "--property=TimeoutStopSec=30s",
                "--property=Delegate=no",
                "--setenv=PVISOR_CLUSTER_WORKER_TOKEN",
                "--setenv=PVISOR_CACHE_BACKEND",
                "--setenv=PVISOR_CACHE_LOCATION",
                "--setenv=XDG_CACHE_HOME",
                "--setenv=TOKIO_WORKER_THREADS",
                "--setenv=PATH",
            ])
            .arg(format!("--unit={}", scope.0))
            .arg(format!("--property=CPUAffinity={} {}", cpus[0], cpus[1]))
            .arg(format!(
                "--working-directory={}",
                std::env::current_dir().unwrap().display()
            ))
            .arg(binary)
            .args([
                "--url",
                url,
                "--id",
                "cpu-bench-worker",
                "--backend",
                "vm",
                "--poll-ms",
                "20",
                "--slots",
                "4",
                "--cpu-millis",
                "4000",
                "--memory-bytes",
                "1073741824",
            ])
            .arg("--state")
            .arg(directory.join("worker"))
            .arg("--config")
            .arg(config)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", cache)
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            .env("TOKIO_WORKER_THREADS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&log).unwrap()))
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if scope
                .property("MainPID")
                .parse::<u32>()
                .is_ok_and(|p| p > 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let worker_pid = scope.property("MainPID").parse::<u32>().unwrap();
    let cgroup = scope.directory();
    let quota = fs::read_to_string(cgroup.join("cpu.max")).unwrap();
    let quota_fields: Vec<u64> = quota
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(quota_fields[0], quota_fields[1] * 2);
    for (file, expected) in [
        ("memory.max", 2_147_483_648u64),
        ("memory.high", 1_610_612_736),
        ("memory.swap.max", 0),
        ("memory.oom.group", 1),
    ] {
        assert_eq!(
            fs::read_to_string(cgroup.join(file))
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap(),
            expected
        );
    }
    assert_eq!(affinity(worker_pid), cpus.iter().copied().collect());
    assert_ne!(
        fs::read_to_string("/proc/self/cgroup").unwrap(),
        fs::read_to_string(format!("/proc/{worker_pid}/cgroup")).unwrap()
    );
    let count = if mode == Mode::Solo { 1 } else { 4 };
    let mut jobs = Vec::new();
    let mut samples = Vec::new();
    let mut policy_evidence = Vec::new();
    for role in 0..count {
        let id = format!("cpu-r{round}-{}-{role}", mode.name());
        admin
            .submit(&task(&id, environment, role == 0, mode, steps))
            .await
            .unwrap();
        let upper = directory.join(format!("worker/tasks/{id}-1/upper/env"));
        jobs.push((id, upper));
    }
    for (role, (id, upper)) in jobs.iter().enumerate() {
        marker(admin, id, upper, "ready", &log).await;
        let sample = sampled(admin, id).await;
        let pid = sample.usage.as_ref().unwrap().pid;
        native_identity(pid, &sample.run_id);
        assert_eq!(
            fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap(),
            fs::read_to_string(format!("/proc/{worker_pid}/cgroup")).unwrap()
        );
        let expected_cookie = if mode == Mode::Protected && role == 0 {
            let c = cookie(pid);
            assert_ne!(c, 0);
            assert_ne!(c, cookie(worker_pid));
            c
        } else {
            cookie(worker_pid)
        };
        let idle = role > 0 && matches!(mode, Mode::IdleOnly | Mode::Protected);
        policy_evidence.push(json!({"run_id":id,"pid":pid,"threads":threads(pid,if idle {libc::SCHED_IDLE}else{libc::SCHED_OTHER},expected_cookie,&cpus,mode==Mode::IdleOnly&&role>0)}));
        samples.push(sample);
    }
    assert_eq!(
        admin
            .workers()
            .await
            .unwrap()
            .into_iter()
            .find(|w| w.registration.id == "cpu-bench-worker")
            .unwrap()
            .reserved
            .slots,
        count as u32
    );
    for (id, upper) in &jobs {
        fs::write(upper.join("go"), b"go\n").unwrap();
        marker(admin, id, upper, "warm", &log).await;
    }
    let be_before: Vec<_> = jobs
        .iter()
        .skip(1)
        .map(|(_, upper)| completed_lines(upper))
        .collect();
    let before = scope.snapshot(&cpus);
    let began = Instant::now();
    fs::write(jobs[0].1.join("measure"), b"measure\n").unwrap();
    marker(admin, &jobs[0].0, &jobs[0].1, "done", &log).await;
    let elapsed_ns = began.elapsed().as_nanos() as u64;
    let after = scope.snapshot(&cpus);
    let be_after: Vec<_> = jobs
        .iter()
        .skip(1)
        .map(|(_, upper)| completed_lines(upper))
        .collect();
    let be_window_steps: usize = be_after
        .iter()
        .zip(&be_before)
        .map(|(after, before)| after.checked_sub(*before).unwrap())
        .sum();
    for (_, upper) in jobs.iter().skip(1) {
        fs::write(upper.join("stop"), b"stop\n").unwrap();
    }
    for (id, upper) in &jobs {
        marker(admin, id, upper, "done", &log).await;
        fs::write(upper.join("release"), b"release\n").unwrap();
    }
    let ls = records(&jobs[0].1);
    assert_eq!(ls.len(), steps as usize);
    for (i, row) in ls.iter().enumerate() {
        assert_eq!(row.intended_ns - ls[0].intended_ns, i as u64 * 100_000_000);
    }
    let mut results = Vec::new();
    let mut be_steps = 0;
    let mut be_records = Vec::new();
    for ((id, upper), previous) in jobs.iter().zip(&samples) {
        let terminal = finished(admin, id, &log).await;
        assert_eq!(terminal.phase, TaskPhase::Succeeded, "{terminal:?}");
        let result = terminal.result.unwrap();
        assert_eq!(result.attempt_id, previous.attempt_id);
        let Some(pvisor_core::cpu::TerminalCpuUsage::Measured { usage }) =
            &result.executor_observations.cpu_usage
        else {
            panic!("missing final CPU: {result:?}")
        };
        usage
            .interval_since(previous.usage.as_ref().unwrap())
            .unwrap();
        if id != &jobs[0].0 {
            let rows = records(upper);
            assert!(!rows.is_empty(), "BE must continue doing correct work");
            be_steps += rows.len();
            be_records.push(json!({"run_id":id,"steps":rows}));
        }
        results.push(result);
    }
    let cpu_usage = after["cpu_stat"]["usage_usec"]
        .as_u64()
        .unwrap()
        .checked_sub(before["cpu_stat"]["usage_usec"].as_u64().unwrap())
        .unwrap();
    let reserved = admin
        .workers()
        .await
        .unwrap()
        .into_iter()
        .find(|w| w.registration.id == "cpu-bench-worker")
        .unwrap()
        .reserved;
    assert_eq!(reserved, Resources::default());
    drop(scope);
    let status = tokio::task::spawn_blocking(move || {
        let status = process.0.wait().unwrap();
        drop(process);
        status
    })
    .await
    .unwrap();
    assert!(status.success());
    assert!(!Path::new(&format!("/proc/{worker_pid}")).exists());
    json!({"round":round,"mode":mode,"worker_pid":worker_pid,"scope_cpu_max":quota.trim(),"host_measurement_elapsed_ns":elapsed_ns,
        "worker_cpu_usage_usec":cpu_usage,"scope_before":before,"scope_after":after,"ls_summary":summary(&ls),"ls_steps":ls,
        "be_completed_steps":be_window_steps,"be_total_completed_steps":be_steps,"be_counts_before":be_before,"be_counts_after":be_after,"be_steps":be_records,"kernel_policy_evidence":policy_evidence,"terminal_results":results})
}

fn completed_lines(root: &Path) -> usize {
    fs::read(root.join("steps.jsonl"))
        .unwrap()
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

#[test]
fn percentile_and_arrival_backlog_accounting_do_not_hide_late_requests() {
    assert_eq!(
        cpu_list("0-2,8,10-11"),
        [0, 1, 2, 8, 10, 11].into_iter().collect()
    );
    assert_eq!(percentile(&[90, 10, 20, 30], 50), 20);
    assert_eq!(percentile(&[90, 10, 20, 30], 99), 90);
    let rows = vec![
        Step {
            sequence: 0,
            intended_ns: 100,
            begin_ns: 200,
            end_ns: 250,
            cpu_ns: 40,
            solutions: 73712,
        },
        Step {
            sequence: 1,
            intended_ns: 200,
            begin_ns: 250,
            end_ns: 300,
            cpu_ns: 40,
            solutions: 73712,
        },
    ];
    let value = summary(&rows);
    assert_eq!(value["response_ns"]["p50"], 100);
    assert_eq!(value["service_ns"]["p50"], 50);
    // Independent VMs have independent epochs. Aggregate durations, never
    // subtract timestamps across VMs; one CPU second per wall second is 1000m.
    let trials: Vec<_> = [Mode::Solo, Mode::Unprotected, Mode::IdleOnly, Mode::Protected]
        .into_iter().flat_map(|mode| [1_000_000_000u64, 0].map(move |epoch| json!({
            "mode":mode,"ls_steps":[Step { sequence:0,intended_ns:epoch,begin_ns:epoch,end_ns:epoch+1_000_000_000,cpu_ns:500_000_000,solutions:73712 }],
            "host_measurement_elapsed_ns":1_000_000_000,"ls_summary":{"elapsed_ns":1_000_000_000},
            "be_completed_steps":5,"worker_cpu_usage_usec":1_000_000,
            "scope_before":{"cpu_stat":{"nr_throttled":10}},"scope_after":{"cpu_stat":{"nr_throttled":12}}
        }))).collect();
    let aggregate = aggregate(&trials);
    assert_eq!(
        aggregate["protected"]["ls_steps_per_second"].as_f64(),
        Some(1.0)
    );
    assert_eq!(
        aggregate["protected"]["be_steps_per_second"].as_f64(),
        Some(5.0)
    );
    assert_eq!(
        aggregate["protected"]["worker_cpu_millis"].as_f64(),
        Some(1000.0)
    );
    assert_eq!(aggregate["protected"]["quota_throttled_periods"], 4);
}
