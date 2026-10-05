//! Result-checked native Agent experiment under identical finite CPU/RAM limits.
//! Ratios are measurements, never passing thresholds or production-scale claims.
#![cfg(all(feature = "gateway", target_os = "linux", target_arch = "x86_64"))]
#[path = "common/agent_fixture.rs"]
mod agent_fixture;
#[path = "common/model_service.rs"]
mod model_service;
#[path = "common/native_cache.rs"]
mod native_cache;
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::memory::NativeVmMemory;
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const ADMIN: &str = "inference-bench-admin-0123456789012345";
const WORKER: &str = "inference-bench-worker-0123456789012345";
const TASKS: usize = 8;
const DELAY_MS: u64 = 2000;
const MEMORY_MAX: u64 = 4 * 1024 * 1024 * 1024;

fn text(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn save(path: &Path, report: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let temporary = path.with_extension("partial.json");
    let bytes = serde_json::to_vec_pretty(report).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(model_service::KEY));
    fs::write(&temporary, bytes).unwrap();
    fs::rename(temporary, path).unwrap();
}
fn counters(root: &Path, file: &str) -> BTreeMap<String, u64> {
    fs::read_to_string(root.join(file))
        .unwrap()
        .lines()
        .map(|line| {
            let (key, value) = line.split_once(' ').unwrap();
            (key.into(), value.parse().unwrap())
        })
        .collect()
}
fn scalar(root: &Path, file: &str) -> u64 {
    fs::read_to_string(root.join(file))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn resident(usage: &NativeVmMemory, worker_pid: u32, worker_group: &str) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", usage.pid)).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    if fields[19].parse::<u64>().unwrap() != usage.start_time_ticks {
        return None;
    }
    let cgroup = fs::read_to_string(format!("/proc/{}/cgroup", usage.pid)).ok()?;
    let status = fs::read_to_string(format!("/proc/{}/status", usage.pid)).ok()?;
    let kb = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?;
    assert_eq!(fields[1].parse::<u32>().unwrap(), worker_pid);
    assert_eq!(cgroup, worker_group);
    Some(
        kb.split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024,
    )
}

struct Service {
    name: String,
    launcher: Child,
}
impl Service {
    fn property(&self, name: &str) -> String {
        text(
            "systemctl",
            &[
                "--user",
                "show",
                &self.name,
                "--value",
                &format!("--property={name}"),
            ],
        )
    }
    fn group(&self) -> PathBuf {
        let group = self.property("ControlGroup");
        assert!(group.starts_with('/') && group.ends_with(&self.name));
        Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'))
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.launcher.kill();
        let _ = self.launcher.wait();
    }
}

#[derive(Clone)]
struct Delay {
    upstream: String,
    http: reqwest::Client,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    received: Arc<AtomicUsize>,
}
async fn delayed(
    State(state): State<Delay>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.received.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        headers.get("authorization").unwrap(),
        format!("Bearer {}", model_service::KEY).as_str()
    );
    let warm = body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("BENCH_ID=warmup");
    if !warm {
        let count = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        state.peak.fetch_max(count, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(DELAY_MS)).await;
    }
    let reply = state
        .http
        .post(format!("{}/chat/completions", state.upstream))
        .bearer_auth(model_service::KEY)
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    if !warm {
        state.active.fetch_sub(1, Ordering::SeqCst);
    }
    Json(reply)
}

fn task(id: &str, environment: &str) -> TaskSpec {
    let mut task = agent_fixture::task(id, environment);
    task.retain_bundle = false;
    task.retain_artifacts = None;
    let RunInvocation::Process(process) = &mut task.run.invocation;
    process.env.remove("PVISOR_TEST_BINARY_ARTIFACT");
    process
        .env
        .insert("PVISOR_INFERENCE_BENCH_ID".into(), id.into());
    task
}
fn quantile(values: &[f64], percentile: usize) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    values[(values.len() * percentile).div_ceil(100).saturating_sub(1)]
}

struct Experiment<'a> {
    root: &'a Path,
    cache: &'a Path,
    environment: &'a EnvironmentTemplate,
    binary: &'a Path,
    firmware: &'a Path,
}
impl Experiment<'_> {
    async fn trial(&self, block: usize, idle: bool) -> Value {
        let directory = self.root.join(format!("block-{block}-idle-{idle}"));
        fs::create_dir(&directory).unwrap();
        let supplier = model_service::ModelService::start_held().await;
        supplier.release();
        let delay = Delay {
            upstream: supplier.base_url.clone(),
            http: reqwest::Client::new(),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            received: Arc::new(AtomicUsize::new(0)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let model_router = Router::new()
            .route("/v1/chat/completions", post(delayed))
            .with_state(delay.clone());
        let model = tokio::spawn(async move {
            axum::serve(listener, model_router).await.unwrap();
        });
        let scheduler =
            Scheduler::open(&directory.join("journal"), SchedulerConfig::default()).unwrap();
        let router =
            pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let admin = Client::new(&url, ADMIN.into()).unwrap();
        let environment = admin.publish_environment(self.environment).await.unwrap();
        let config = directory.join("worker.toml");
        fs::write(&config, format!("[environments]\nenabled = true\n[memory_sampling]\nenabled = true\ninterval_ms = 1000\n[cpu_sampling]\nenabled = true\ninterval_ms = 1000\n[vm]\nlibrary_dir = {}\n[overlaynet]\nmode = 'auto'\n[gateway]\nenabled = true\nrelease_cpu_on_idle = {idle}\nlevel = 'dialogue'\n[[gateway.routes]]\nname = '*'\nupstream = {}\napi_key_env = 'PVISOR_TEST_MODEL_KEY'\n", serde_json::to_string(&self.firmware.to_string_lossy()).unwrap(), serde_json::to_string(&model_url).unwrap())).unwrap();
        let log = directory.join("worker.log");
        let name = format!("pvisor-test-inference-{}.service", uuid::Uuid::new_v4());
        let launcher = Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--service-type=exec",
                "--property=CPUQuota=200%",
                "--property=MemoryMax=4294967296",
                "--property=MemorySwapMax=0",
                "--property=OOMPolicy=kill",
                "--property=KillMode=mixed",
                "--property=TimeoutStopSec=10s",
                "--property=Delegate=no",
                "--setenv=PVISOR_CLUSTER_WORKER_TOKEN",
                "--setenv=PVISOR_TEST_MODEL_KEY",
                "--setenv=PVISOR_CACHE_BACKEND",
                "--setenv=PVISOR_CACHE_LOCATION",
                "--setenv=XDG_CACHE_HOME",
                "--setenv=TOKIO_WORKER_THREADS",
                "--setenv=PATH",
            ])
            .arg(format!("--unit={name}"))
            .arg(format!("--working-directory={}", directory.display()))
            .arg(self.binary)
            .args([
                "--url",
                &url,
                "--id",
                "agent-benchmark",
                "--backend",
                "vm",
                "--poll-ms",
                "20",
                "--slots",
                "8",
                "--cpu-millis",
                "2000",
                "--memory-bytes",
                "2147483648",
            ])
            .arg("--state")
            .arg(directory.join("worker"))
            .arg("--config")
            .arg(config)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_TEST_MODEL_KEY", model_service::KEY)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", self.cache)
            .env("XDG_CACHE_HOME", directory.join("local-cache"))
            .env("TOKIO_WORKER_THREADS", "2")
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&log).unwrap()))
            .spawn()
            .unwrap();
        let service = Service { name, launcher };
        let worker_pid = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(pid) = service.property("MainPID").parse::<u32>()
                    && pid > 0
                    && !admin.workers().await.unwrap().is_empty()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        let group = service.group();
        let cpu_max = fs::read_to_string(group.join("cpu.max")).unwrap();
        let quota: Vec<u64> = cpu_max
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(quota[0], quota[1] * 2);
        assert_eq!(scalar(&group, "memory.max"), MEMORY_MAX);
        assert_eq!(scalar(&group, "memory.swap.max"), 0);
        let worker_group = fs::read_to_string(format!("/proc/{worker_pid}/cgroup")).unwrap();
        assert_ne!(
            worker_group,
            fs::read_to_string("/proc/self/cgroup").unwrap()
        );
        // One real tool task warms this arm's mounts and interpreter. It has
        // identical limits but no artificial supplier delay, and is excluded
        // from burst timing and CPU deltas.
        admin
            .submit(&task("warmup", &environment.digest))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let record = admin.task("warmup").await.unwrap();
                if record.phase.terminal() {
                    assert_eq!(
                        record.phase,
                        TaskPhase::Succeeded,
                        "{record:?}\n{}",
                        fs::read_to_string(&log).unwrap()
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        let cpu_before = counters(&group, "cpu.stat");
        let memory_events_before = counters(&group, "memory.events");
        let begun = Instant::now();
        let mut submissions = BTreeMap::new();
        for sequence in 0..TASKS {
            let id = format!("agent-{sequence:02}");
            submissions.insert(id.clone(), begun.elapsed().as_secs_f64() * 1000.0);
            admin.submit(&task(&id, &environment.digest)).await.unwrap();
        }
        let mut completed = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut samples = vec![];
        tokio::time::timeout(Duration::from_secs(90), async {
            while completed.len() < TASKS {
                let mut native = BTreeSet::new();
                let mut candidates = vec![];
                let mut rss = 0u64;
                let mut paused = 0;
                for id in submissions.keys() {
                    let record = admin.task(id).await.unwrap();
                    if let Some(sample) = &record.memory_sample && let Some(usage) = &sample.report.sample.usage {
                        identities.entry(id.clone()).or_insert_with(|| serde_json::to_value(usage).unwrap());
                        if !record.phase.terminal() { candidates.push(usage.clone()); }
                    }
                    paused += usize::from(record.phase == TaskPhase::Paused);
                    if record.phase.terminal() && !completed.contains_key(id) {
                        assert_eq!(record.phase, TaskPhase::Succeeded, "{record:?}\n{}", fs::read_to_string(&log).unwrap());
                        let result = record.result.as_ref().unwrap();
                        assert_eq!(result.output.stdout.as_deref(), Some("agent loop completed: 3 tests passed; unauthorized model denied\n"));
                        let bundle: pvisor::RunBundle = serde_json::from_slice(&fs::read(directory.join(format!("worker/tasks/{id}-1/run-bundle.json"))).unwrap()).unwrap();
                        let plan = bundle.executor_plan.unwrap();
                        assert_eq!(plan.kind, ExecutorKind::VirtualMachine);
                        assert_eq!(plan.isolation, IsolationKind::VirtualMachine);
                        completed.insert(id.clone(), json!({"submitted_ms":submissions[id],"completed_ms":begun.elapsed().as_secs_f64()*1000.0,"native_cpu":record.cpu_sample}));
                    }
                }
                // Probe all candidates together after HTTP collection; an old
                // sample of an exited/reused PID never increases live density.
                for usage in &candidates {
                    if let Some(bytes) = resident(usage, worker_pid, &worker_group) {
                        rss += bytes;
                        assert!(native.insert(usage.pid));
                    }
                }
                let reserved = admin.workers().await.unwrap()[0].reserved;
                assert!(reserved.cpu_millis <= 2000 && reserved.memory_bytes <= 2147483648 && reserved.slots <= 8);
                samples.push(json!({"elapsed_ms":begun.elapsed().as_secs_f64()*1000.0,"native_vm_count":native.len(),"native_vm_rss_sum_bytes":rss,"paused_count":paused,"supplier_waits":delay.active.load(Ordering::SeqCst),"worker_scope_memory_current_bytes":scalar(&group,"memory.current"),"reserved":reserved}));
                if completed.len() < TASKS { tokio::time::sleep(Duration::from_millis(100)).await; }
            }
        }).await.unwrap_or_else(|error| panic!("{error}\n{}", fs::read_to_string(&log).unwrap()));
        let wall_seconds = begun.elapsed().as_secs_f64();
        assert_eq!(
            identities.len(),
            TASKS,
            "each result needs actual native identity evidence"
        );
        assert_eq!(supplier.calls.lock().unwrap().len(), (TASKS + 1) * 2);
        let cpu_after = counters(&group, "cpu.stat");
        let memory_events_after = counters(&group, "memory.events");
        for key in ["oom", "oom_kill"] {
            assert_eq!(memory_events_after[key], memory_events_before[key]);
        }
        let cpu_usec = cpu_after["usage_usec"] - cpu_before["usage_usec"];
        let latencies: Vec<_> = completed
            .values()
            .map(|v| v["completed_ms"].as_f64().unwrap() - v["submitted_ms"].as_f64().unwrap())
            .collect();
        let maximum = |key: &str| {
            samples
                .iter()
                .map(|v| v[key].as_u64().unwrap())
                .max()
                .unwrap()
        };
        assert!(maximum("native_vm_count") > 0);
        let trial = json!({"block":block,"release_cpu_on_idle":idle,"completed_tasks":TASKS,"wall_seconds":wall_seconds,"tasks_per_second":TASKS as f64/wall_seconds,
            "latency_ms":{"p50":quantile(&latencies,50),"p95":quantile(&latencies,95),"p99":quantile(&latencies,99)},
            "peak_native_vms_sampled":maximum("native_vm_count"),"peak_paused_tasks_sampled":maximum("paused_count"),"peak_supplier_waits":delay.peak.load(Ordering::SeqCst),
            "peak_worker_scope_memory_current_sampled_bytes":maximum("worker_scope_memory_current_bytes"),"scope_lifetime_memory_peak_bytes":scalar(&group,"memory.peak"),"peak_native_rss_sum_sampled_bytes":maximum("native_vm_rss_sum_bytes"),
            "worker_scope_cpu_usec":cpu_usec,"worker_scope_cpu_usec_per_task":cpu_usec/TASKS as u64,"cpu_before":cpu_before,"cpu_after":cpu_after,"memory_events_before":memory_events_before,"memory_events_after":memory_events_after,
            "cpu_max":cpu_max.trim(),"worker_pid":worker_pid,"native_identities":identities,"supplier_request_count_including_warmup":delay.received.load(Ordering::SeqCst),"task_results":completed,"samples":samples});
        drop(service);
        server.abort();
        model.abort();
        trial
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "native Agent throughput experiment; run just bench-cluster-inference; needs KVM/FUSE/Python3/user-systemd/firmware"]
async fn ordinary_and_cooperative_waits_complete_identical_native_agent_workloads() {
    let output = PathBuf::from(
        std::env::var_os("PVISOR_INFERENCE_BENCH_OUT").expect("absolute JSON report path required"),
    );
    assert!(output.is_absolute());
    let blocks = std::env::var("PVISOR_INFERENCE_BENCH_BLOCKS")
        .ok()
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(3);
    assert!((1..=9).contains(&blocks));
    let firmware = PathBuf::from(
        std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR").expect("regular firmware directory required"),
    );
    assert!(
        fs::symlink_metadata(firmware.join("libkrunfw.so.5"))
            .unwrap()
            .is_file()
    );
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let cache = root.join("cache");
    let source = root.join("source");
    let script = include_str!("fixtures/cluster_agent_loop.py")
        .replace("headers={\"Content-Type\":", "headers={\"x-pvisor-inference-idle\": \"true\", \"Content-Type\":")
        .replace("first = request(messages)", "messages[0]['content'] += '\\nBENCH_ID=' + os.environ['PVISOR_INFERENCE_BENCH_ID']\nfirst = request(messages)");
    let (base, python, scaffold) = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        let script = script.clone();
        move || {
            (
                native_cache::publish_layer(
                    &source,
                    &cache,
                    "agent-base",
                    &[("env/input", "Implement multiply(a,b).\n")],
                    true,
                ),
                agent_fixture::python_layer(&source, &cache),
                native_cache::publish_layer(
                    &source,
                    &cache,
                    "agent-scaffold",
                    &[("toolkit/agent.py", &script)],
                    false,
                ),
            )
        }
    })
    .await
    .unwrap();
    let environment = EnvironmentTemplate {
        version: CLUSTER_VERSION,
        architecture: "amd64".into(),
        base,
        workspace: None,
        toolkits: vec![python, scaffold],
    };
    let binary = root.join("pvisor-worker");
    fs::copy(env!("CARGO_BIN_EXE_pvisor-worker"), &binary).unwrap();
    let mut report = json!({"schema_version":1,"complete":false,"started_at_unix_ms":pvisor_core::unix_now_ms(),"revision":text("git",&["rev-parse","HEAD"]),"dirty_files":text("git",&["status","--short"]),"kernel":text("uname",&["-srmo"]),"rustc":text("rustc",&["--version"]),"python":text("/usr/bin/python3",&["--version"]),
        "compiled_input_sha256":{"benchmark":digest(include_bytes!("cluster_inference_benchmark.rs")),"agent_fixture":digest(include_bytes!("common/agent_fixture.rs")),"model_fixture":digest(include_bytes!("common/model_service.rs")),"native_cache":digest(include_bytes!("common/native_cache.rs")),"cargo_lock":digest(include_bytes!("../../../Cargo.lock"))},
        "worker_binary_sha256":digest(&fs::read(&binary).unwrap()),"firmware_sha256":digest(&fs::read(firmware.join("libkrunfw.so.5")).unwrap()),"scaffold_sha256":digest(script.as_bytes()),"environment":environment,
        "scope":"single-host deterministic model replies with real native Python/tool work; not model quality or production/multi-host density",
        "conditions":{"tasks_per_arm":TASKS,"model_delay_ms_per_call":DELAY_MS,"model_calls_per_task":2,"kernel_cpu_quota_percent":200,"kernel_memory_max_bytes":MEMORY_MAX,"worker_capacity":{"slots":8,"memory_bytes":2147483648u64,"cpu_millis":2000},"ram_bytes_per_guest":268435456,"sampling":"100 ms coordinator polls; native identity telemetry every 1000 ms; completion quantization includes polling", "warmup":"one real tool task per arm without artificial delay, excluded from CPU/time deltas", "excluded_costs":"controller, model fixture and publisher run outside the Worker service"},"trials":[]});
    save(&output, &report);
    let experiment = Experiment {
        root,
        cache: &cache,
        environment: &environment,
        binary: &binary,
        firmware: &firmware,
    };
    for block in 0..blocks {
        let order = if block % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for idle in order {
            let trial = experiment.trial(block, idle).await;
            report["trials"].as_array_mut().unwrap().push(trial);
            save(&output, &report);
        }
    }
    let trials = report["trials"].as_array().unwrap();
    let summaries: Vec<_> = (0..blocks).map(|block| {
        let pick = |idle| trials.iter().find(|v| v["block"]==block && v["release_cpu_on_idle"]==idle).unwrap();
        let normal = pick(false); let idle = pick(true);
        json!({"block":block,"throughput_ratio_idle_over_ordinary":idle["tasks_per_second"].as_f64().unwrap()/normal["tasks_per_second"].as_f64().unwrap(),"peak_native_vm_ratio":idle["peak_native_vms_sampled"].as_f64().unwrap()/normal["peak_native_vms_sampled"].as_f64().unwrap(),"worker_cpu_cost_ratio":idle["worker_scope_cpu_usec"].as_f64().unwrap()/normal["worker_scope_cpu_usec"].as_f64().unwrap(),"worker_memory_peak_ratio":idle["peak_worker_scope_memory_current_sampled_bytes"].as_f64().unwrap()/normal["peak_worker_scope_memory_current_sampled_bytes"].as_f64().unwrap()})
    }).collect();
    report["paired_results"] = json!(summaries);
    report["complete"] = true.into();
    report["finished_at_unix_ms"] = pvisor_core::unix_now_ms().into();
    save(&output, &report);
}
