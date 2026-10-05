//! Opt-in real Linux namespace and KVM/FUSE execution. No simulated VM result.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::FileTypeExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

#[path = "common/native_cache.rs"]
mod native_cache;
use native_cache::publish_layer;

const ADMIN: &str = "vm-test-admin-0123456789012345";
const WORKER: &str = "vm-test-worker-0123456789012345";

fn worker_binary(root: &Path) -> std::path::PathBuf {
    let binary = root.join("pvisor-worker");
    if !binary.exists() {
        // A concurrent Cargo build may replace the shared executable. Native
        // reentry and checkpoint build identity must use one immutable binary
        // throughout each gate, including Worker restart and cold restore.
        fs::copy(env!("CARGO_BIN_EXE_pvisor-worker"), &binary).unwrap();
    }
    binary
}

fn assert_native_capture_inodes_transferred(
    machine: &serde_json::Value,
    original_root: &[u8],
    published_root: &Path,
) {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    fn walk(value: &serde_json::Value, source: &Path, sealed: &Path, count: &mut usize) {
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("Passthrough") {
            let state = &value["state"];
            let root: Vec<u8> = serde_json::from_value(state["root"].clone()).unwrap();
            let root = Path::new(std::ffi::OsStr::from_bytes(&root));
            let relative = root.strip_prefix(source).unwrap();
            assert_eq!(relative.components().count(), 1);
            let identity = &state["inodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|inode| inode["path"].as_array().is_some_and(Vec::is_empty))
                .unwrap()["identity"];
            let metadata = fs::metadata(sealed.join(relative)).unwrap();
            assert_eq!(metadata.dev(), identity["dev"].as_u64().unwrap());
            assert_eq!(
                metadata.ino(),
                identity["ino"].as_u64().unwrap(),
                "native capture was copied again at publication: {}",
                relative.display()
            );
            *count += 1;
        }
        match value {
            serde_json::Value::Object(fields) => {
                for child in fields.values() {
                    walk(child, source, sealed, count);
                }
            }
            serde_json::Value::Array(children) => {
                for child in children {
                    walk(child, source, sealed, count);
                }
            }
            _ => {}
        }
    }
    let source = Path::new(std::ffi::OsStr::from_bytes(original_root));
    assert!(
        !source.exists(),
        "published native capture must relinquish its path"
    );
    let mut count = 0;
    walk(machine, source, published_root, &mut count);
    assert!(count >= 2, "must check both upper and lower physical roots");
}

fn assert_incremental_ram(
    published: &pvisor::environment_snapshot::PublishedEnvironment,
    baseline: &pvisor::environment_snapshot::RamBlocks,
    baseline_hash: &str,
) {
    let machine: serde_json::Value =
        serde_json::from_slice(&published.machine_bytes().unwrap()).unwrap();
    let delta: pvisor_vm::api::RamDeltaCapture =
        serde_json::from_value(machine["ram_delta"].clone())
            .expect("restored compressed VM must use the actual incremental path");
    pvisor_vm::api::RamDeltaState::validate(&delta).unwrap();
    assert_eq!(delta.base_sha256, baseline_hash);
    assert_eq!(delta.length, baseline.length);
    assert_eq!(delta.block_bytes as usize, pvisor::ram_backing::BLOCK_BYTES);
    assert!(
        delta.changed_blocks.len() < baseline.blocks.len(),
        "recapture must preserve some clean RAM without reading the live mapping"
    );
    let child = published.manifest().ram_blocks.as_ref().unwrap();
    assert_eq!(child.length, baseline.length);
    for (index, (parent, child)) in baseline.blocks.iter().zip(&child.blocks).enumerate() {
        if delta.changed_blocks.binary_search(&(index as u64)).is_err() {
            assert_eq!(child.id, parent.id);
            assert_eq!(child.length, parent.length);
        }
    }
    eprintln!(
        "native incremental RAM: {} / {} blocks captured ({} KiB each)",
        delta.changed_blocks.len(),
        baseline.blocks.len(),
        delta.block_bytes / 1024
    );
}

#[derive(Debug, Default)]
struct SnapshotRamUsage {
    // Device, inode and path identify the actual mapped kernel cache object.
    identities: std::collections::BTreeSet<(String, String, String)>,
    rss_kib: u64,
    pss_kib: u64,
    shared_clean_kib: u64,
    private_dirty_kib: u64,
}

fn snapshot_ram_usage(pid: u32) -> SnapshotRamUsage {
    let smaps = fs::read_to_string(format!("/proc/{pid}/smaps")).unwrap();
    let mut usage = SnapshotRamUsage::default();
    let mut ram = false;
    for line in smaps.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first().is_some_and(|f| f.contains('-')) {
            ram = fields
                .get(5)
                .is_some_and(|p| p.contains("/ram-mount-") && p.ends_with("/ram"));
            if ram {
                assert_eq!(fields[1], "rw-p", "guest RAM must be private COW");
                usage
                    .identities
                    .insert((fields[3].into(), fields[4].into(), fields[5].into()));
            }
        } else if ram && fields.len() >= 2 {
            let value = match fields[0] {
                "Rss:" => &mut usage.rss_kib,
                "Pss:" => &mut usage.pss_kib,
                "Shared_Clean:" => &mut usage.shared_clean_kib,
                "Private_Dirty:" => &mut usage.private_dirty_kib,
                _ => continue,
            };
            *value += fields[1].parse::<u64>().unwrap();
        }
    }
    assert_eq!(
        usage.identities.len(),
        1,
        "missing or mixed snapshot RAM: {usage:?}"
    );
    usage
}

#[cfg(not(target_env = "musl"))]
fn maps_libkrunfw(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/maps"))
        .unwrap()
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .any(|path| {
            Path::new(path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("libkrunfw.so"))
        })
}

async fn restored_vm_pids(
    worker_pid: u32,
    store: &Path,
    worker_log: &Path,
    client: &Client,
    tasks: [&str; 2],
) -> [u32; 2] {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            for id in tasks {
                let task = client.task(id).await.unwrap();
                assert!(
                    !task.phase.terminal(),
                    "restored task {id} terminated before RAM mapping: error={:?}, result={:?}",
                    task.error,
                    task.result
                );
            }
            // Tokio can spawn runners from any thread. The other child is the
            // mount watchdog; only native VMMs map the snapshot RAM inode.
            let mut pids = std::collections::BTreeSet::new();
            for thread in fs::read_dir(format!("/proc/{worker_pid}/task")).unwrap() {
                let children = fs::read_to_string(thread.unwrap().path().join("children")).unwrap();
                for pid in children.split_whitespace() {
                    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
                    if maps.lines().any(|line| {
                        line.contains(&store.join("ram-mounts").to_string_lossy().to_string())
                            && line.ends_with("/ram")
                    }) {
                        pids.insert(pid.parse::<u32>().unwrap());
                    }
                }
            }
            if pids.len() == 2 {
                break pids.into_iter().collect::<Vec<_>>().try_into().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "native RAM mapping evidence deadline: {error}\n{}",
            fs::read_to_string(worker_log).unwrap()
        )
    })
}

async fn measured_memory(client: &Client, id: &str, after_ms: u64) -> ReceivedMemorySample {
    let waited = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let task = client.task(id).await.unwrap();
            assert!(
                !task.phase.terminal(),
                "VM ended before memory observation: {task:?}"
            );
            if let Some(sample) = task.memory_sample
                && sample.report.sample.sampled_at_unix_ms >= after_ms
                && sample.report.sample.usage.is_some()
            {
                sample.report.sample.validate().unwrap();
                break sample;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    match waited {
        Ok(sample) => sample,
        Err(error) => panic!(
            "physical memory observation deadline: {error}\n{:?}",
            client.task(id).await
        ),
    }
}

async fn measured_node_memory(client: &Client, after_ms: u64) -> ReceivedNodeMemorySample {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let workers = client.workers().await.unwrap();
            if let Some(sample) = workers
                .iter()
                .find(|worker| worker.registration.id == "shared-ram-worker")
                .and_then(|worker| worker.memory_sample.as_ref())
                && sample.report.sample.sampled_at_unix_ms >= after_ms
            {
                sample.report.sample.validate().unwrap();
                break sample.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("node memory report deadline")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn concurrent_restores_share_physical_ram_baseline_and_keep_private_writes() {
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cache = root.join("cache");
    let layer = tokio::task::spawn_blocking({
        let source = root.join("source");
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "shared-ram-base",
                &[("env/seed", "saved-memory\n")],
                true,
            )
        }
    })
    .await
    .unwrap();
    let toolkit = tokio::task::spawn_blocking({
        let source = root.join("toolkit-source");
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "shared-filesystem-toolkit",
                &[("env/copyup", "immutable-lower\n")],
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
    let telemetry_entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay_telemetry = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let node_entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay_node = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let node_missing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let node_missing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let router = router.layer(axum::middleware::from_fn({
        let entered = telemetry_entered.clone();
        let delayed = delay_telemetry.clone();
        let node_entered = node_entered.clone();
        let node_delayed = delay_node.clone();
        let node_missing = node_missing.clone();
        let node_missing_calls = node_missing_calls.clone();
        move |request: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
            let entered = entered.clone();
            let delayed = delayed.clone();
            let node_entered = node_entered.clone();
            let node_delayed = node_delayed.clone();
            let node_missing = node_missing.clone();
            let node_missing_calls = node_missing_calls.clone();
            async move {
                if request.uri().path() == "/v1/workers/node-memory"
                    && node_missing.load(std::sync::atomic::Ordering::SeqCst)
                {
                    node_missing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    use axum::response::IntoResponse;
                    return axum::http::StatusCode::NOT_FOUND.into_response();
                }
                if request.uri().path() == "/v1/workers/memory"
                    && delayed.load(std::sync::atomic::Ordering::SeqCst)
                {
                    entered.store(true, std::sync::atomic::Ordering::SeqCst);
                    // Longer than the Client HTTP timeout and the live lease.
                    tokio::time::sleep(Duration::from_millis(5500)).await;
                }
                if request.uri().path() == "/v1/workers/node-memory"
                    && node_delayed.load(std::sync::atomic::Ordering::SeqCst)
                {
                    node_entered.store(true, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5500)).await;
                }
                next.run(request).await
            }
        }
    }));
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
            base: layer,
            workspace: None,
            toolkits: vec![toolkit],
        })
        .await
        .unwrap();
    let mut profile =
        "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n".to_owned();
    profile += "[memory_sampling]\nenabled = true\ninterval_ms = 1000\n";
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        profile += &format!(
            "[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        );
    }
    let config = root.join("worker.toml");
    let mut worker_unit = if std::env::var_os("PVISOR_TEST_WORKER_SYSTEMD").is_some() {
        profile += "[admission]\nmode = 'linux_pressure'\nmemory_reserve_bytes = 134217728\ncpu_overcommit_bps = 20000\n";
        Some(WorkerUnit(format!(
            "pvisor-test-worker-{}.service",
            uuid::Uuid::new_v4()
        )))
    } else {
        None
    };
    fs::write(&config, profile).unwrap();
    let worker_log = root.join("worker.log");
    let mut worker_command = if let Some(unit) = &worker_unit {
        let mut command = Command::new("systemd-run");
        command
            .args([
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--service-type=exec",
                "--property=MemoryAccounting=yes",
                "--property=MemoryHigh=805306368",
                "--property=MemoryMax=1073741824",
                "--property=MemorySwapMax=0",
                "--property=CPUQuota=100%",
                "--property=Delegate=no",
                "--property=KillMode=mixed",
                "--property=TimeoutStopSec=30s",
                "--property=OOMPolicy=kill",
                "--setenv=PVISOR_CLUSTER_WORKER_TOKEN",
                "--setenv=PVISOR_CACHE_BACKEND",
                "--setenv=PVISOR_CACHE_LOCATION",
                "--setenv=PVISOR_STARTUP_TIMING",
                "--setenv=XDG_CACHE_HOME",
                "--setenv=PATH",
                "--setenv=TOKIO_WORKER_THREADS",
            ])
            .arg(format!("--unit={}", unit.0))
            .arg(format!(
                "--working-directory={}",
                std::env::current_dir().unwrap().display()
            ))
            .arg(worker_binary(root));
        command
    } else {
        Command::new(worker_binary(root))
    };
    let worker = ChildGuard(
        worker_command
            .args([
                "--url",
                &url,
                "--id",
                "shared-ram-worker",
                "--backend",
                "vm",
                "--poll-ms",
                "50",
                "--slots",
                "2",
                "--cpu-millis",
                "2000",
                "--memory-bytes",
                "536870912",
            ])
            .arg("--state")
            .arg(root.join("worker"))
            .arg("--config")
            .arg(&config)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .env("PVISOR_STARTUP_TIMING", "1")
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            // Lifecycle filesystem work must not starve a one-thread async
            // runtime while native VMs and their FUSE readers remain active.
            .env("TOKIO_WORKER_THREADS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap(),
    );
    // A real idle Worker reports supervisor/node overhead without a task lease.
    let idle_node = measured_node_memory(&admin, pvisor_core::unix_now_ms()).await;
    let pvisor_core::memory::MemoryObservation::Measured {
        usage: idle_supervisor,
    } = &idle_node.report.sample.supervisor
    else {
        panic!("idle supervisor sample failed: {idle_node:?}")
    };
    let worker_pid = idle_supervisor.pid;
    if let Some(unit) = &worker_unit {
        unit.assert_membership(&idle_node, &[worker_pid]);
    } else {
        assert_eq!(worker_pid, worker.0.id());
    }
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    eprintln!("idle worker physical memory: {idle_node:?}");
    let mut original = task("shared-source", &environment.digest, "unused");
    original.run.runtime.timeout_ms = Some(90_000);
    let RunInvocation::Process(process) = &mut original.run.invocation;
    // Both branches resume with the same memory token and open FD. Each then
    // writes its own token/descriptor and reads it back after the peer diverges.
    process.args = vec!["-c".into(), "set -eu; read -r token < /env/seed; : > /env/seed; printf 'saved-fd\\n' > /env/owned; exec 3<>/env/owned; printf 'ready\\n' > /env/ready; while [ ! -e /env/branch ]; do /bin/sleep 0.2; done; read -r branch < /env/branch; printf '%s\\n' \"$branch\" > /env/copyup; read -r owned <&3; token=\"$branch:$token\"; printf '%s\\n' \"$token\" >&3; printf 'ready\\n' > /env/diverged; while [ ! -e /env/finish ]; do /bin/sleep 0.2; done; { read -r unchanged; read -r changed; } < /env/owned; test \"$unchanged\" = \"$owned\"; printf '%s|%s|%s\\n' \"$token\" \"$owned\" \"$changed\"".into()];
    admin.submit(&original).await.unwrap();
    guest_ready(
        &admin,
        &original.id,
        &root.join("worker/tasks/shared-source-1/upper/env/ready"),
        &worker_log,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !telemetry_entered.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("first physical telemetry request");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let alive = admin.task(&original.id).await.unwrap();
    assert_eq!(alive.phase, TaskPhase::Running);
    assert!(
        alive.lease.unwrap().expires_at_ms > pvisor_core::unix_now_ms(),
        "telemetry timeout must not block lease renewal"
    );
    controlled(
        &admin,
        &original.id,
        "pause-during-telemetry",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    controlled(
        &admin,
        &original.id,
        "resume-during-telemetry",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    delay_telemetry.store(false, std::sync::atomic::Ordering::SeqCst);
    let source_memory = measured_memory(&admin, &original.id, pvisor_core::unix_now_ms()).await;
    delay_node.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(4), async {
        while !node_entered.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("node observation request was never sent");
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let alive = admin.task(&original.id).await.unwrap();
    assert_eq!(alive.phase, TaskPhase::Running);
    assert!(
        alive.lease.unwrap().expires_at_ms > pvisor_core::unix_now_ms(),
        "node telemetry timeout blocked lease renewal"
    );
    controlled(
        &admin,
        &original.id,
        "pause-during-node-telemetry",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    controlled(
        &admin,
        &original.id,
        "resume-during-node-telemetry",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    delay_node.store(false, std::sync::atomic::Ordering::SeqCst);
    let source_node = measured_node_memory(&admin, pvisor_core::unix_now_ms()).await;
    let pvisor_core::memory::MemoryObservation::Measured { usage: supervisor } =
        &source_node.report.sample.supervisor
    else {
        panic!("supervisor observation failed: {source_node:?}")
    };
    assert_eq!(supervisor.pid, worker_pid);
    assert!(supervisor.process.rss_bytes > 0 && supervisor.process.pss_bytes > 0);
    let pvisor_core::memory::MemoryObservation::Measured { usage: system } =
        &source_node.report.sample.system
    else {
        panic!("system observation failed: {source_node:?}")
    };
    assert_eq!(
        system.host_boot_id,
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
    );
    let pvisor_core::memory::MemoryObservation::Measured { usage: cgroup } =
        &source_node.report.sample.cgroup
    else {
        panic!("real cgroup observation failed: {source_node:?}")
    };
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(&cgroup.directory).unwrap();
    assert_eq!(
        (cgroup.device, cgroup.inode),
        (metadata.dev(), metadata.ino())
    );
    assert!(cgroup.current_bytes > 0 && cgroup.stat["anon"] > 0 && cgroup.stat["file"] > 0);
    // The real gate requires memory-controller kernel accounting, rather than
    // substituting process RSS for page-cache and kernel allocations.
    assert!(cgroup.stat.get("kernel").is_some_and(|bytes| *bytes > 0));
    #[cfg(not(target_env = "musl"))]
    assert!(maps_libkrunfw(
        source_memory.report.sample.usage.as_ref().unwrap().pid
    ));
    assert!(
        source_memory
            .report
            .sample
            .usage
            .as_ref()
            .unwrap()
            .guest_ram
            .rss_bytes
            > 0
    );
    let sealed = controlled(
        &admin,
        &original.id,
        "share-point",
        ControlAction::Suspend,
        &worker_log,
    )
    .await;
    let Some(ControlOutcome::Checkpointed { checkpoint }) = sealed.outcome else {
        panic!("no sealed RAM snapshot")
    };
    assert_eq!(
        finished(&admin, &original.id).await.phase,
        TaskPhase::Suspended
    );
    let mut first = original.clone();
    first.id = "shared-first".into();
    first.run.run_id = first.id.clone().into();
    first.run.parent_run_id = Some(original.run.run_id.clone());
    first.restore = Some(ExecutionRestore {
        task_id: original.id.clone(),
        request_id: "share-point".into(),
    });
    let mut second = first.clone();
    second.id = "shared-second".into();
    second.run.run_id = second.id.clone().into();
    // Atomic branch creation precedes concurrent admission/preparation and the
    // real RAM-mount singleflight. Retrying a lost response must not duplicate it.
    let fork = ExecutionForkRequest {
        version: CLUSTER_VERSION,
        request_id: "shared-pair".into(),
        checkpoint_request_id: "share-point".into(),
        branches: vec![
            ExecutionForkBranch {
                task_id: first.id.clone(),
                run_id: first.run.run_id.clone(),
            },
            ExecutionForkBranch {
                task_id: second.id.clone(),
                run_id: second.run.run_id.clone(),
            },
        ],
    };
    let worker_api = Client::new(&url, WORKER.into()).unwrap();
    let unauthorized = worker_api
        .fork_execution(&original.id, &fork)
        .await
        .unwrap_err();
    assert_eq!(
        unauthorized
            .downcast_ref::<reqwest::Error>()
            .and_then(|e| e.status()),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    assert!(
        worker_api
            .execution_fork(&original.id, &fork.request_id)
            .await
            .is_err()
    );
    let receipt = admin.fork_execution(&original.id, &fork).await.unwrap();
    assert_eq!(receipt.checkpoint, checkpoint);
    assert_eq!(
        admin.fork_execution(&original.id, &fork).await.unwrap(),
        receipt
    );
    assert_eq!(
        admin
            .execution_fork(&original.id, &fork.request_id)
            .await
            .unwrap(),
        receipt
    );
    let [first_pid, second_pid] = restored_vm_pids(
        worker_pid,
        &checkpoint.store,
        &worker_log,
        &admin,
        [&first.id, &second.id],
    )
    .await;
    assert_ne!(first_pid, second_pid);
    let first_directory = root.join("worker/tasks/shared-first-1");
    let second_directory = root.join("worker/tasks/shared-second-1");
    let first_upper = pvisor::RunRecord::read(&first_directory)
        .unwrap()
        .overlay
        .unwrap()
        .upper
        .path()
        .to_owned();
    let second_upper = pvisor::RunRecord::read(&second_directory)
        .unwrap()
        .overlay
        .unwrap()
        .upper
        .path()
        .to_owned();
    let first_overlay = pvisor::RunRecord::read(&first_directory)
        .unwrap()
        .overlay
        .unwrap();
    let second_overlay = pvisor::RunRecord::read(&second_directory)
        .unwrap()
        .overlay
        .unwrap();
    assert_ne!(first_overlay.target, second_overlay.target);
    for overlay in [&first_overlay, &second_overlay] {
        assert!(overlay.target.starts_with(root.join("worker/tasks")));
        assert!(
            !overlay
                .target
                .starts_with(checkpoint.store.join("filesystems"))
        );
    }
    let referenced_ids = |directory: &std::path::Path| {
        let mut ids: Vec<_> =
            fs::read_dir(directory.join("execution-restore/filesystem-references"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
        ids.sort();
        ids
    };
    let first_references = referenced_ids(&first_directory);
    assert_eq!(
        first_references.len(),
        1,
        "only the pure toolkit lower may be shared"
    );
    assert_eq!(first_references, referenced_ids(&second_directory));
    let shared_lower = checkpoint
        .store
        .join("filesystems")
        .join(&first_references[0])
        .join("tree");
    assert!(first_overlay.protect_target && second_overlay.protect_target);
    assert_eq!(
        fs::read(shared_lower.join("env/copyup")).unwrap(),
        b"immutable-lower\n"
    );
    let filesystem_id = shared_lower.parent().unwrap().file_name().unwrap();
    for directory in [&first_directory, &second_directory] {
        let reference = directory
            .join("execution-restore/filesystem-references")
            .join(filesystem_id);
        assert_eq!(
            fs::metadata(reference).unwrap().ino(),
            fs::metadata(shared_lower.parent().unwrap().join("reference"))
                .unwrap()
                .ino(),
        );
    }
    assert_ne!(first_upper, second_upper);
    fs::write(first_upper.join("env/branch"), b"first\n").unwrap();
    fs::write(second_upper.join("env/branch"), b"second\n").unwrap();
    let first_diverged = first_upper.join("env/diverged");
    let second_diverged = second_upper.join("env/diverged");
    let ((), ()) = tokio::join!(
        guest_ready(&admin, &first.id, &first_diverged, &worker_log),
        guest_ready(&admin, &second.id, &second_diverged, &worker_log)
    );
    assert_eq!(
        fs::read(first_upper.join("env/copyup")).unwrap(),
        b"first\n"
    );
    assert_eq!(
        fs::read(second_upper.join("env/copyup")).unwrap(),
        b"second\n"
    );
    assert_eq!(
        fs::read(shared_lower.join("env/copyup")).unwrap(),
        b"immutable-lower\n"
    );
    if worker_unit.is_some() {
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            Resources {
                slots: 2,
                memory_bytes: 512 * 1024 * 1024,
                cpu_millis: 2000
            },
            "two running VMs must retain their full logical CPU budget under the one-CPU unit quota"
        );
    }
    controlled(
        &admin,
        &first.id,
        "pause-first",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    controlled(
        &admin,
        &second.id,
        "pause-second",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    let first_memory = measured_memory(&admin, &first.id, pvisor_core::unix_now_ms()).await;
    let second_memory = measured_memory(
        &admin,
        &second.id,
        first_memory.report.sample.sampled_at_unix_ms,
    )
    .await;
    let first_usage = first_memory.report.sample.usage.as_ref().unwrap();
    let second_usage = second_memory.report.sample.usage.as_ref().unwrap();
    let restored_node = measured_node_memory(&admin, pvisor_core::unix_now_ms()).await;
    let pvisor_core::memory::MemoryObservation::Measured { usage: supervisor } =
        &restored_node.report.sample.supervisor
    else {
        panic!("restored pager supervisor observation failed: {restored_node:?}")
    };
    assert_eq!(supervisor.pid, worker_pid);
    assert_ne!(supervisor.pid, first_usage.pid);
    assert_ne!(supervisor.pid, second_usage.pid);
    eprintln!("node physical memory with two restored VMs: {restored_node:?}");
    if let Some(unit) = &worker_unit {
        unit.assert_membership(&restored_node, &[worker_pid, first_pid, second_pid]);
        let worker_report = admin.workers().await.unwrap().remove(0);
        let admission = worker_report.admission.as_ref().unwrap();
        assert_eq!(admission.cpu_overcommit_bps, 20000);
        assert_eq!(
            admission.measurements.as_ref().unwrap().cpu_limit_millis,
            1000
        );
        assert_eq!(
            admission
                .measurements
                .as_ref()
                .unwrap()
                .local_cpu_quota_millis,
            Some(1000)
        );
        assert_eq!(admission.cpu_reservation_limit_millis(), Some(2000));
        assert!(worker_report.reserved.memory_bytes <= 512 * 1024 * 1024);
    }
    // An older controller can lack just the additive node endpoint. Only that
    // channel stops; the native VM observations and execution keep advancing.
    node_missing.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(4), async {
        while node_missing_calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("node endpoint compatibility probe deadline");
    measured_memory(&admin, &first.id, pvisor_core::unix_now_ms()).await;
    measured_memory(&admin, &second.id, pvisor_core::unix_now_ms()).await;
    assert_eq!(
        node_missing_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "404 must retire node sampling only once"
    );
    assert_ne!(first_usage.pid, second_usage.pid);
    for usage in [first_usage, second_usage] {
        #[cfg(not(target_env = "musl"))]
        assert!(
            !maps_libkrunfw(usage.pid),
            "restored VM redundantly loaded firmware"
        );
        assert!([first_pid, second_pid].contains(&usage.pid));
        assert!(usage.guest_ram.shared_clean_bytes > 0);
        assert!(usage.guest_ram.private_dirty_bytes > 0);
        assert!(
            usage.non_ram.rss_bytes > 0 && usage.process.pss_bytes >= usage.guest_ram.pss_bytes
        );
    }
    eprintln!("control-plane physical memory: first={first_memory:?} second={second_memory:?}");
    let one = snapshot_ram_usage(first_pid);
    let two = snapshot_ram_usage(second_pid);
    assert_eq!(
        one.identities, two.identities,
        "restores must map the same RAM inode"
    );
    assert!(
        one.shared_clean_kib > 0 && two.shared_clean_kib > 0,
        "no physical page sharing: {one:?} {two:?}"
    );
    assert!(
        one.private_dirty_kib > 0 && two.private_dirty_kib > 0,
        "guest writes must COW: {one:?} {two:?}"
    );
    assert!(
        one.pss_kib + two.pss_kib < one.rss_kib + two.rss_kib,
        "no resident RAM saving: {one:?} {two:?}"
    );
    eprintln!("shared restored RAM: first={one:?} second={two:?}");
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources {
            slots: 2,
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 0
        }
    );
    let ram_path = std::path::PathBuf::from(&one.identities.iter().next().unwrap().2);
    let mountpoint = ram_path.parent().unwrap();
    assert!(mountpoint.starts_with(checkpoint.store.join("ram-mounts")));
    assert_eq!(
        fs::read_dir(checkpoint.store.join("ram-mounts"))
            .unwrap()
            .count(),
        1
    );

    // Live readers pin their backing, but deletion must prohibit a new restore
    // even while a shared inode is still cached by the remaining branch.
    tokio::task::spawn_blocking({
        let checkpoint = checkpoint.clone();
        move || {
            let store =
                pvisor::environment_snapshot::SnapshotStore::new(&checkpoint.store).unwrap();
            store.delete(&checkpoint.snapshot_id).unwrap();
            store.collect_abandoned().unwrap();
        }
    })
    .await
    .unwrap();
    fs::write(first_upper.join("env/finish"), b"go\n").unwrap();
    controlled(
        &admin,
        &first.id,
        "resume-first",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    let done = finished(&admin, &first.id).await;
    assert_eq!(done.phase, TaskPhase::Succeeded, "{done:?}");
    assert!(
        done.memory_sample.is_none(),
        "terminal tasks must retire physical observations"
    );
    assert_eq!(
        done.result.unwrap().output.stdout.as_deref(),
        Some("first:saved-memory|saved-fd|first:saved-memory\n")
    );
    // The saved descriptor's offset was after the first line, so each branch
    // appends its independent COW token. Neither branch may see its peer's line.
    assert_eq!(
        fs::read(first_upper.join("env/owned")).unwrap(),
        b"saved-fd\nfirst:saved-memory\n"
    );
    assert_eq!(
        fs::read(second_upper.join("env/owned")).unwrap(),
        b"saved-fd\nsecond:saved-memory\n"
    );
    assert!(
        ram_path.exists(),
        "first branch exit must retain the shared pager"
    );
    assert_eq!(
        admin.task(&second.id).await.unwrap().phase,
        TaskPhase::Paused
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources {
            cpu_millis: 0,
            ..second.resources
        }
    );
    let remaining_memory = measured_memory(&admin, &second.id, pvisor_core::unix_now_ms()).await;
    let remaining_usage = remaining_memory.report.sample.usage.as_ref().unwrap();
    assert_eq!(remaining_usage.pid, second_usage.pid);
    assert_eq!(
        remaining_usage.start_time_ticks,
        second_usage.start_time_ticks
    );
    assert!(
        remaining_usage.guest_ram.pss_bytes > second_usage.guest_ram.pss_bytes,
        "remaining VM's proportional charge must reflect peer exit: {remaining_memory:?}"
    );
    let mut deleted_restore = first.clone();
    deleted_restore.id = "shared-deleted".into();
    deleted_restore.run.run_id = deleted_restore.id.clone().into();
    admin.submit(&deleted_restore).await.unwrap();
    assert_eq!(
        finished(&admin, &deleted_restore.id).await.phase,
        TaskPhase::Failed
    );
    assert!(ram_path.exists());
    fs::write(second_upper.join("env/finish"), b"go\n").unwrap();
    controlled(
        &admin,
        &second.id,
        "resume-second",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    let done = finished(&admin, &second.id).await;
    assert_eq!(done.phase, TaskPhase::Succeeded, "{done:?}");
    assert_eq!(
        done.result.unwrap().output.stdout.as_deref(),
        Some("second:saved-memory|saved-fd|second:saved-memory\n")
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while mountpoint.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("last restored Attempt must unmount and release its RAM baseline");
    // Retained Run records keep their lower available after both native owners
    // exit and after reopening the store; only Attempt collection drops pins.
    tokio::task::spawn_blocking({
        let store = checkpoint.store.clone();
        move || {
            pvisor::environment_snapshot::SnapshotStore::new(&store)
                .unwrap()
                .collect_abandoned()
                .unwrap()
        }
    })
    .await
    .unwrap();
    for directory in [&first_directory, &second_directory] {
        let overlay = pvisor::RunRecord::read(directory).unwrap().overlay.unwrap();
        assert!(overlay.target.is_dir());
        assert_eq!(referenced_ids(directory), first_references);
        assert!(shared_lower.is_dir());
    }
    assert_eq!(
        fs::read(shared_lower.join("env/copyup")).unwrap(),
        b"immutable-lower\n"
    );
    if let Some(unit) = worker_unit.take() {
        let scope = unit.directory();
        // Stop the unit with a live paused VMM. SIGTERM reaches the Worker,
        // which must cancel/reap its native child before the stop deadline.
        let mut stopping = original.clone();
        stopping.id = "shared-stop-live".into();
        stopping.run.run_id = stopping.id.clone().into();
        stopping.retain_bundle = false;
        admin.submit(&stopping).await.unwrap();
        guest_ready(
            &admin,
            &stopping.id,
            &root.join("worker/tasks/shared-stop-live-1/upper/env/ready"),
            &worker_log,
        )
        .await;
        controlled(
            &admin,
            &stopping.id,
            "pause-before-stop",
            ControlAction::Pause,
            &worker_log,
        )
        .await;
        let live = measured_memory(&admin, &stopping.id, pvisor_core::unix_now_ms()).await;
        let live_pid = live.report.sample.usage.as_ref().unwrap().pid;
        let members: Vec<u32> = fs::read_to_string(scope.join("cgroup.procs"))
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        assert!(members.contains(&worker_pid) && members.contains(&live_pid));
        unit.stop();
        let stopped = finished(&admin, &stopping.id).await;
        // No controller cancellation was requested: a Worker shutdown is a
        // failed task with native cancellation evidence, not a user cancel.
        assert_eq!(stopped.phase, TaskPhase::Failed, "{stopped:?}");
        assert_eq!(
            stopped.result.unwrap().state,
            pvisor_core::RunState::Cancelled
        );
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            Resources::default()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while scope.exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("stopped Worker unit must release its cgroup and every process");
        for pid in members {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "unit retained process {pid}"
            );
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn controller_suspends_frozen_vm_reuses_capacity_and_restores_after_worker_restart() {
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cache = root.join("cache");
    let layer = tokio::task::spawn_blocking({
        let source = root.join("source");
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "suspend-base",
                &[("env/seed", "saved-memory\n")],
                true,
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
            base: layer,
            workspace: None,
            toolkits: vec![],
        })
        .await
        .unwrap();
    let mut profile =
        "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n".to_owned();
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        profile += &format!(
            "[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        );
    }
    profile += "[cpu_sampling]\nenabled = true\ninterval_ms = 1000\n";
    let config = root.join("worker.toml");
    fs::write(&config, profile).unwrap();
    let worker_log = root.join("worker.log");
    let spawn_worker = || {
        ChildGuard(
            Command::new(worker_binary(root))
                .args([
                    "--url",
                    &url,
                    "--id",
                    "suspend-worker",
                    "--backend",
                    "vm",
                    "--poll-ms",
                    "50",
                    "--slots",
                    "1",
                    "--cpu-millis",
                    "1000",
                    "--memory-bytes",
                    "268435456",
                ])
                .arg("--state")
                .arg(root.join("worker"))
                .arg("--config")
                .arg(&config)
                .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
                .env("PVISOR_CACHE_BACKEND", "filesystem")
                .env("PVISOR_CACHE_LOCATION", &cache)
                .env("XDG_CACHE_HOME", root.join("local-cache"))
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&worker_log)
                        .unwrap(),
                ))
                .spawn()
                .unwrap(),
        )
    };
    let mut worker = spawn_worker();
    let mut original = task("suspend-source", &environment.digest, "unused");
    original.retain_artifacts = Some(ArtifactRetention {
        execution_checkpoint: None,
        version: ARTIFACT_EXPORT_VERSION,
        trace: true,
        workspace_upper: true,
    });
    original.run.runtime.timeout_ms = Some(45_000);
    let RunInvocation::Process(process) = &mut original.run.invocation;
    process.args = vec!["-c".into(), "set -eu; read -r token < /env/seed; : > /env/seed; printf 'saved-fd\\n' > /env/owned; exec 3<>/env/owned; printf 'ready\\n' > /env/ready; /bin/sleep 15; read -r owned <&3; printf '%s|%s\\n' \"$token\" \"$owned\"; printf 'continued\\n' > /env/after".into()];
    admin.submit(&original).await.unwrap();
    let source_directory = root.join("worker/tasks/suspend-source-1");
    guest_ready(
        &admin,
        "suspend-source",
        &source_directory.join("upper/env/ready"),
        &worker_log,
    )
    .await;
    let cpu_before = measured_cpu(&admin, "suspend-source", 0, false).await;
    let native_pids = native_vm_pids(worker.0.id());
    assert_eq!(native_pids.len(), 1);
    assert_cpu_kernel_evidence(&cpu_before, native_pids[0]);
    let lease = admin.task("suspend-source").await.unwrap().lease.unwrap();
    let suspend = ControlRequest {
        request_id: "save-and-stop".into(),
        action: ControlAction::Suspend,
    };
    admin.control("suspend-source", &suspend).await.unwrap();
    let stopped = finished(&admin, "suspend-source").await;
    assert_eq!(
        stopped.phase,
        TaskPhase::Suspended,
        "{stopped:?}\n{}",
        fs::read_to_string(&worker_log).unwrap()
    );
    let native = stopped.result.as_ref().unwrap();
    assert_eq!(native.state, pvisor_core::RunState::Hibernated);
    assert_terminal_cpu(native, &cpu_before.report.sample);
    assert!(native.failure.is_none());
    assert!(native.exit_code.is_none());
    let receipt = pvisor_core::operation::ExecutionSuspension::from_result(native).unwrap();
    let suspended_download = root.join("suspended-download");
    admin
        .download_artifacts("suspend-source", &suspended_download)
        .await
        .unwrap();
    assert_eq!(
        retained_upper_file(&suspended_download, "upper/env/owned"),
        Some(b"saved-fd\n".to_vec())
    );
    assert!(retained_upper_file(&suspended_download, "upper/env/after").is_none());
    assert!(
        !pvisor::trace::Journal::read(&suspended_download.join("trace"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(receipt.request_id, suspend.request_id);
    assert_eq!(stopped.lease.as_ref().unwrap().key, lease.key);
    let observed = admin.control("suspend-source", &suspend).await.unwrap();
    assert_eq!(observed.phase, ControlPhase::Succeeded);
    assert_eq!(
        observed.outcome,
        Some(ControlOutcome::Checkpointed {
            checkpoint: receipt.checkpoint.clone()
        })
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    assert_eq!(
        pvisor::RunRecord::read(&source_directory).unwrap().state,
        pvisor::RunRecordState::Hibernated
    );
    assert!(
        !source_directory.join("upper/env/after").exists(),
        "source must never thaw after sealing"
    );

    // One slot and one full VM memory budget: only native termination can make
    // this second VM admissible. Merely pausing/offloading cannot pass this gate.
    let mut reused_task = task("capacity-reused", &environment.digest, "reused");
    let RunInvocation::Process(process) = &mut reused_task.run.invocation;
    process.args = vec![
        "-c".into(),
        "set -eu; read -r token < /env/seed; printf 'reused|%s\\n' \"$token\"".into(),
    ];
    admin.submit(&reused_task).await.unwrap();
    let reused = finished(&admin, "capacity-reused").await;
    assert_eq!(reused.phase, TaskPhase::Succeeded, "{reused:?}");
    assert_eq!(
        reused.result.unwrap().output.stdout.as_deref(),
        Some("reused|saved-memory\n")
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    let old_epoch = admin.workers().await.unwrap()[0]
        .registration
        .incarnation
        .clone();
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    fs::remove_file(source_directory.join("upper/env/owned")).unwrap();
    fs::rename(&cache, root.join("detached-cache")).unwrap();
    let _restarted = spawn_worker();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if admin.workers().await.unwrap()[0].registration.incarnation != old_epoch {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("worker restart registration deadline");
    let mut continuation = original.clone();
    continuation.id = "suspend-continuation".into();
    continuation.run.run_id = "suspend-continuation".into();
    continuation.run.parent_run_id = Some(original.run.run_id.clone());
    continuation.restore = Some(ExecutionRestore {
        task_id: original.id,
        request_id: suspend.request_id,
    });
    admin.submit(&continuation).await.unwrap();
    let resumed = finished(&admin, "suspend-continuation").await;
    assert_eq!(
        resumed.phase,
        TaskPhase::Succeeded,
        "{resumed:?}\n{}",
        fs::read_to_string(&worker_log).unwrap()
    );
    assert_eq!(
        resumed.result.as_ref().unwrap().output.stdout.as_deref(),
        Some("saved-memory|saved-fd\n")
    );
    assert_ne!(
        resumed.result.as_ref().unwrap().attempt_id,
        native.attempt_id
    );
    assert_ne!(
        resumed.lease.unwrap().key.incarnation,
        lease.key.incarnation
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    assert!(!source_directory.join("upper/env/after").exists());
    let continuation_download = root.join("continuation-download");
    admin
        .download_artifacts("suspend-continuation", &continuation_download)
        .await
        .unwrap();
    assert_eq!(
        retained_upper_file(&continuation_download, "upper/env/after"),
        Some(b"continued\n".to_vec())
    );
    assert!(
        !pvisor::trace::Journal::read(&continuation_download.join("trace"))
            .unwrap()
            .is_empty()
    );
    let restored_directory = root.join("worker/tasks/suspend-continuation-1");
    let restored = pvisor::RunRecord::read(&restored_directory).unwrap();
    assert_eq!(
        restored.lineage.unwrap().checkpoint_id,
        receipt.checkpoint.snapshot_id
    );
    assert_eq!(
        fs::read(restored.overlay.unwrap().upper.path().join("env/after")).unwrap(),
        b"continued\n"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn parked_vms_suspend_without_cpu_readmission_or_guest_progress_and_restore_state() {
    for action in [ControlAction::Pause, ControlAction::Offload] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cache = root.join("cache");
        let layer = tokio::task::spawn_blocking({
            let source = root.join("source");
            let cache = cache.clone();
            move || {
                publish_layer(
                    &source,
                    &cache,
                    "parked-base",
                    &[("env/seed", "saved-memory\n")],
                    true,
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
        let router =
            pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
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
                base: layer,
                workspace: None,
                toolkits: vec![],
            })
            .await
            .unwrap();
        let mut profile =
            "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n"
                .to_owned();
        if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
            profile += &format!(
                "[vm]\nlibrary_dir = {}\n",
                serde_json::to_string(&directory.to_string_lossy()).unwrap()
            );
        }
        let config = root.join("worker.toml");
        fs::write(&config, profile).unwrap();
        let worker_log = root.join("worker.log");
        let _worker = ChildGuard(
            Command::new(worker_binary(root))
                .args([
                    "--url",
                    &url,
                    "--id",
                    "parked-worker",
                    "--backend",
                    "vm",
                    "--poll-ms",
                    "50",
                    "--slots",
                    "2",
                    "--cpu-millis",
                    "1000",
                    "--memory-bytes",
                    "536870912",
                ])
                .arg("--state")
                .arg(root.join("worker"))
                .arg("--config")
                .arg(&config)
                .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
                .env("PVISOR_CACHE_BACKEND", "filesystem")
                .env("PVISOR_CACHE_LOCATION", &cache)
                .env("XDG_CACHE_HOME", root.join("local-cache"))
                .env("TOKIO_WORKER_THREADS", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
                .spawn()
                .unwrap(),
        );
        let mut original = task("parked-source", &environment.digest, "unused");
        original.retain_bundle = false;
        let RunInvocation::Process(process) = &mut original.run.invocation;
        process.args = vec!["-c".into(), "set -eu; read -r token < /env/seed; : > /env/seed; printf 'saved-fd\\n' > /env/owned; exec 3<>/env/owned; printf 'ready\\n' > /env/ready; while [ ! -e /env/continue ]; do /bin/sleep 0.1; done; read -r owned <&3; printf 'continued\\n' > /env/after; printf '%s|%s\\n' \"$token\" \"$owned\"".into()];
        admin.submit(&original).await.unwrap();
        let source_directory = root.join("worker/tasks/parked-source-1");
        let source_upper = source_directory.join("upper");
        guest_ready(
            &admin,
            &original.id,
            &source_upper.join("env/ready"),
            &worker_log,
        )
        .await;
        controlled(&admin, &original.id, "park", action, &worker_log).await;
        let held = Resources {
            cpu_millis: 0,
            ..original.resources
        };
        assert_eq!(admin.workers().await.unwrap()[0].reserved, held);
        // Only the host can publish this continuation signal. Any hidden
        // resume during save-and-stop would execute /env/after on the source.
        fs::write(source_upper.join("env/continue"), b"go\n").unwrap();
        let mut competitor = task("parked-competitor", &environment.digest, "unused");
        competitor.retain_bundle = false;
        let RunInvocation::Process(process) = &mut competitor.run.invocation;
        process.args = vec!["-c".into(), "set -eu; printf 'ready\\n' > /env/ready; while [ ! -e /env/finish ]; do /bin/sleep 0.1; done".into()];
        admin.submit(&competitor).await.unwrap();
        guest_ready(
            &admin,
            &competitor.id,
            &root.join("worker/tasks/parked-competitor-1/upper/env/ready"),
            &worker_log,
        )
        .await;
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            held.checked_add(competitor.resources).unwrap()
        );
        let sealed = controlled(
            &admin,
            &original.id,
            "suspend",
            ControlAction::Suspend,
            &worker_log,
        )
        .await;
        let Some(ControlOutcome::Checkpointed { checkpoint }) = sealed.outcome else {
            panic!("parked source was not sealed")
        };
        let stopped = finished(&admin, &original.id).await;
        assert_eq!(
            stopped.phase,
            TaskPhase::Suspended,
            "{stopped:?}\n{}",
            fs::read_to_string(&worker_log).unwrap()
        );
        assert_eq!(
            stopped.result.as_ref().unwrap().state,
            pvisor_core::RunState::Hibernated
        );
        assert!(
            !source_upper.join("env/after").exists(),
            "parked source executed after host continuation signal: {action:?}"
        );
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            competitor.resources
        );
        assert_eq!(
            admin.task(&competitor.id).await.unwrap().phase,
            TaskPhase::Running
        );
        let mut continuation = original.clone();
        continuation.id = "parked-restored".into();
        continuation.run.run_id = continuation.id.clone().into();
        continuation.run.parent_run_id = Some(original.run.run_id.clone());
        continuation.restore = Some(ExecutionRestore {
            task_id: original.id.clone(),
            request_id: "suspend".into(),
        });
        admin.submit(&continuation).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            admin.task(&continuation.id).await.unwrap().phase,
            TaskPhase::Queued,
            "restore must reacquire the full CPU budget"
        );
        admin.cancel(&competitor.id).await.unwrap();
        let cancelled = finished(&admin, &competitor.id).await;
        assert_eq!(
            cancelled.phase,
            TaskPhase::Cancelled,
            "{action:?}: {cancelled:?}\n{}",
            fs::read_to_string(&worker_log).unwrap()
        );
        let resumed = finished(&admin, &continuation.id).await;
        assert_eq!(resumed.phase, TaskPhase::Succeeded, "{resumed:?}");
        assert_eq!(
            resumed.result.as_ref().unwrap().output.stdout.as_deref(),
            Some("saved-memory|saved-fd\n")
        );
        assert_ne!(
            resumed.result.as_ref().unwrap().attempt_id,
            stopped.result.as_ref().unwrap().attempt_id
        );
        let restored_upper = pvisor::RunRecord::read(&root.join("worker/tasks/parked-restored-1"))
            .unwrap()
            .overlay
            .unwrap()
            .upper
            .path()
            .to_owned();
        assert!(restored_upper.join("env/after").exists());
        assert!(!source_upper.join("env/after").exists());
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            Resources::default()
        );
        assert_eq!(
            pvisor_core::operation::ExecutionSuspension::from_result(
                stopped.result.as_ref().unwrap()
            )
            .unwrap()
            .checkpoint,
            checkpoint
        );

        // A rejected seal must also leave the parked source stopped. Force a
        // host publication error after native capture, without corrupting any
        // previously sealed object or altering the guest's own filesystem.
        let mut rejected = original.clone();
        rejected.id = "parked-rejected".into();
        rejected.run.run_id = rejected.id.clone().into();
        admin.submit(&rejected).await.unwrap();
        let rejected_directory = root.join("worker/tasks/parked-rejected-1");
        let rejected_upper = rejected_directory.join("upper");
        guest_ready(
            &admin,
            &rejected.id,
            &rejected_upper.join("env/ready"),
            &worker_log,
        )
        .await;
        controlled(&admin, &rejected.id, "park", action, &worker_log).await;
        fs::write(rejected_upper.join("env/continue"), b"go\n").unwrap();
        let pending = rejected_directory.join("execution-snapshots/pending");
        fs::rename(&pending, pending.with_file_name("blocked-pending")).unwrap();
        fs::write(&pending, b"publication blocked").unwrap();
        admin
            .control(
                &rejected.id,
                &ControlRequest {
                    request_id: "rejected-suspend".into(),
                    action: ControlAction::Suspend,
                },
            )
            .await
            .unwrap();
        let rejected = finished(&admin, &rejected.id).await;
        assert_eq!(rejected.phase, TaskPhase::Failed, "{rejected:?}");
        assert!(rejected.result.as_ref().is_some_and(|r| matches!(
            r.state,
            pvisor_core::RunState::Failed | pvisor_core::RunState::Cancelled
        )));
        assert!(
            !rejected_upper.join("env/after").exists(),
            "rejected suspension thawed its source: {action:?}"
        );
        assert!(
            rejected
                .controls
                .iter()
                .all(|c| !matches!(c.outcome, Some(ControlOutcome::Checkpointed { .. })))
        );
        assert_eq!(
            fs::read_dir(rejected_directory.join("execution-snapshots/objects"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            Resources::default()
        );
        server.abort();
    }
}

struct WorkerUnit(String);
impl WorkerUnit {
    fn directory(&self) -> std::path::PathBuf {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                &self.0,
                "--property=ControlGroup",
                "--value",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let path = String::from_utf8(output.stdout).unwrap();
        let path = path.trim();
        assert!(path.starts_with('/') && path.ends_with(&self.0));
        Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'))
    }
    fn assert_membership(&self, node: &ReceivedNodeMemorySample, pids: &[u32]) {
        use pvisor_core::memory::{MemoryLimit, MemoryObservation};
        let MemoryObservation::Measured { usage } = &node.report.sample.cgroup else {
            panic!("dedicated scope probe failed: {node:?}")
        };
        let scope = self.directory();
        assert_eq!(Path::new(&usage.directory), scope);
        assert_eq!(usage.high, MemoryLimit::Bytes(768 * 1024 * 1024));
        assert_eq!(usage.max, MemoryLimit::Bytes(1024 * 1024 * 1024));
        assert_eq!(
            fs::read_to_string(scope.join("memory.swap.max"))
                .unwrap()
                .trim(),
            "0"
        );
        assert_eq!(
            fs::read_to_string(scope.join("memory.oom.group"))
                .unwrap()
                .trim(),
            "1"
        );
        let cpu = fs::read_to_string(scope.join("cpu.max")).unwrap();
        let counters: Vec<u64> = cpu.split_whitespace().map(|s| s.parse().unwrap()).collect();
        assert_eq!(counters.len(), 2);
        assert_eq!(counters[0], counters[1]); // one physical CPU
        let membership = format!(
            "0::/{}\n",
            scope.strip_prefix("/sys/fs/cgroup").unwrap().display()
        );
        for pid in pids {
            assert_eq!(
                fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap(),
                membership
            );
        }
        let all: Vec<u32> = fs::read_to_string(scope.join("cgroup.procs"))
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert!(pids.iter().all(|p| all.contains(p)));
        if pids.len() == 1 {
            assert_eq!(all, pids);
        } else {
            let watchdogs = all
                .iter()
                .filter(|pid| {
                    fs::read(format!("/proc/{pid}/environ"))
                        .unwrap()
                        .split(|b| *b == 0)
                        .any(|v| v.starts_with(b"PVISOR_VM_RESTORE_RAM_WATCHDOG="))
                })
                .count();
            assert_eq!(
                watchdogs, 1,
                "the shared RAM watchdog must belong to the Worker unit"
            );
        }
        let own = fs::read_to_string("/proc/self/cgroup").unwrap();
        assert_ne!(
            own, membership,
            "controller/test harness must remain outside the Worker envelope"
        );
    }
    fn stop(self) {
        let status = Command::new("systemctl")
            .args(["--user", "stop", &self.0])
            .status()
            .unwrap();
        assert!(status.success(), "stop dedicated Worker service");
    }
}
impl Drop for WorkerUnit {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn retained_upper_file(directory: &Path, name: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut archive =
        tar::Archive::new(fs::File::open(directory.join("workspace-upper.tar")).unwrap());
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap() == Path::new(name) {
            let mut bytes = vec![];
            entry.read_to_end(&mut bytes).unwrap();
            return Some(bytes);
        }
    }
    None
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn task(id: &str, environment: &str, value: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "vm-environment-test", "/bin/sh");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    process.args = vec![
        "-c".into(),
        format!(
            "set -eu; test ! -e /env/private; read -r conflict < /env/conflict; read -r base < /env/base-only; read -r workspace < /env/workspace-only; printf '{value}\\n' > /env/modify; printf '{value}\\n' > /env/private; /bin/sleep 2; read -r changed < /env/modify; read -r private < /env/private; test \"$private\" = '{value}'; printf '%s|%s|%s|%s\\n' \"$conflict\" \"$base\" \"$workspace\" \"$changed\""
        ),
    ];
    run.runtime.max_output_bytes = 4096;
    run.runtime.timeout_ms = Some(20_000);
    TaskSpec {
        retain_artifacts: None,
        gateway: None,
        cpu_qos: None,
        restore: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "vm-test".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        },
        resources: Resources {
            slots: 1,
            memory_bytes: 256 * 1024 * 1024,
            cpu_millis: 1000,
        },
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: true,
        environment: Some(environment.into()),
    }
}
async fn finished(client: &Client, id: &str) -> TaskRecord {
    finished_with_log(client, id, None).await
}

async fn finished_with_log(client: &Client, id: &str, worker_log: Option<&Path>) -> TaskRecord {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let task = client.task(id).await.unwrap();
            if task.phase.terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "task completion deadline for {id}: {error}\n{}",
            worker_log
                .and_then(|path| fs::read_to_string(path).ok())
                .unwrap_or_default()
        )
    })
}

async fn guest_ready(client: &Client, id: &str, marker: &Path, worker_log: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fs::read_to_string(marker).is_ok_and(|value| value == "ready\n") {
                break;
            }
            let task = client.task(id).await.unwrap();
            assert!(
                !task.phase.terminal(),
                "guest never became ready: {task:?}\nworker={}",
                fs::read_to_string(worker_log).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("guest readiness deadline");
}

fn control_failure_log(worker_log: &Path) -> String {
    let contents = match fs::read_to_string(worker_log) {
        Ok(contents) => contents,
        Err(error) => return format!("worker log {}: {error}", worker_log.display()),
    };
    // The fixture's TempDir is removed during panic unwinding. Retain just its
    // diagnostic log, without keeping VM state or child processes alive.
    let saved = (|| -> std::io::Result<std::path::PathBuf> {
        let mut file = tempfile::Builder::new()
            .prefix("pvisor-vm-control-failure-")
            .suffix(".log")
            .tempfile()?;
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
        file.keep()
            .map(|(_, path)| path)
            .map_err(|error| error.error)
    })();
    let saved = match saved {
        Ok(path) => path.display().to_string(),
        Err(error) => format!("could not preserve log: {error}"),
    };
    format!(
        "worker log {} (preserved: {saved})\n{contents}",
        worker_log.display()
    )
}

async fn controlled(
    client: &Client,
    id: &str,
    request_id: &str,
    action: ControlAction,
    worker_log: &Path,
) -> ControlRecord {
    let request = ControlRequest {
        request_id: request_id.into(),
        action,
    };
    client.control(id, &request).await.unwrap_or_else(|error| {
        panic!(
            "remote native control {id}/{request_id} rejected: {error}\n{}",
            control_failure_log(worker_log)
        )
    });
    let observed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let task = client.task(id).await.unwrap_or_else(|error| {
                panic!(
                    "remote native control {id}/{request_id} observation failed: {error}\n{}",
                    control_failure_log(worker_log)
                )
            });
            let record = task
                .controls
                .iter()
                .find(|c| c.command.request == request)
                .unwrap_or_else(|| {
                    panic!(
                        "remote native control {id}/{request_id} missing: {task:?}\n{}",
                        control_failure_log(worker_log)
                    )
                });
            if record.phase.terminal() {
                assert_eq!(
                    record.phase,
                    ControlPhase::Succeeded,
                    "{task:?}\n{}",
                    control_failure_log(worker_log)
                );
                break record.clone();
            }
            assert!(
                !task.phase.terminal(),
                "{task:?}\n{}",
                control_failure_log(worker_log)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "remote native control {id}/{request_id} deadline: {error}\n{}",
            control_failure_log(worker_log)
        )
    });
    let replayed = client.control(id, &request).await.unwrap_or_else(|error| {
        panic!(
            "remote native control {id}/{request_id} retry rejected: {error}\n{}",
            control_failure_log(worker_log)
        )
    });
    assert_eq!(
        replayed,
        observed,
        "remote native control {id}/{request_id} retry changed\n{}",
        control_failure_log(worker_log)
    );
    observed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM, FUSE and libkrunfw; run just test-cluster-vm"]
async fn concurrent_vm_environments_preserve_layers_private_writes_and_remote_lifecycle() {
    for device in ["/dev/kvm", "/dev/fuse"] {
        let metadata =
            fs::metadata(device).expect("real VM environment test requires KVM/FUSE devices");
        assert!(metadata.file_type().is_char_device());
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("source");
    let cache = root.join("cache");
    let (base, workspace, toolkit, updated) = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            let base = publish_layer(
                &source,
                &cache,
                "env-base",
                &[
                    ("env/conflict", "base\n"),
                    ("env/base-only", "base-only\n"),
                    ("env/modify", "original\n"),
                ],
                true,
            );
            let workspace = publish_layer(
                &source,
                &cache,
                "env-workspace",
                &[
                    ("env/conflict", "workspace\n"),
                    ("env/workspace-only", "workspace-only\n"),
                ],
                false,
            );
            let toolkit = publish_layer(
                &source,
                &cache,
                "env-toolkit",
                &[("env/conflict", "toolkit\n")],
                false,
            );
            let updated = publish_layer(
                &source,
                &cache,
                "env-toolkit-next",
                &[("env/conflict", "toolkit-next\n")],
                false,
            );
            (base, workspace, toolkit, updated)
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
    let original = admin
        .publish_environment(&EnvironmentTemplate {
            version: CLUSTER_VERSION,
            architecture: "amd64".into(),
            base: base.clone(),
            workspace: Some(workspace.clone()),
            toolkits: vec![toolkit.clone()],
        })
        .await
        .unwrap();
    let next = admin
        .publish_environment(&EnvironmentTemplate {
            toolkits: vec![toolkit.clone(), updated],
            ..original.template.clone()
        })
        .await
        .unwrap();
    let profile = root.join("worker.toml");
    let mut config = "[environments]\nenabled = true\nmax_layers = 8\n".to_owned();
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        config += &format!(
            "\n[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        );
    }
    fs::write(&profile, config).unwrap();
    admin
        .submit(&task("first", &original.digest, "first"))
        .await
        .unwrap();
    admin
        .submit(&task("second", &next.digest, "second"))
        .await
        .unwrap();
    let worker_log = root.join("worker.log");
    let _worker = ChildGuard(
        Command::new(worker_binary(root))
            .args([
                "--url",
                &url,
                "--id",
                "vm-env",
                "--backend",
                "vm",
                "--poll-ms",
                "100",
                "--slots",
                "2",
                "--cpu-millis",
                "2000",
            ])
            .arg("--state")
            .arg(root.join("worker"))
            .arg("--config")
            .arg(&profile)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap(),
    );
    let (first, second) = tokio::join!(
        finished_with_log(&admin, "first", Some(&worker_log)),
        finished_with_log(&admin, "second", Some(&worker_log))
    );
    for (task, expected, environment) in [
        (
            &first,
            "toolkit|base-only|workspace-only|first\n",
            &original,
        ),
        (
            &second,
            "toolkit-next|base-only|workspace-only|second\n",
            &next,
        ),
    ] {
        assert_eq!(
            task.phase,
            TaskPhase::Succeeded,
            "task={task:?}\nworker={}",
            fs::read_to_string(&worker_log).unwrap()
        );
        let result = task.result.as_ref().unwrap();
        assert_eq!(result.state, pvisor_core::RunState::Completed);
        assert_eq!(result.output.stdout.as_deref(), Some(expected));
        let output = root.join(format!("{}-download", task.spec.id));
        admin
            .download_artifacts(&task.spec.id, &output)
            .await
            .unwrap();
        let bytes = fs::read(output.join("run-bundle.json")).unwrap();
        assert_eq!(
            bytes,
            fs::read(root.join(format!("worker/tasks/{}-1/run-bundle.json", task.spec.id)))
                .unwrap()
        );
        let bundle: pvisor::RunBundle = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            bundle.executor_plan.unwrap().isolation,
            IsolationKind::VirtualMachine
        );
        assert!(!bundle.safety.host_process);
        assert_eq!(
            bundle.orchestration["pvisor.orchestration.environment"],
            serde_json::to_value(environment).unwrap()
        );
    }
    assert!(
        first.result.as_ref().unwrap().started_at_unix_ms
            < second.result.as_ref().unwrap().finished_at_unix_ms
    );
    assert!(
        second.result.as_ref().unwrap().started_at_unix_ms
            < first.result.as_ref().unwrap().finished_at_unix_ms
    );
    let base_root = source
        .join("rootfs-v3/sha256")
        .join(&base.manifest_digest[7..]);
    assert_eq!(
        fs::read_to_string(base_root.join("env/modify")).unwrap(),
        "original\n"
    );
    assert!(!base_root.join("env/private").exists());
    admin
        .submit(&task("fresh", &original.digest, "fresh"))
        .await
        .unwrap();
    let fresh = finished_with_log(&admin, "fresh", Some(&worker_log)).await;
    assert_eq!(
        fresh.phase,
        TaskPhase::Succeeded,
        "fresh={fresh:?}\nworker={}",
        fs::read_to_string(&worker_log).unwrap()
    );
    assert_eq!(
        fresh.result.unwrap().output.stdout.as_deref(),
        Some("toolkit|base-only|workspace-only|fresh\n")
    );
    assert_eq!(admin.environment(&original.digest).await.unwrap(), original);

    // A shell variable exists only in the running guest's memory. The marker
    // proves the shell has initialized it before control, and the final output
    // proves we resumed that execution instead of starting the command again.
    let mut live = task("controlled", &original.digest, "unused");
    live.resources.cpu_millis = 2000;
    let RunInvocation::Process(process) = &mut live.run.invocation;
    process.args = vec![
        "-c".into(),
        "set -eu; test ! -e /env/ready; token=before-offload; printf 'ready\\n' > /env/ready; /bin/sleep 4; printf '%s\\n' \"$token\"".into(),
    ];
    admin.submit(&live).await.unwrap();
    guest_ready(
        &admin,
        "controlled",
        &root.join("worker/tasks/controlled-1/upper/env/ready"),
        &worker_log,
    )
    .await;
    let key = admin.task("controlled").await.unwrap().lease.unwrap().key;
    let paused = controlled(
        &admin,
        "controlled",
        "pause",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    assert!(matches!(
        paused.outcome,
        Some(ControlOutcome::Succeeded {
            state: pvisor_core::VmState::Paused,
            memory: None
        })
    ));
    let held = Resources {
        cpu_millis: 0,
        ..live.resources
    };
    assert_eq!(
        admin.task("controlled").await.unwrap().phase,
        TaskPhase::Paused
    );
    assert_eq!(admin.workers().await.unwrap()[0].reserved, held);
    let offloaded = controlled(
        &admin,
        "controlled",
        "offload",
        ControlAction::Offload,
        &worker_log,
    )
    .await;
    let Some(ControlOutcome::Succeeded {
        state: pvisor_core::VmState::Offloaded,
        memory: Some(memory),
    }) = &offloaded.outcome
    else {
        panic!("missing native RAM offload evidence: {offloaded:?}")
    };
    assert!(memory.backed_bytes > 0);
    assert!(fs::metadata(&memory.backing_file).unwrap().len() >= memory.backed_bytes);
    if let (Some(before), Some(after)) = (memory.resident_before_bytes, memory.resident_after_bytes)
    {
        assert!(after <= before, "{memory:?}");
    }
    // More than one lease interval: the offloaded VM must stay owned/renewed,
    // and a residency report must not release its slot or full memory budget.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let dormant = admin.task("controlled").await.unwrap();
    assert_eq!(dormant.phase, TaskPhase::Offloaded);
    assert_eq!(dormant.lease.as_ref().unwrap().key, key);
    assert!(dormant.lease.unwrap().expires_at_ms > pvisor_core::unix_now_ms());
    assert_eq!(admin.workers().await.unwrap()[0].reserved, held);

    let mut competitor = task("competitor", &original.digest, "unused");
    competitor.resources.cpu_millis = 2000;
    competitor.retain_bundle = false;
    let RunInvocation::Process(process) = &mut competitor.run.invocation;
    process.args = vec![
        "-c".into(),
        "set -eu; printf 'ready\\n' > /env/ready; /bin/sleep 15".into(),
    ];
    admin.submit(&competitor).await.unwrap();
    guest_ready(
        &admin,
        "competitor",
        &root.join("worker/tasks/competitor-1/upper/env/ready"),
        &worker_log,
    )
    .await;
    let resume = ControlRequest {
        request_id: "resume".into(),
        action: ControlAction::Resume,
    };
    assert_eq!(
        admin.control("controlled", &resume).await.unwrap().phase,
        ControlPhase::Pending
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pending = admin.task("controlled").await.unwrap();
    assert_eq!(pending.phase, TaskPhase::Offloaded);
    assert_eq!(
        pending.controls.last().unwrap().phase,
        ControlPhase::Pending
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        held.checked_add(competitor.resources).unwrap()
    );
    admin.cancel("competitor").await.unwrap();
    assert_eq!(
        finished_with_log(&admin, "competitor", Some(&worker_log))
            .await
            .phase,
        TaskPhase::Cancelled
    );
    let resumed = controlled(
        &admin,
        "controlled",
        "resume",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    assert!(matches!(
        resumed.outcome,
        Some(ControlOutcome::Succeeded {
            state: pvisor_core::VmState::Running,
            memory: None
        })
    ));
    let complete = finished_with_log(&admin, "controlled", Some(&worker_log)).await;
    assert_eq!(complete.phase, TaskPhase::Succeeded, "{complete:?}");
    assert_eq!(complete.generation, key.generation);
    assert_eq!(complete.controls.len(), 3);
    assert_eq!(
        complete.result.unwrap().output.stdout.as_deref(),
        Some("before-offload\n")
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    admin
        .download_artifacts("controlled", &root.join("controlled-download"))
        .await
        .unwrap();

    // The same dependency protocol must dispatch actual immutable-environment
    // VMs. A blocked successor holds no budget and receives a fresh private
    // upper, rather than inheriting the predecessor's writable workspace.
    let graph = TaskGraphSpec {
        version: CLUSTER_VERSION,
        id: "native-graph".into(),
        tenant: "vm-test".into(),
        nodes: vec![
            TaskGraphNode {
                task: task("graph-first", &original.digest, "graph-first"),
                depends_on: vec![],
            },
            TaskGraphNode {
                task: task("graph-second", &original.digest, "graph-second"),
                depends_on: vec!["graph-first".into()],
            },
        ],
    };
    admin.submit_graph(&graph).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let marker = root.join("worker/tasks/graph-first-1/upper/env/private");
        while !fs::read_to_string(&marker).is_ok_and(|value| value == "graph-first\n") {
            let task = admin.task("graph-first").await.unwrap();
            assert!(
                !task.phase.terminal(),
                "graph VM ended before its private marker: {task:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("graph VM private marker deadline");
    let waiting = admin.task("graph-second").await.unwrap();
    assert_eq!(waiting.phase, TaskPhase::WaitingDependencies);
    assert_eq!(waiting.generation, 0);
    assert!(waiting.lease.is_none());
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        graph.nodes[0].task.resources
    );
    let graph_first = finished_with_log(&admin, "graph-first", Some(&worker_log)).await;
    let graph_second = finished_with_log(&admin, "graph-second", Some(&worker_log)).await;
    assert_eq!(graph_first.phase, TaskPhase::Succeeded, "{graph_first:?}");
    assert_eq!(graph_second.phase, TaskPhase::Succeeded, "{graph_second:?}");
    assert!(graph_second.result.as_ref().unwrap().started_at_unix_ms >= graph_first.updated_at_ms);
    for record in [&graph_first, &graph_second] {
        assert_eq!(record.generation, 1);
        assert!(record.artifacts.is_some());
        assert_eq!(
            record.result.as_ref().unwrap().output.stdout.as_deref(),
            Some(format!("toolkit|base-only|workspace-only|{}\n", record.spec.id).as_str())
        );
        let bundle: pvisor::RunBundle = serde_json::from_slice(
            &fs::read(root.join(format!("worker/tasks/{}-1/run-bundle.json", record.spec.id)))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            bundle.executor_plan.unwrap().isolation,
            IsolationKind::VirtualMachine
        );
        assert!(!bundle.safety.host_process);
        assert_eq!(bundle.run.run_id, record.spec.run.run_id.as_str());
        assert_eq!(
            bundle.orchestration["pvisor.orchestration.environment"],
            serde_json::to_value(&original).unwrap()
        );
    }
    assert_eq!(
        admin.graph("native-graph").await.unwrap().phase,
        TaskGraphPhase::Succeeded
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux user/mount/network namespaces; run just test-cluster-vm"]
async fn rootless_worker_reentry_initializes_namespace_before_tokio_threads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let scheduler = Scheduler::open(&root.join("journal"), SchedulerConfig::default()).unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let mut run = RunSpec::process("rootless", "namespace-test", "/bin/sh");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    process
        .env
        .insert("PVISOR_NATIVE_CPU_QOS_ANCHOR".into(), "1".into());
    process.cwd = Some(root.display().to_string());
    let uid = unsafe { libc::getuid() };
    process.args = vec![
        "-c".into(),
        format!(
            "set -eu; read -r inside outside count < /proc/self/uid_map; test \"$inside\" = '{uid}'; test \"$outside\" = '{uid}'; test \"$count\" = 1; test -z \"${{PVISOR_CLUSTER_WORKER_TOKEN:-}}\"; test \"$PVISOR_NATIVE_CPU_QOS_ANCHOR\" = 1; printf 'rootless-ready\\n'"
        ),
    ];
    run.runtime.max_output_bytes = 1024;
    run.runtime.timeout_ms = Some(10_000);
    admin
        .submit(&TaskSpec {
            retain_artifacts: None,
            gateway: None,
            cpu_qos: None,
            restore: None,
            version: CLUSTER_VERSION,
            id: "rootless".into(),
            tenant: "rootless-test".into(),
            run,
            execution: ExecutionClass {
                executor: ExecutorKind::Process,
                isolation: IsolationKind::RootlessProcess,
            },
            resources: Resources {
                slots: 1,
                memory_bytes: 64 * 1024 * 1024,
                cpu_millis: 1000,
            },
            labels: BTreeMap::new(),
            cache_keys: vec![],
            retain_bundle: true,
            environment: None,
        })
        .await
        .unwrap();
    let worker_log = root.join("worker.log");
    let _worker = ChildGuard(
        Command::new(worker_binary(root))
            .args([
                "--url",
                &url,
                "--id",
                "rootless-node",
                "--backend",
                "rootless",
                "--poll-ms",
                "100",
            ])
            .arg("--state")
            .arg(root.join("worker"))
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap(),
    );
    let task = finished(&admin, "rootless").await;
    assert_eq!(
        task.phase,
        TaskPhase::Succeeded,
        "{task:?}\nworker={}",
        fs::read_to_string(worker_log).unwrap()
    );
    assert_eq!(
        task.result.unwrap().output.stdout.as_deref(),
        Some("rootless-ready\n")
    );
    let output = root.join("download");
    admin.download_artifacts("rootless", &output).await.unwrap();
    let bundle: pvisor::RunBundle =
        serde_json::from_slice(&fs::read(output.join("run-bundle.json")).unwrap()).unwrap();
    assert_eq!(
        bundle.executor_plan.unwrap().isolation,
        IsolationKind::RootlessProcess
    );
    assert!(!bundle.safety.host_process);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn ordinary_vm_job_seals_cpu_ram_and_owned_layers_before_resuming_source() {
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("source");
    let cache = root.join("cache");
    let layer = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "snapshot-base",
                &[
                    ("env/conflict", "snapshot-base\n"),
                    ("env/seed", "memory-before-checkpoint\n"),
                ],
                true,
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
            base: layer,
            workspace: None,
            toolkits: vec![],
        })
        .await
        .unwrap();
    let mut profile =
        "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n".to_owned();
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        profile += &format!(
            "[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        );
    }
    let config = root.join("worker.toml");
    fs::write(&config, profile).unwrap();
    let worker_log = root.join("worker.log");
    let _worker = ChildGuard(
        Command::new(worker_binary(root))
            .args([
                "--url",
                &url,
                "--id",
                "snapshot-worker",
                "--backend",
                "vm",
                "--poll-ms",
                "50",
                "--slots",
                "1",
                "--cpu-millis",
                "1000",
            ])
            .arg("--state")
            .arg(root.join("worker"))
            .arg("--config")
            .arg(&config)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap(),
    );
    let mut snapshot_task = task("snapshot", &environment.digest, "unused");
    snapshot_task.run.runtime.timeout_ms = Some(45_000);
    let RunInvocation::Process(process) = &mut snapshot_task.run.invocation;
    process.args = vec!["-c".into(), "set -eu; read -r token < /env/seed; : > /env/seed; printf 'owned-before-checkpoint\\n' > /env/owned; exec 3<>/env/owned; printf 'ready\\n' > /env/ready; while [ ! -e /env/allow-continue ]; do /bin/sleep 0.2; done; read -r owned <&3; printf '%s|%s\\n' \"$token\" \"$owned\"; printf 'continued\\n' > /env/continued".into()];
    admin.submit(&snapshot_task).await.unwrap();
    guest_ready(
        &admin,
        "snapshot",
        &root.join("worker/tasks/snapshot-1/upper/env/ready"),
        &worker_log,
    )
    .await;
    let before = admin.task("snapshot").await.unwrap();
    let observed = controlled(
        &admin,
        "snapshot",
        "seal-owned-machine",
        ControlAction::Checkpoint,
        &worker_log,
    )
    .await;
    let Some(ControlOutcome::Checkpointed { checkpoint }) = observed.outcome else {
        panic!("no sealed checkpoint")
    };
    checkpoint.validate().unwrap();
    let after = admin.task("snapshot").await.unwrap();
    assert_eq!(after.phase, TaskPhase::Running);
    assert_eq!(
        after.lease.as_ref().unwrap().key,
        before.lease.as_ref().unwrap().key
    );
    assert_eq!(after.current_reservation(), snapshot_task.resources);
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        snapshot_task.resources
    );
    let store = pvisor::environment_snapshot::SnapshotStore::new(&checkpoint.store).unwrap();
    // Verification here runs on the host, outside the VM user namespace.
    let manifest: pvisor::environment_snapshot::EnvironmentManifest = serde_json::from_slice(
        &fs::read(
            checkpoint
                .store
                .join("objects")
                .join(&checkpoint.snapshot_id)
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let binding = manifest.compatibility;
    let published = store
        .open_for_restore(&checkpoint.snapshot_id, &binding)
        .unwrap();
    let machine: serde_json::Value =
        serde_json::from_slice(&published.machine_bytes().unwrap()).unwrap();
    assert_eq!(machine["run_id"], "snapshot");
    assert_eq!(machine["attempt_id"], checkpoint.source_attempt_id);
    for key in [
        pvisor::AGENTCTL_ENDPOINT_ENV,
        pvisor::AGENTCTL_TOKEN_ENV,
        pvisor::AGENTCTL_TRANSPORT_ENV,
        pvisor::AGENTCTL_VERSION_ENV,
    ] {
        assert!(
            !machine["guest"]["env"]
                .as_object()
                .unwrap()
                .contains_key(key)
        );
    }
    assert_eq!(machine["state"]["cpus"].as_array().unwrap().len(), 1);
    assert!(!machine["state"]["ram"].as_array().unwrap().is_empty());
    assert!(!machine["state"]["devices"].as_array().unwrap().is_empty());
    assert!(published.manifest().ram_blocks.is_some());
    assert_native_capture_inodes_transferred(
        &machine,
        &published.manifest().source_root,
        &checkpoint
            .store
            .join("objects")
            .join(&checkpoint.snapshot_id)
            .join("rootfs"),
    );
    let materialized = root.join("independent-copy");
    published.materialize(&materialized).unwrap();
    let upper = Path::new(machine["root"]["upper"].as_str().unwrap());
    let upper_name = upper.file_name().unwrap();
    assert_eq!(
        fs::read(materialized.join(upper_name).join("env/owned")).unwrap(),
        b"owned-before-checkpoint\n"
    );
    assert!(!upper.exists(), "transient capture tree should be gone");
    drop(published);
    // Change only the original Attempt after the freeze/seal transaction is
    // complete. Its copied checkpoint contains no permit and cannot advance
    // the restored guest before recapture/pause/resume assertions complete.
    fs::write(
        root.join("worker/tasks/snapshot-1/upper/env/allow-continue"),
        b"go\n",
    )
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let task = admin.task("snapshot").await.unwrap();
            if task.phase.terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("source execution completion deadline");
    assert_eq!(
        result.phase,
        TaskPhase::Succeeded,
        "{result:?}\n{}",
        fs::read_to_string(worker_log).unwrap()
    );
    assert_eq!(
        result.result.unwrap().output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint\n")
    );
    let published = store
        .open_for_restore(&checkpoint.snapshot_id, &binding)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&published.machine_bytes().unwrap()).unwrap(),
        machine
    );
    drop(published);

    // New execution starts from saved CPU/RAM and the old open descriptor.
    // Re-running argv would fail immediately: its input seed is already empty.
    // The original private file and native cache are unavailable at restore.
    let source_attempt = checkpoint.source_attempt_id.clone();
    fs::remove_file(root.join("worker/tasks/snapshot-1/upper/env/owned")).unwrap();
    fs::rename(&cache, root.join("detached-cache")).unwrap();
    // Recover the entire sealed execution from independently signed remote
    // storage. The source snapshot object is physically removed before import;
    // the controller's acknowledged identity and local ownership gate stay exact.
    let remote = s3_fixture::MockS3::start();
    let remote_client = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name("cache-bucket")
        .with_region("us-east-1")
        .with_access_key_id("AKIATEST")
        .with_secret_access_key("test-secret")
        .with_token("test-token")
        .with_endpoint(&remote.endpoint)
        .with_allow_http(true)
        .build()
        .unwrap();
    let repository = std::sync::Arc::new(
        pvisor::environment_snapshot::SnapshotRepository::object_store(
            std::sync::Arc::new(remote_client),
            "team",
            false,
        )
        .unwrap(),
    );
    let transfer = tokio::task::spawn_blocking({
        let repository = repository.clone();
        let published = store.open(&checkpoint.snapshot_id, &binding).unwrap();
        move || repository.publish(&published).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(transfer.snapshot_id, checkpoint.snapshot_id);
    let old_root = checkpoint
        .store
        .join("objects")
        .join(&checkpoint.snapshot_id)
        .join("rootfs");
    use std::os::unix::fs::MetadataExt;
    // Retain only the empty directory inode after deletion, so inode-number
    // recycling cannot make a freshly imported tree look like the old backing.
    let old_directory = fs::File::open(&old_root).unwrap();
    let old_inode = old_directory.metadata().unwrap().ino();
    store.delete(&checkpoint.snapshot_id).unwrap();
    assert!(!old_root.exists());
    remote
        .read_only
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let remote_puts = remote.puts.load(std::sync::atomic::Ordering::SeqCst);
    tokio::task::spawn_blocking({
        let repository = repository.clone();
        let receipt = transfer.clone();
        let compatibility = binding.clone();
        let original = checkpoint.store.clone();
        let replica = root.join("independent-checkpoint-replica");
        move || {
            let replica = pvisor::environment_snapshot::SnapshotStore::new(&replica).unwrap();
            repository
                .import(&replica, &receipt, &compatibility)
                .unwrap();
            replica.open(&receipt.snapshot_id, &compatibility).unwrap();
            repository
                .import(
                    &pvisor::environment_snapshot::SnapshotStore::new(&original).unwrap(),
                    &receipt,
                    &compatibility,
                )
                .unwrap();
        }
    })
    .await
    .unwrap();
    assert_ne!(fs::metadata(&old_root).unwrap().ino(), old_inode);
    assert_eq!(
        remote.puts.load(std::sync::atomic::Ordering::SeqCst),
        remote_puts
    );
    assert!(remote.gets.load(std::sync::atomic::Ordering::SeqCst) > 0);
    let mut restored_task = snapshot_task.clone();
    restored_task.id = "snapshot-restored".into();
    restored_task.run.run_id = "snapshot-restored".into();
    restored_task.run.parent_run_id = Some(snapshot_task.run.run_id.clone());
    restored_task.restore = Some(ExecutionRestore {
        task_id: "snapshot".into(),
        request_id: "seal-owned-machine".into(),
    });
    admin.submit(&restored_task).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = admin.task("snapshot-restored").await.unwrap();
            if task.phase == TaskPhase::Running {
                break;
            }
            assert!(!task.phase.terminal(), "{task:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("restored Attempt running deadline");
    assert!(
        admin
            .control(
                "snapshot-restored",
                &ControlRequest {
                    request_id: "unsupported-cow-offload".into(),
                    action: ControlAction::Offload,
                }
            )
            .await
            .is_err()
    );
    let recaptured = controlled(
        &admin,
        "snapshot-restored",
        "capture-restored-machine",
        ControlAction::Checkpoint,
        &worker_log,
    )
    .await;
    let Some(ControlOutcome::Checkpointed {
        checkpoint: recaptured,
    }) = recaptured.outcome
    else {
        panic!("restored Attempt did not seal its machine state")
    };
    assert_eq!(recaptured.source_run_id, "snapshot-restored");
    assert_ne!(recaptured.source_attempt_id, checkpoint.source_attempt_id);
    assert_eq!(
        admin
            .task("snapshot-restored")
            .await
            .unwrap()
            .current_reservation(),
        restored_task.resources
    );
    controlled(
        &admin,
        "snapshot-restored",
        "pause-restored",
        ControlAction::Pause,
        &worker_log,
    )
    .await;
    assert_eq!(admin.workers().await.unwrap()[0].reserved.cpu_millis, 0);
    controlled(
        &admin,
        "snapshot-restored",
        "resume-restored",
        ControlAction::Resume,
        &worker_log,
    )
    .await;
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        restored_task.resources
    );
    let restored_directory = root.join("worker/tasks/snapshot-restored-1");
    let restored_record = pvisor::RunRecord::read(&restored_directory).unwrap();
    fs::write(
        restored_record
            .overlay
            .as_ref()
            .unwrap()
            .upper
            .path()
            .join("env/allow-continue"),
        b"go\n",
    )
    .unwrap();
    let restored = finished(&admin, "snapshot-restored").await;
    assert_eq!(
        restored.phase,
        TaskPhase::Succeeded,
        "{restored:?}\n{}",
        fs::read_to_string(&worker_log).unwrap()
    );
    let restored_result = restored.result.as_ref().unwrap();
    assert_ne!(restored_result.attempt_id.as_str(), source_attempt);
    assert_eq!(restored_result.run_id.as_str(), "snapshot-restored");
    assert_eq!(
        restored_result.output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint\n")
    );
    let restored_directory = root.join("worker/tasks/snapshot-restored-1");
    let restored_record = pvisor::RunRecord::read(&restored_directory).unwrap();
    let recaptured_store =
        pvisor::environment_snapshot::SnapshotStore::new(&recaptured.store).unwrap();
    let recaptured_state = recaptured_store
        .open_for_restore(&recaptured.snapshot_id, &binding)
        .unwrap();
    let recaptured_machine: serde_json::Value =
        serde_json::from_slice(&recaptured_state.machine_bytes().unwrap()).unwrap();
    assert_eq!(recaptured_machine["run_id"], "snapshot-restored");
    assert_incremental_ram(
        &recaptured_state,
        manifest.ram_blocks.as_ref().unwrap(),
        &manifest.ram_sha256,
    );
    assert_eq!(
        recaptured_machine["attempt_id"],
        restored_result.attempt_id.as_str()
    );
    drop(recaptured_state);
    assert_eq!(
        restored_record.lineage.as_ref().unwrap().checkpoint_id,
        checkpoint.snapshot_id
    );
    let restored_upper = restored_record.overlay.as_ref().unwrap().upper.path();
    assert!(restored_upper.starts_with(&restored_directory));
    assert_eq!(fs::read(restored_upper.join("env/seed")).unwrap(), b"");
    assert_eq!(
        fs::read(restored_upper.join("env/continued")).unwrap(),
        b"continued\n"
    );
    let final_snapshot = store
        .open_for_restore(&checkpoint.snapshot_id, &binding)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&final_snapshot.machine_bytes().unwrap())
            .unwrap(),
        machine
    );
    drop(final_snapshot);
    fs::rename(root.join("detached-cache"), &cache).unwrap();

    // A receipt failure after object publication is uncertain. The supervisor
    // must terminate the source rather than resume it and allow a second save
    // under the same request id at another execution point.
    let mut failing_task = task("snapshot-receipt-failure", &environment.digest, "unused");
    failing_task.run.runtime.timeout_ms = Some(45_000);
    let RunInvocation::Process(process) = &mut failing_task.run.invocation;
    process.args = vec![
        "-c".into(),
        "set -eu; printf 'ready\\n' > /env/ready; /bin/sleep 25; printf 'continued\\n' > /env/continued".into(),
    ];
    admin.submit(&failing_task).await.unwrap();
    let failing_directory = root.join("worker/tasks/snapshot-receipt-failure-1");
    guest_ready(
        &admin,
        "snapshot-receipt-failure",
        &failing_directory.join("upper/env/ready"),
        &worker_log,
    )
    .await;
    let failing_store = failing_directory.join("execution-snapshots");
    assert!(failing_store.join("objects").is_dir());
    assert_eq!(
        fs::read_dir(failing_store.join("objects")).unwrap().count(),
        0
    );
    // Lookup of an absent receipt must remain possible; only the subsequent
    // write is denied. A non-directory would fail before native capture.
    use std::os::unix::fs::PermissionsExt;
    let receipt_directory = failing_store.join("requests");
    fs::create_dir(&receipt_directory).unwrap();
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        fs::File::create(receipt_directory.join("permission-probe")).is_err(),
        "receipt fault injection requires an unprivileged host owner"
    );
    let failed_request = ControlRequest {
        request_id: "unrecorded-seal".into(),
        action: ControlAction::Checkpoint,
    };
    admin
        .control("snapshot-receipt-failure", &failed_request)
        .await
        .unwrap();
    let failed_task = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let task = admin.task("snapshot-receipt-failure").await.unwrap();
            if task.phase.terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "uncertain publication must terminate the source: {error}\n{}",
            fs::read_to_string(&worker_log).unwrap()
        )
    });
    assert!(
        matches!(failed_task.phase, TaskPhase::Failed | TaskPhase::Cancelled)
            && failed_task.result.is_some(),
        "uncertain capture must report native termination, not an unknown lease loss: {failed_task:?}"
    );
    assert_eq!(
        fs::read_dir(failing_store.join("objects")).unwrap().count(),
        1
    );
    assert!(receipt_directory.is_dir());
    assert_eq!(fs::read_dir(&receipt_directory).unwrap().count(), 0);
    fs::set_permissions(&receipt_directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!failing_directory.join("upper/env/continued").exists());
    assert_eq!(admin.workers().await.unwrap()[0].reserved.slots, 0);
    assert!(
        failed_task.controls.iter().any(|record| {
            record.command.request == failed_request && record.phase != ControlPhase::Succeeded
        }),
        "{failed_task:?}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn live_capture_forks_running_vm_without_stopping_source_and_preserves_private_state() {
    live_capture_fork(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux core scheduling, KVM/FUSE and libkrunfw; run just test-cluster-vm"]
async fn cpu_qos_classes_apply_to_all_vm_threads_and_survive_live_fork_and_resume() {
    prove_cpu_anchor_lifetime();
    live_capture_fork(true).await;
}

async fn live_capture_fork(cpu_qos: bool) {
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("source");
    let cache = root.join("cache");
    let layer = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "snapshot-base",
                &[
                    ("env/conflict", "snapshot-base\n"),
                    ("env/seed", "memory-before-checkpoint\n"),
                ],
                true,
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
    let arm_cpu_delay = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delayed_cpu = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cpu_inflight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut router =
        pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
    if cpu_qos {
        let armed = arm_cpu_delay.clone();
        let delayed = delayed_cpu.clone();
        let inflight = cpu_inflight.clone();
        router = router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let armed = armed.clone();
                let delayed = delayed.clone();
                let inflight = inflight.clone();
                async move {
                    if request.uri().path() == "/v1/workers/cpu"
                        && armed.load(std::sync::atomic::Ordering::SeqCst)
                        && !delayed.swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        inflight.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _guard = CpuUploadGuard(inflight);
                        tokio::time::sleep(Duration::from_secs(6)).await;
                    }
                    next.run(request).await
                }
            },
        ));
    }
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
            base: layer,
            workspace: None,
            toolkits: vec![],
        })
        .await
        .unwrap();
    let mut profile =
        "[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n".to_owned();
    if cpu_qos {
        profile +=
            "[cpu_qos]\nenabled = true\n[cpu_sampling]\nenabled = true\ninterval_ms = 1000\n";
    }
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR") {
        profile += &format!(
            "[vm]\nlibrary_dir = {}\n",
            serde_json::to_string(&directory.to_string_lossy()).unwrap()
        );
    }
    let config = root.join("worker.toml");
    fs::write(&config, profile).unwrap();
    let worker_log = root.join("worker.log");
    let start_worker = || {
        Command::new(worker_binary(root))
            .args([
                "--url",
                &url,
                "--id",
                "live-fork-worker",
                "--backend",
                "vm",
                "--poll-ms",
                "50",
                "--slots",
                if cpu_qos { "4" } else { "3" },
                "--cpu-millis",
                if cpu_qos { "4000" } else { "3000" },
            ])
            .arg("--state")
            .arg(root.join("worker"))
            .arg("--config")
            .arg(&config)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("TOKIO_WORKER_THREADS", "1")
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap()
    };
    let mut _worker = ChildGuard(start_worker());
    let mut source_task = task("live-source", &environment.digest, "unused");
    source_task.run.runtime.timeout_ms = Some(90_000);
    if cpu_qos {
        source_task.cpu_qos = Some(pvisor_core::CpuQosClass::LatencySensitive);
    }
    let RunInvocation::Process(process) = &mut source_task.run.invocation;
    process.args = vec!["-c".into(), "set -eu; read -r token < /env/seed; : > /env/seed; printf 'owned-before-checkpoint\\n' > /env/owned; exec 3<>/env/owned; printf 'ready\\n' > /env/ready; while [ ! -e /env/allow-continue ]; do /bin/sleep 0.2; done; read -r owned <&3; read -r branch < /env/allow-continue; printf '%s|%s|%s\\n' \"$token\" \"$owned\" \"$branch\"; printf '%s\\n' \"$branch\" > /env/private".into()];
    admin.submit(&source_task).await.unwrap();
    let source_upper = root.join("worker/tasks/live-source-1/upper");
    guest_ready(
        &admin,
        "live-source",
        &source_upper.join("env/ready"),
        &worker_log,
    )
    .await;
    let mut be_pid = None;
    let mut ls_cookie = None;
    let mut cpu_evidence = std::collections::BTreeMap::new();
    let parent_cookie = if cpu_qos {
        cpu_cookie(_worker.0.id())
    } else {
        0
    };
    if cpu_qos {
        let pids = native_vm_pids(_worker.0.id());
        assert_eq!(pids.len(), 1);
        let source_pid = pids[0];
        let cookie = cpu_cookie(source_pid);
        assert_ne!(cookie, 0);
        assert_ne!(cookie, parent_cookie);
        assert_vm_cpu_class(
            source_pid,
            pvisor_core::CpuQosClass::LatencySensitive,
            cookie,
        )
        .await;
        ls_cookie = Some(cookie);
        arm_cpu_delay.store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !cpu_inflight.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("CPU upload must enter the deliberately slow endpoint");
        let before = admin.task("live-source").await.unwrap().lease.unwrap().key;
        controlled(
            &admin,
            "live-source",
            "cpu-upload-pause",
            ControlAction::Pause,
            &worker_log,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(3100)).await;
        assert!(
            cpu_inflight.load(std::sync::atomic::Ordering::SeqCst),
            "CPU upload must remain in flight beyond the three-second lease interval"
        );
        assert_eq!(
            admin
                .task("live-source")
                .await
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .key,
            before
        );
        controlled(
            &admin,
            "live-source",
            "cpu-upload-resume",
            ControlAction::Resume,
            &worker_log,
        )
        .await;
        assert_eq!(
            admin.task("live-source").await.unwrap().lease.unwrap().key,
            before,
            "slow CPU uploads cannot expire or replace the native lease"
        );
        let measured = measured_cpu(&admin, "live-source", 0, false).await;
        assert_cpu_kernel_evidence(&measured, source_pid);
        cpu_evidence.insert("live-source".to_string(), measured.report.sample.clone());
        assert_eq!(
            admin.workers().await.unwrap()[0]
                .registration
                .cpu_observation_protocol,
            Some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION)
        );
        let mut be = source_task.clone();
        be.id = "qos-be".into();
        be.run.run_id = "qos-be".into();
        be.cpu_qos = Some(pvisor_core::CpuQosClass::BestEffort);
        let RunInvocation::Process(process) = &mut be.run.invocation;
        process.args[1] = process.args[1].replace("/bin/sleep 0.2", ":");
        admin.submit(&be).await.unwrap();
        guest_ready(
            &admin,
            "qos-be",
            &root.join("worker/tasks/qos-be-1/upper/env/ready"),
            &worker_log,
        )
        .await;
        let pids = native_vm_pids(_worker.0.id());
        assert_eq!(pids.len(), 2);
        let pid = *pids.iter().find(|pid| **pid != source_pid).unwrap();
        assert_vm_cpu_class(pid, pvisor_core::CpuQosClass::BestEffort, parent_cookie).await;
        be_pid = Some(pid);
        let first = measured_cpu(&admin, "qos-be", 0, false).await;
        assert_cpu_kernel_evidence(&first, pid);
        let next = measured_cpu(&admin, "qos-be", first.report.sequence, true).await;
        assert_cpu_kernel_evidence(&next, pid);
        cpu_evidence.insert("qos-be".to_string(), next.report.sample.clone());
        assert!(
            next.report
                .sample
                .usage
                .as_ref()
                .unwrap()
                .total_ticks()
                .unwrap()
                > first
                    .report
                    .sample
                    .usage
                    .as_ref()
                    .unwrap()
                    .total_ticks()
                    .unwrap()
        );
        assert!(delayed_cpu.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            unsafe { libc::sched_getscheduler(_worker.0.id() as i32) },
            libc::SCHED_OTHER
        );
        assert_eq!(
            admin.workers().await.unwrap()[0]
                .registration
                .cpu_qos_classes,
            vec![
                pvisor_core::CpuQosClass::BestEffort,
                pvisor_core::CpuQosClass::LatencySensitive
            ]
        );
    }
    let source_key = admin.task("live-source").await.unwrap().lease.unwrap().key;
    let request = ExecutionForkRequest {
        version: CLUSTER_VERSION,
        request_id: "live-pair".into(),
        checkpoint_request_id: "capture-live-pair".into(),
        branches: vec![
            ExecutionForkBranch {
                task_id: "live-left".into(),
                run_id: "live-left".into(),
            },
            ExecutionForkBranch {
                task_id: "live-right".into(),
                run_id: "live-right".into(),
            },
        ],
    };
    for path in [
        "/v1/tasks/live-source/live-forks",
        "/v1/tasks/live-source/live-forks/live-pair",
    ] {
        let http = reqwest::Client::new();
        let response = if path.ends_with("live-pair") {
            http.get(format!("{url}{path}"))
                .bearer_auth(WORKER)
                .send()
                .await
                .unwrap()
        } else {
            http.post(format!("{url}{path}"))
                .bearer_auth(WORKER)
                .json(&request)
                .send()
                .await
                .unwrap()
        };
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let pending = admin
        .request_live_fork("live-source", &request)
        .await
        .unwrap();
    assert_eq!(pending.phase, LiveForkPhase::Capturing);
    assert_eq!(pending.source_key, source_key);
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let record = admin.live_fork("live-source", "live-pair").await.unwrap();
            if record.phase != LiveForkPhase::Capturing {
                assert_eq!(record.phase, LiveForkPhase::Ready, "{record:?}");
                break record;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "live capture deadline: {e}\n{}",
            fs::read_to_string(&worker_log).unwrap()
        )
    });
    assert_eq!(
        admin
            .request_live_fork("live-source", &request)
            .await
            .unwrap(),
        ready
    );
    assert_eq!(
        admin
            .execution_fork("live-source", "live-pair")
            .await
            .unwrap(),
        ready.fork.clone().unwrap()
    );
    let checkpoint = ready.fork.as_ref().unwrap().checkpoint.clone();
    let source_record = admin.task("live-source").await.unwrap();
    assert_eq!(source_record.phase, TaskPhase::Running);
    assert_eq!(source_record.lease.as_ref().unwrap().key, source_key);
    assert_eq!(source_record.current_reservation(), source_task.resources);
    let mut branch_uppers = Vec::new();
    for branch in &request.branches {
        let directory = root.join(format!("worker/tasks/{}-1", branch.task_id));
        // Controller Running precedes native initialization. The inherited ready
        // file is inside the restored private upper only after materialization.
        let upper = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let task = admin.task(&branch.task_id).await.unwrap();
                assert!(
                    !task.phase.terminal(),
                    "{task:?}\n{}",
                    fs::read_to_string(&worker_log).unwrap()
                );
                if let Ok(record) = pvisor::RunRecord::read(&directory)
                    && let Some(overlay) = record.overlay
                {
                    let upper = overlay.upper.path().to_path_buf();
                    if upper.join("env/ready").exists() {
                        assert_eq!(task.current_reservation(), source_task.resources);
                        assert_eq!(
                            record.lineage.unwrap().checkpoint_id,
                            checkpoint.snapshot_id
                        );
                        break upper;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|e| {
            panic!(
                "branch restore deadline: {e}\n{}",
                fs::read_to_string(&worker_log).unwrap()
            )
        });
        assert_ne!(upper, source_upper);
        assert_eq!(fs::read(upper.join("env/seed")).unwrap(), b"");
        branch_uppers.push(upper);
    }
    assert_ne!(branch_uppers[0], branch_uppers[1]);
    let [left_pid, right_pid] = restored_vm_pids(
        _worker.0.id(),
        &checkpoint.store,
        &worker_log,
        &admin,
        ["live-left", "live-right"],
    )
    .await;
    assert_ne!(left_pid, right_pid);
    if let Some(cookie) = ls_cookie {
        for pid in [left_pid, right_pid] {
            assert_vm_cpu_class(pid, pvisor_core::CpuQosClass::LatencySensitive, cookie).await;
        }
        assert_vm_cpu_class(
            be_pid.unwrap(),
            pvisor_core::CpuQosClass::BestEffort,
            parent_cookie,
        )
        .await;
        assert_eq!(cpu_cookie(_worker.0.id()), parent_cookie);
        let mut observed_pids = std::collections::BTreeSet::new();
        for branch in &request.branches {
            let sample = measured_cpu(&admin, &branch.task_id, 0, false).await;
            let pid = sample.report.sample.usage.as_ref().unwrap().pid;
            assert!([left_pid, right_pid].contains(&pid));
            assert!(
                observed_pids.insert(pid),
                "each fork must own a distinct native CPU process"
            );
            assert_cpu_kernel_evidence(&sample, pid);
            cpu_evidence.insert(branch.task_id.clone(), sample.report.sample.clone());
            assert_ne!(
                sample.report.sample.attempt_id,
                checkpoint.source_attempt_id.as_str().into()
            );
            assert_eq!(
                admin.task(&branch.task_id).await.unwrap().spec.cpu_qos,
                Some(pvisor_core::CpuQosClass::LatencySensitive)
            );
        }
    }
    assert_eq!(
        snapshot_ram_usage(left_pid).identities,
        snapshot_ram_usage(right_pid).identities
    );
    for branch in &request.branches {
        controlled(
            &admin,
            &branch.task_id,
            "prove-native-pause",
            ControlAction::Pause,
            &worker_log,
        )
        .await;
        controlled(
            &admin,
            &branch.task_id,
            "prove-native-resume",
            ControlAction::Resume,
            &worker_log,
        )
        .await;
    }
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources {
            slots: if cpu_qos { 4 } else { 3 },
            cpu_millis: if cpu_qos { 4000 } else { 3000 },
            memory_bytes: (if cpu_qos { 4 } else { 3 }) * source_task.resources.memory_bytes,
        }
    );
    if let Some(cookie) = ls_cookie {
        for pid in [left_pid, right_pid] {
            assert_vm_cpu_class(pid, pvisor_core::CpuQosClass::LatencySensitive, cookie).await;
        }
        let machine: serde_json::Value = serde_json::from_slice(
            &fs::read(
                checkpoint
                    .store
                    .join("objects")
                    .join(&checkpoint.snapshot_id)
                    .join("machine.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(machine["cpu_qos"], "latency_sensitive");
        fs::write(
            root.join("worker/tasks/qos-be-1/upper/env/allow-continue"),
            b"be\n",
        )
        .unwrap();
        let be = finished_with_log(&admin, "qos-be", Some(&worker_log)).await;
        assert_eq!(be.phase, TaskPhase::Succeeded, "{be:?}");
        assert_terminal_cpu(be.result.as_ref().unwrap(), &cpu_evidence["qos-be"]);
        assert_eq!(
            be.result.unwrap().executor_observations.cpu_qos,
            Some(pvisor_core::CpuQosObservation {
                class: pvisor_core::CpuQosClass::BestEffort,
                scheduler_policy: libc::SCHED_IDLE,
                core_cookie: Some(parent_cookie)
            })
        );
    }
    // Source resumes normally after capture while both children remain held at
    // the saved shell loop. Each continuation changes only its own private tree.
    fs::write(source_upper.join("env/allow-continue"), b"source\n").unwrap();
    let source = finished_with_log(&admin, "live-source", Some(&worker_log)).await;
    assert_eq!(source.phase, TaskPhase::Succeeded, "{source:?}");
    if let Some(cookie) = ls_cookie {
        assert_terminal_cpu(
            source.result.as_ref().unwrap(),
            &cpu_evidence["live-source"],
        );
        assert_eq!(
            source
                .result
                .as_ref()
                .unwrap()
                .executor_observations
                .cpu_qos,
            Some(pvisor_core::CpuQosObservation {
                class: pvisor_core::CpuQosClass::LatencySensitive,
                scheduler_policy: libc::SCHED_OTHER,
                core_cookie: Some(cookie)
            })
        );
    }
    assert_eq!(
        source.result.unwrap().output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint|source\n")
    );
    for upper in &branch_uppers {
        assert!(!upper.join("env/private").exists());
        assert!(!upper.join("env/allow-continue").exists());
    }
    fs::write(branch_uppers[0].join("env/allow-continue"), b"left\n").unwrap();
    let left = finished_with_log(&admin, "live-left", Some(&worker_log)).await;
    assert_eq!(left.phase, TaskPhase::Succeeded, "{left:?}");
    assert!(!branch_uppers[1].join("env/private").exists());
    fs::write(branch_uppers[1].join("env/allow-continue"), b"right\n").unwrap();
    let right = finished_with_log(&admin, "live-right", Some(&worker_log)).await;
    assert_eq!(right.phase, TaskPhase::Succeeded, "{right:?}");
    for (task, value, upper) in [
        (left, "left", &branch_uppers[0]),
        (right, "right", &branch_uppers[1]),
    ] {
        let result = task.result.unwrap();
        if let Some(cookie) = ls_cookie {
            assert_terminal_cpu(&result, &cpu_evidence[result.run_id.as_str()]);
            assert_eq!(
                result.executor_observations.cpu_qos,
                Some(pvisor_core::CpuQosObservation {
                    class: pvisor_core::CpuQosClass::LatencySensitive,
                    scheduler_policy: libc::SCHED_OTHER,
                    core_cookie: Some(cookie)
                })
            );
        }
        assert_ne!(result.attempt_id.as_str(), checkpoint.source_attempt_id);
        assert_eq!(
            result.output.stdout.unwrap(),
            format!("memory-before-checkpoint|owned-before-checkpoint|{value}\n")
        );
        assert_eq!(
            fs::read_to_string(upper.join("env/private")).unwrap(),
            format!("{value}\n")
        );
    }
    assert_eq!(
        fs::read(source_upper.join("env/private")).unwrap(),
        b"source\n"
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    assert_eq!(
        admin
            .request_live_fork("live-source", &request)
            .await
            .unwrap(),
        ready
    );
    if cpu_qos {
        // Exercise executor cancellation and watchdog publication, rather than
        // inferring their terminal observations from a normal guest exit.
        for (id, deadline) in [("qos-cancelled", false), ("qos-deadline", true)] {
            let mut task = admin.task("qos-be").await.unwrap().spec;
            task.id = id.into();
            task.run.run_id = id.into();
            task.run.runtime.timeout_ms = Some(if deadline { 6000 } else { 90_000 });
            task.run.runtime.termination_grace_ms = 50;
            admin.submit(&task).await.unwrap();
            guest_ready(
                &admin,
                id,
                &root.join(format!("worker/tasks/{id}-1/upper/env/ready")),
                &worker_log,
            )
            .await;
            let sample = measured_cpu(&admin, id, 0, true).await;
            let pids = native_vm_pids(_worker.0.id());
            assert_eq!(pids.len(), 1);
            assert_cpu_kernel_evidence(&sample, pids[0]);
            if !deadline {
                admin.cancel(id).await.unwrap();
            }
            let terminal = finished_with_log(&admin, id, Some(&worker_log)).await;
            assert_eq!(
                terminal.phase,
                if deadline {
                    TaskPhase::Failed
                } else {
                    TaskPhase::Cancelled
                },
                "{terminal:?}"
            );
            let result = terminal.result.as_ref().unwrap();
            assert_eq!(
                result.state,
                if deadline {
                    pvisor_core::RunState::Failed
                } else {
                    pvisor_core::RunState::Cancelled
                }
            );
            if deadline {
                assert_eq!(
                    result.failure.as_ref().unwrap().kind,
                    pvisor_core::RunFailureKind::DeadlineExceeded
                );
            }
            assert_terminal_cpu(result, &sample.report.sample);
            assert_eq!(
                admin.workers().await.unwrap()[0].reserved,
                Resources::default()
            );
        }
        for id in [
            "live-source",
            "qos-be",
            "live-left",
            "live-right",
            "qos-cancelled",
            "qos-deadline",
        ] {
            let task = admin.task(id).await.unwrap();
            assert!(task.cpu_sample.is_none());
            let bundle: pvisor::RunBundle = serde_json::from_slice(
                &fs::read(root.join(format!("worker/tasks/{id}-1/run-bundle.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(
                bundle.executor_observations.cpu_usage,
                task.result.unwrap().executor_observations.cpu_usage,
                "Run Bundle must carry the same final native counters as completion"
            );
        }
        assert!(
            fs::read_to_string(&worker_log)
                .unwrap()
                .contains("CPU observation delivery failed"),
            "the deliberately slow CPU upload must exceed Client timeout"
        );
        let incarnation = admin.workers().await.unwrap()[0]
            .registration
            .incarnation
            .clone();
        _worker.0.kill().unwrap();
        _worker.0.wait().unwrap();
        _worker = ChildGuard(start_worker());
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if admin.workers().await.unwrap()[0].registration.incarnation != incarnation {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("CPU QoS Worker must register after cold restart");
        let owners = worker_children(_worker.0.id());
        assert_eq!(owners.len(), 1, "restarted idle Worker owns one LS anchor");
        let cookie = cpu_cookie(*owners.iter().next().unwrap());
        assert_ne!(cookie, 0);
        assert_ne!(cookie, cpu_cookie(_worker.0.id()));
        let mut cold = source_task.clone();
        cold.id = "qos-cold".into();
        cold.run.run_id = "qos-cold".into();
        cold.run.parent_run_id = Some(source_task.run.run_id.clone());
        cold.restore = Some(ExecutionRestore {
            task_id: source_task.id.clone(),
            request_id: request.checkpoint_request_id.clone(),
        });
        admin.submit(&cold).await.unwrap();
        let directory = root.join("worker/tasks/qos-cold-1");
        let upper = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let task = admin.task("qos-cold").await.unwrap();
                assert!(
                    !task.phase.terminal(),
                    "{task:?}\n{}",
                    fs::read_to_string(&worker_log).unwrap()
                );
                if let Ok(record) = pvisor::RunRecord::read(&directory)
                    && let Some(overlay) = record.overlay
                    && overlay.upper.path().join("env/ready").exists()
                {
                    break overlay.upper.path().to_path_buf();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("cold CPU QoS restore readiness");
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let restored = worker_children(_worker.0.id());
                if let Some(pid) = restored.difference(&owners).copied().find(|pid| {
                    fs::read_to_string(format!("/proc/{pid}/maps")).is_ok_and(|maps| {
                        maps.lines().any(|line| {
                            line.contains(
                                &checkpoint
                                    .store
                                    .join("ram-mounts")
                                    .to_string_lossy()
                                    .to_string(),
                            ) && line.ends_with("/ram")
                        })
                    })
                }) {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("cold restored native RAM owner PID");
        assert_vm_cpu_class(pid, pvisor_core::CpuQosClass::LatencySensitive, cookie).await;
        let sample = measured_cpu(&admin, "qos-cold", 0, false).await;
        assert_cpu_kernel_evidence(&sample, pid);
        assert_ne!(
            sample.report.sample.attempt_id,
            checkpoint.source_attempt_id.as_str().into()
        );
        assert_eq!(
            admin.task("qos-cold").await.unwrap().current_reservation(),
            source_task.resources
        );
        fs::write(upper.join("env/allow-continue"), b"cold\n").unwrap();
        let cold = finished_with_log(&admin, "qos-cold", Some(&worker_log)).await;
        assert_eq!(cold.phase, TaskPhase::Succeeded, "{cold:?}");
        let result = cold.result.unwrap();
        assert_terminal_cpu(&result, &sample.report.sample);
        assert_eq!(
            result.output.stdout.as_deref(),
            Some("memory-before-checkpoint|owned-before-checkpoint|cold\n")
        );
        assert_eq!(
            result.executor_observations.cpu_qos,
            Some(pvisor_core::CpuQosObservation {
                class: pvisor_core::CpuQosClass::LatencySensitive,
                scheduler_policy: libc::SCHED_OTHER,
                core_cookie: Some(cookie)
            })
        );
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved,
            Resources::default()
        );
    }
    server.abort();
}

fn cpu_cookie(pid: u32) -> u64 {
    let mut cookie = 0_u64;
    assert_eq!(
        unsafe {
            libc::prctl(
                62,
                0 as libc::c_ulong,
                libc::c_ulong::from(pid),
                0 as libc::c_ulong,
                &mut cookie as *mut u64,
            )
        },
        0,
        "read actual core cookie for {pid}: {}",
        std::io::Error::last_os_error()
    );
    cookie
}

fn worker_children(pid: u32) -> std::collections::BTreeSet<u32> {
    let mut children = std::collections::BTreeSet::new();
    for thread in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        if let Ok(ids) = fs::read_to_string(thread.unwrap().path().join("children")) {
            children.extend(ids.split_whitespace().map(|id| id.parse::<u32>().unwrap()));
        }
    }
    children
}

fn native_vm_pids(worker: u32) -> Vec<u32> {
    worker_children(worker)
        .into_iter()
        .filter(|pid| maps_libkrunfw(*pid))
        .collect()
}

async fn assert_vm_cpu_class(pid: u32, class: pvisor_core::CpuQosClass, cookie: u64) {
    // Restored upper/RAM mappings exist before native vCPU threads are started.
    // Wait for the actual CPU owner rather than treating storage readiness as VM readiness.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let threads: Vec<_> = fs::read_dir(format!("/proc/{pid}/task"))
                .unwrap()
                .map(|thread| thread.unwrap().path())
                .collect();
            if threads.iter().any(|thread| {
                fs::read_to_string(thread.join("comm"))
                    .unwrap_or_default()
                    .contains("vcpu")
            }) {
                assert!(
                    threads.len() >= 2,
                    "must inspect VMM and vCPU threads for {pid}"
                );
                for thread in threads {
                    let tid: u32 = thread
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .parse()
                        .unwrap();
                    assert_eq!(
                        unsafe { libc::sched_getscheduler(tid as i32) },
                        if class == pvisor_core::CpuQosClass::BestEffort {
                            libc::SCHED_IDLE
                        } else {
                            libc::SCHED_OTHER
                        },
                        "actual scheduler policy for VM {pid} thread {tid}"
                    );
                    assert_eq!(
                        cpu_cookie(tid),
                        cookie,
                        "actual cookie for VM {pid} thread {tid}"
                    );
                }
                eprintln!("CPU QoS verified native PID {pid}, class {class:?}, cookie {cookie}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("VMM/vCPU thread readiness for PID {pid}: {error}"));
}

fn prove_cpu_anchor_lifetime() {
    let parent = std::process::id();
    let original = cpu_cookie(parent);
    let policy = unsafe { libc::sched_getscheduler(0) };
    let before = worker_children(parent);
    let group =
        pvisor::CpuQosGroup::with_launcher(Path::new(env!("CARGO_BIN_EXE_pvisor-worker"))).unwrap();
    let after = worker_children(parent);
    let owners: Vec<_> = after.difference(&before).copied().collect();
    assert_eq!(owners.len(), 1);
    let owner = owners[0];
    assert_ne!(cpu_cookie(owner), 0);
    assert_ne!(cpu_cookie(owner), original);
    assert_eq!(cpu_cookie(parent), original);
    assert_eq!(unsafe { libc::sched_getscheduler(0) }, policy);
    let environment = fs::read(format!("/proc/{owner}/environ")).unwrap();
    assert_eq!(environment, b"PVISOR_NATIVE_CPU_QOS_ANCHOR=1\0");
    let retained = group.clone();
    drop(group);
    assert!(Path::new(&format!("/proc/{owner}")).exists());
    drop(retained);
    assert!(
        !Path::new(&format!("/proc/{owner}")).exists(),
        "last owner must reap the anchor"
    );
}

fn assert_terminal_cpu(result: &pvisor_core::RunResult, previous: &pvisor_core::cpu::RunCpuSample) {
    assert_eq!(result.run_id, previous.run_id);
    assert_eq!(result.attempt_id, previous.attempt_id);
    let Some(pvisor_core::cpu::TerminalCpuUsage::Measured { usage }) =
        &result.executor_observations.cpu_usage
    else {
        panic!(
            "expected final native CPU counters: {:?}",
            result.executor_observations
        );
    };
    usage
        .interval_since(previous.usage.as_ref().unwrap())
        .unwrap();
    assert!(usage.total_ticks().unwrap() > 0);
    assert!(
        !Path::new(&format!("/proc/{}", usage.pid)).exists(),
        "final counters must survive process reap"
    );
}

async fn measured_cpu(
    client: &Client,
    id: &str,
    after_sequence: u64,
    positive_interval: bool,
) -> ReceivedCpuSample {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = client.task(id).await.unwrap();
            assert!(!task.phase.terminal(), "{task:?}");
            if let Some(sample) = task.cpu_sample
                && sample.report.sequence > after_sequence
                && sample.report.sample.usage.is_some()
                && sample
                    .interval
                    .as_ref()
                    .is_some_and(|interval| !positive_interval || interval.total_cpu_time_ns > 0)
            {
                return sample;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("native CPU observations for {id}: {error}"))
}

fn assert_cpu_kernel_evidence(sample: &ReceivedCpuSample, pid: u32) {
    let usage = sample.report.sample.usage.as_ref().unwrap();
    assert_eq!(usage.pid, pid);
    let environment = fs::read(format!("/proc/{pid}/environ")).unwrap();
    let launch = environment
        .split(|byte| *byte == 0)
        .find_map(|field| field.strip_prefix(b"PVISOR_KRUN_RUNNER_SPEC="))
        .expect("CPU process must carry its actual private native launch binding");
    use std::os::unix::ffi::OsStrExt;
    let launch: serde_json::Value =
        serde_json::from_slice(&fs::read(Path::new(std::ffi::OsStr::from_bytes(launch))).unwrap())
            .unwrap();
    assert_eq!(
        launch["run_id"].as_str(),
        Some(sample.report.sample.run_id.as_str()),
        "CPU telemetry must name the Run actually executing in the kernel process"
    );
    assert_eq!(
        usage.clock_ticks_per_second,
        unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64
    );
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let (_, fields) = stat.rsplit_once(')').unwrap();
    let fields: Vec<_> = fields.split_whitespace().collect();
    assert_eq!(
        usage.start_time_ticks,
        fields[22 - 3].parse::<u64>().unwrap()
    );
    assert!(usage.user_time_ticks <= fields[14 - 3].parse::<u64>().unwrap());
    assert!(usage.system_time_ticks <= fields[15 - 3].parse::<u64>().unwrap());
    assert!(usage.guest_time_ticks <= fields[43 - 3].parse::<u64>().unwrap());
    assert!(usage.total_ticks().unwrap() > 0);
    let interval = sample.interval.as_ref().unwrap();
    assert!(interval.elapsed_monotonic_ns > 0);
    eprintln!(
        "Native CPU sample PID {pid}: user={} system={} guest={} Hz={} consumed_mCPU={}",
        usage.user_time_ticks,
        usage.system_time_ticks,
        usage.guest_time_ticks,
        usage.clock_ticks_per_second,
        interval.cpu_millis
    );
}

struct CpuUploadGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for CpuUploadGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[path = "common/s3.rs"]
mod s3_fixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM, FUSE and libkrunfw; run just test-cluster-vm"]
async fn independent_workers_fetch_pinned_s3_layers_without_source_access_or_storage_credentials_in_guests()
 {
    use std::sync::atomic::Ordering;
    for device in ["/dev/kvm", "/dev/fuse"] {
        assert!(fs::metadata(device).unwrap().file_type().is_char_device());
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("publisher/source");
    let staging = root.join("publisher/local-cache");
    let (base, workspace, toolkit) = tokio::task::spawn_blocking({
        let source = source.clone();
        let staging = staging.clone();
        move || {
            (
                publish_layer(
                    &source,
                    &staging,
                    "s3-env-base",
                    &[
                        ("env/base-only", "base-only\n"),
                        ("env/modify", "original\n"),
                    ],
                    true,
                ),
                publish_layer(
                    &source,
                    &staging,
                    "s3-env-workspace",
                    &[("env/workspace-only", "workspace-only\n")],
                    false,
                ),
                publish_layer(
                    &source,
                    &staging,
                    "s3-env-toolkit",
                    &[("env/conflict", "toolkit\n")],
                    false,
                ),
            )
        }
    })
    .await
    .unwrap();
    let s3 = s3_fixture::MockS3::start();
    let publish = |name: &str| -> pvisor_cluster::EnvironmentLayer {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor-cache"))
            .env_clear()
            .env("HOME", root.join("publisher/home"))
            .env("XDG_CACHE_HOME", root.join("publisher/blocks"))
            .env("AWS_ACCESS_KEY_ID", "AKIATEST")
            .env("AWS_SECRET_ACCESS_KEY", "test-secret")
            .env("AWS_SESSION_TOKEN", "test-token")
            .env("AWS_DEFAULT_REGION", "us-east-1")
            .env("AWS_ENDPOINT", &s3.endpoint)
            .env("AWS_ALLOW_HTTP", "true")
            .args([
                "--backend",
                "s3",
                "--location",
                "s3://cache-bucket/team",
                "--image-store",
            ])
            .arg(&source)
            .args(["publish", &format!("{name}:test")])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let pvisor::cache::Response::Prepared {
            image_handle: Some(handle),
            digest,
            ..
        } = serde_json::from_slice(&output.stdout).unwrap()
        else {
            panic!("missing remote immutable revision")
        };
        EnvironmentLayer {
            handle,
            manifest_digest: digest,
        }
    };
    for (name, expected) in [
        ("s3-env-base", &base),
        ("s3-env-workspace", &workspace),
        ("s3-env-toolkit", &toolkit),
    ] {
        assert_eq!(publish(name), *expected);
    }
    // Change mutable HEAD before either Worker opens the original template.
    fs::write(
        source
            .join("rootfs-v3/sha256")
            .join(&toolkit.manifest_digest[7..])
            .join("env/conflict"),
        "toolkit-v2\n",
    )
    .unwrap();
    let newer_toolkit = publish("s3-env-toolkit");
    assert_ne!(newer_toolkit.handle, toolkit.handle);
    let old_revision = toolkit.handle.rsplit(':').next().unwrap();
    assert!(
        s3.objects
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.contains(&format!("/revisions/{old_revision}/"))),
        "updating HEAD must preserve the old immutable revision"
    );
    fs::remove_dir_all(root.join("publisher")).unwrap();
    s3.read_only.store(true, Ordering::SeqCst);
    let writes = s3.puts.load(Ordering::SeqCst);
    let initial_reads = s3.gets.load(Ordering::SeqCst);
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
    let template = EnvironmentTemplate {
        version: CLUSTER_VERSION,
        architecture: "amd64".into(),
        base,
        workspace: Some(workspace),
        toolkits: vec![toolkit],
    };
    let original = admin.publish_environment(&template).await.unwrap();
    let mut revised = template.clone();
    revised.toolkits = vec![newer_toolkit];
    let revised = admin.publish_environment(&revised).await.unwrap();
    let firmware =
        std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR").expect("regular firmware required");
    assert!(
        fs::symlink_metadata(Path::new(&firmware).join("libkrunfw.so.5"))
            .unwrap()
            .is_file()
    );
    let profile = root.join("s3-worker.toml");
    fs::write(&profile, format!("[environments]\nenabled = true\n[memory_sampling]\nenabled = true\ninterval_ms = 1000\n[vm]\nlibrary_dir = {}\n[overlaynet]\nmode = 'off'\n",serde_json::to_string(&firmware.to_string_lossy()).unwrap())).unwrap();
    let binary = worker_binary(root);
    let mut workers = Vec::new();
    for node in ["node-a", "node-b"] {
        let directory = root.join(node);
        fs::create_dir_all(directory.join("home")).unwrap();
        let log = directory.join("worker.log");
        workers.push(ChildGuard(
            Command::new(&binary)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", directory.join("home"))
                .env("XDG_CACHE_HOME", directory.join("blocks"))
                .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
                .env("PVISOR_CACHE_BACKEND", "s3")
                .env("PVISOR_CACHE_LOCATION", "s3://cache-bucket/team")
                .env("PVISOR_CACHE_READ_ONLY", "true")
                .env("AWS_ACCESS_KEY_ID", "AKIATEST")
                .env("AWS_SECRET_ACCESS_KEY", "test-secret")
                .env("AWS_SESSION_TOKEN", "test-token")
                .env("AWS_DEFAULT_REGION", "us-east-1")
                .env("AWS_ENDPOINT", &s3.endpoint)
                .env("AWS_ALLOW_HTTP", "true")
                .args([
                    "--url",
                    &url,
                    "--id",
                    node,
                    "--backend",
                    "vm",
                    "--poll-ms",
                    "50",
                    "--slots",
                    "1",
                    "--cpu-millis",
                    "1000",
                    "--memory-bytes",
                    "268435456",
                    "--label",
                    &format!("node={node}"),
                ])
                .arg("--state")
                .arg(directory.join("state"))
                .arg("--config")
                .arg(&profile)
                .current_dir(&directory)
                .stdout(Stdio::null())
                .stderr(Stdio::from(fs::File::create(log).unwrap()))
                .spawn()
                .unwrap(),
        ));
    }
    for node in ["node-a", "node-b"] {
        let mut job = task(node, &original.digest, node);
        job.labels.insert("node".into(), node.into());
        job.retain_artifacts = Some(ArtifactRetention {
            execution_checkpoint: None,
            version: ARTIFACT_EXPORT_VERSION,
            trace: true,
            workspace_upper: true,
        });
        let RunInvocation::Process(process) = &mut job.run.invocation;
        process.args[1] = process.args[1].replace("/bin/sleep 2", "/bin/sleep 6");
        process.args[1] = format!(
            "set -eu; test -z \"${{AWS_ACCESS_KEY_ID+x}}${{AWS_SECRET_ACCESS_KEY+x}}${{AWS_SESSION_TOKEN+x}}${{PVISOR_CACHE_LOCATION+x}}${{PVISOR_CLUSTER_WORKER_TOKEN+x}}\"; {}",
            process.args[1]
        );
        admin.submit(&job).await.unwrap();
    }
    let native = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let tasks = [
                admin.task("node-a").await.unwrap(),
                admin.task("node-b").await.unwrap(),
            ];
            for task in &tasks {
                assert!(
                    !task.phase.terminal(),
                    "premature S3 VM completion: {task:?}\n{}",
                    fs::read_to_string(root.join(&task.spec.id).join("worker.log")).unwrap()
                );
            }
            let usages: Option<Vec<_>> = tasks
                .iter()
                .map(|task| task.memory_sample.as_ref()?.report.sample.usage.clone())
                .collect();
            if let Some(usages) = usages {
                break usages;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_ne!(native[0].pid, native[1].pid);
    for (i, node) in ["node-a", "node-b"].iter().enumerate() {
        assert!(native[i].guest_ram.rss_bytes > 0);
        let threads = fs::read_dir(format!("/proc/{}/task", workers[i].0.id()))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .ok()
                    .and_then(|entry| fs::read_to_string(entry.path().join("comm")).ok())
                    .is_some_and(|name| name.trim() == "pvisor-cache-s3")
            })
            .count();
        assert_eq!(
            threads, 1,
            "three layers must share one S3 I/O loop on {node}"
        );
    }
    assert!(s3.gets.load(Ordering::SeqCst) > initial_reads);
    for node in ["node-a", "node-b"] {
        let finished =
            finished_with_log(&admin, node, Some(&root.join(node).join("worker.log"))).await;
        assert_eq!(finished.phase, TaskPhase::Succeeded);
        assert_eq!(finished.lease.as_ref().unwrap().key.worker_id, node);
        assert_eq!(
            finished.result.as_ref().unwrap().output.stdout.as_deref(),
            Some(format!("toolkit|base-only|workspace-only|{node}\n").as_str())
        );
        let output = root.join(format!("download-{node}"));
        admin.download_artifacts(node, &output).await.unwrap();
        let bundle: pvisor::RunBundle =
            serde_json::from_slice(&fs::read(output.join("run-bundle.json")).unwrap()).unwrap();
        assert_eq!(
            bundle.executor_plan.as_ref().unwrap().kind,
            ExecutorKind::VirtualMachine
        );
        assert!(!bundle.safety.host_process);
        assert!(
            fs::metadata(output.join("workspace-upper.tar"))
                .unwrap()
                .len()
                > 0
        );
        assert!(fs::metadata(output.join("trace")).unwrap().len() > 0);
        let extracted = root
            .join(node)
            .join("state/environment-mounts/rootfs-v3/sha256");
        if extracted.exists() {
            assert_eq!(
                fs::read_dir(extracted).unwrap().count(),
                0,
                "readonly layers must not be fully extracted"
            );
        }
    }
    // Start a fresh Attempt against the newly published revision on node B.
    let mut job = task("node-b-v2", &revised.digest, "revision-v2");
    job.labels.insert("node".into(), "node-b".into());
    admin.submit(&job).await.unwrap();
    let finished =
        finished_with_log(&admin, "node-b-v2", Some(&root.join("node-b/worker.log"))).await;
    assert_eq!(finished.phase, TaskPhase::Succeeded);
    assert_eq!(
        finished.result.unwrap().output.stdout.as_deref(),
        Some("toolkit-v2|base-only|workspace-only|revision-v2\n")
    );
    assert_eq!(
        s3.puts.load(Ordering::SeqCst),
        writes,
        "Workers must never publish to the read-only remote cache"
    );
    assert!(!s3.deny_reads.load(Ordering::SeqCst));
    assert!(!s3.lost_head_ack.load(Ordering::SeqCst));
    // Cached immutable bodies can be reused without projecting an OCI tree.
    for node in ["node-a", "node-b"] {
        assert!(
            fs::read_dir(root.join(node).join("blocks"))
                .unwrap()
                .next()
                .is_some()
        );
    }
    assert!(!root.join("publisher").exists());
    drop(workers);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM, FUSE and libkrunfw; run just test-cluster-vm"]
async fn worker_checkpoint_publication_recovers_after_crash_and_restores_on_an_independent_worker()
{
    use std::sync::atomic::Ordering;
    for device in ["/dev/kvm", "/dev/fuse"] {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device)
            .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("source");
    let cache = root.join("cache");
    let layer = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            publish_layer(
                &source,
                &cache,
                "remote-checkpoint-base",
                &[("env/seed", "memory-before-checkpoint\n")],
                true,
            )
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
                "remote-checkpoint-toolkit",
                &[
                    ("env/immutable-probe", "retained-readonly-lower\n"),
                    ("env/unvisited-probe", "unvisited-lower-payload\n"),
                ],
                false,
            )
        }
    })
    .await
    .unwrap();
    let remote = s3_fixture::MockS3::start();
    let journal = root.join("journal");
    let settings = SchedulerConfig {
        lease_duration_ms: 3000,
        ..Default::default()
    };
    let router = pvisor_cluster::server::router(
        Scheduler::open(&journal, settings.clone()).unwrap(),
        ADMIN.into(),
        WORKER.into(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let environment = admin
        .publish_environment(&EnvironmentTemplate {
            version: CLUSTER_VERSION,
            architecture: "amd64".into(),
            base: layer,
            workspace: None,
            toolkits: vec![toolkit],
        })
        .await
        .unwrap();
    let firmware =
        std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR").expect("regular firmware required");
    let binary = worker_binary(root);
    let launch = |node: &str, publish: bool| {
        let directory = root.join(node);
        fs::create_dir_all(directory.join("home")).unwrap();
        let config = root.join(format!("{node}.toml"));
        let pool = root.join(format!("{node}-filesystem-pool"));
        fs::write(&config, format!("[environments]\nenabled = true\n[overlaynet]\nmode = 'off'\npolicy = 'deny'\n[vm]\nlibrary_dir = {}\nsnapshot_filesystem_pool = {}\n[checkpoint_storage]\nrepository = 'checkpoints'\nbackend = 's3'\nlocation = 's3://cache-bucket/team'\npublish = {publish}\n", serde_json::to_string(&firmware.to_string_lossy()).unwrap(), serde_json::to_string(&pool.to_string_lossy()).unwrap())).unwrap();
        ChildGuard(
            Command::new(&binary)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", directory.join("home"))
                .env("XDG_CACHE_HOME", directory.join("blocks"))
                .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
                .env("PVISOR_CACHE_BACKEND", "filesystem")
                .env(
                    "PVISOR_CACHE_LOCATION",
                    if publish {
                        cache.clone()
                    } else {
                        directory.join("unavailable-environments")
                    },
                )
                .env("AWS_ACCESS_KEY_ID", "AKIATEST")
                .env("AWS_SECRET_ACCESS_KEY", "test-secret")
                .env("AWS_SESSION_TOKEN", "test-token")
                .env("AWS_DEFAULT_REGION", "us-east-1")
                .env("AWS_ENDPOINT", &remote.endpoint)
                .env("AWS_ALLOW_HTTP", "true")
                .args([
                    "--url",
                    &url,
                    "--id",
                    node,
                    "--backend",
                    "vm",
                    "--poll-ms",
                    "50",
                    "--slots",
                    "1",
                    "--cpu-millis",
                    "1000",
                    "--memory-bytes",
                    "268435456",
                    "--label",
                    &format!("node={node}"),
                ])
                .arg("--state")
                .arg(directory.join("state"))
                .arg("--config")
                .arg(config)
                .current_dir(&directory)
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    fs::File::create(root.join(format!("{node}.log"))).unwrap(),
                ))
                .spawn()
                .unwrap(),
        )
    };
    let mut source_worker = launch("source-worker", true);
    let target_worker = launch("target-worker", false);
    let mut source_task = task("remote-source", &environment.digest, "unused");
    source_task
        .labels
        .insert("node".into(), "source-worker".into());
    source_task.run.runtime.timeout_ms = Some(120_000);
    source_task.retain_artifacts = Some(ArtifactRetention {
        version: ARTIFACT_EXPORT_VERSION,
        trace: false,
        workspace_upper: true,
        execution_checkpoint: Some(CheckpointRetention {
            version: 1,
            repository: "checkpoints".into(),
        }),
    });
    let RunInvocation::Process(process) = &mut source_task.run.invocation;
    process.args = vec!["-c".into(), "set -eu; test -z \"${AWS_ACCESS_KEY_ID+x}${AWS_SECRET_ACCESS_KEY+x}${AWS_SESSION_TOKEN+x}${PVISOR_CLUSTER_WORKER_TOKEN+x}\"; read -r token < /env/seed; : > /env/seed; printf 'owned-before-checkpoint\\n' > /env/owned; exec 3<>/env/owned; exec 4</env/immutable-probe; printf 'ready\\n' > /env/ready; while [ ! -e /env/allow-continue ]; do /bin/sleep 0.2; done; read -r owned <&3; read -r immutable <&4; test \"$immutable\" = retained-readonly-lower; read -r unvisited < /env/unvisited-probe; test \"$unvisited\" = unvisited-lower-payload; printf '%s|%s\\n' \"$token\" \"$owned\"; printf 'continued\\n' > /env/continued".into()];
    admin.submit(&source_task).await.unwrap();
    guest_ready(
        &admin,
        "remote-source",
        &root.join("source-worker/state/tasks/remote-source-1/upper/env/ready"),
        &root.join("source-worker.log"),
    )
    .await;
    let first = controlled(
        &admin,
        "remote-source",
        "seed-immutable-lowers",
        ControlAction::Checkpoint,
        &root.join("source-worker.log"),
    )
    .await;
    let Some(ControlOutcome::Checkpointed { checkpoint: first }) = first.outcome else {
        panic!("fresh VM must seal its first checkpoint");
    };
    let source_pool = root.join("source-worker-filesystem-pool");
    let first_store = pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(
        &first.store,
        &source_pool,
    )
    .unwrap();
    let first_manifest: pvisor::environment_snapshot::EnvironmentManifest = serde_json::from_slice(
        &fs::read(
            first
                .store
                .join("objects")
                .join(&first.snapshot_id)
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let first_snapshot = first_store
        .open_for_restore(&first.snapshot_id, &first_manifest.compatibility)
        .unwrap();
    assert_eq!(first_snapshot.manifest().version, 5);
    let first_machine: serde_json::Value =
        serde_json::from_slice(&first_snapshot.machine_bytes().unwrap()).unwrap();
    assert_eq!(first_machine["version"], 2);
    let live_upper = root.join("source-worker/state/tasks/remote-source-1/upper");
    assert_eq!(first_machine["root"]["upper"].as_str(), live_upper.to_str());
    assert!(
        first_machine["private_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| {
                record["source"].as_str() == live_upper.to_str()
                    && first_snapshot
                        .manifest()
                        .filesystem
                        .entries
                        .iter()
                        .any(|entry| {
                            entry.path == record["path"].as_str().unwrap().as_bytes()
                                && entry.object
                                    == pvisor::environment_snapshot::TreeObject::Directory
                        })
            })
    );
    assert!(first_snapshot.manifest().filesystem_blocks.is_some());
    assert!(
        !first
            .store
            .join("objects")
            .join(&first.snapshot_id)
            .join("rootfs")
            .exists()
    );
    assert!(!first_snapshot.manifest().filesystem_layers.is_empty());
    let fresh_lowers = first_snapshot
        .manifest()
        .filesystem_layers
        .iter()
        .map(|layer| {
            let path = Path::new(std::ffi::OsStr::from_bytes(&layer.path));
            let probe = layer
                .filesystem
                .entries
                .iter()
                .find(|entry| {
                    matches!(
                        entry.object,
                        pvisor::environment_snapshot::TreeObject::File { .. }
                    )
                })
                .unwrap()
                .path
                .clone();
            let actual = first_snapshot.owned_layer_path(path).unwrap();
            (
                layer.id.clone(),
                layer.path.clone(),
                probe.clone(),
                fs::metadata(actual.join(std::ffi::OsStr::from_bytes(&probe)))
                    .unwrap()
                    .ino(),
            )
        })
        .collect::<Vec<_>>();
    let first_private_frames = first_snapshot
        .manifest()
        .filesystem_blocks
        .as_ref()
        .unwrap()
        .files
        .iter()
        .flat_map(|file| file.blocks.iter().flatten())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            (
                id.clone(),
                fs::metadata(source_pool.join("content").join(id))
                    .unwrap()
                    .ino(),
            )
        })
        .collect::<Vec<_>>();
    assert!(!first_private_frames.is_empty());
    let filesystem_work = |node: &str, task: &str| {
        fs::read_to_string(root.join(format!("{node}.log")))
            .unwrap()
            .lines()
            .filter(|line| {
                line.starts_with("pvisor-startup-detail ")
                    && line
                        .split_whitespace()
                        .any(|field| field == "stage=checkpoint.filesystem")
                    && line.split_whitespace().any(|field| {
                        field == format!("run_id={}", serde_json::to_string(task).unwrap())
                    })
            })
            .map(|line| {
                line.split_whitespace()
                    .filter_map(|field| {
                        let (key, value) = field.split_once('=')?;
                        matches!(
                            key,
                            "payload_bytes" | "encoded_frames" | "reused_frames" | "zero_frames"
                        )
                        .then(|| (key.to_owned(), value.parse::<u64>().unwrap()))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .collect::<Vec<_>>()
    };
    let first_work = filesystem_work("source-worker", "remote-source");
    assert_eq!(first_work.len(), 1);
    assert_eq!(
        first_work[0]["encoded_frames"],
        first_private_frames.len() as u64
    );
    drop(first_snapshot);
    first_store.delete(&first.snapshot_id).unwrap();
    first_store.collect_abandoned().unwrap();
    let source_key = admin
        .task("remote-source")
        .await
        .unwrap()
        .lease
        .unwrap()
        .key;
    remote.pause_checkpoint_put.store(true, Ordering::SeqCst);
    admin
        .control(
            "remote-source",
            &ControlRequest {
                request_id: "suspend".into(),
                action: ControlAction::Suspend,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(40), async {
        while !remote.checkpoint_put_waiting.load(Ordering::SeqCst) {
            let task = admin.task("remote-source").await.unwrap();
            assert!(
                !task.phase.terminal(),
                "source failed before checkpoint publication: {task:?}\n{}",
                fs::read_to_string(root.join("source-worker.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("checkpoint PUT must be interrupted after commit");
    let pending: Vec<_> = fs::read_dir(root.join("source-worker/state/outbox/pending"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(pending.len(), 1);
    let native: serde_json::Value =
        serde_json::from_slice(&fs::read(&pending[0]).unwrap()).unwrap();
    assert_eq!(native["ready"], false);
    assert_eq!(native["completion"]["result"]["state"], "hibernated");
    assert!(native["completion"]["artifact_error"].is_null());
    let native_result: pvisor_core::RunResult =
        serde_json::from_value(native["completion"]["result"].clone()).unwrap();
    let recaptured = pvisor_core::operation::ExecutionSuspension::from_result(&native_result)
        .unwrap()
        .checkpoint;
    let manifest_path = recaptured
        .store
        .join("objects")
        .join(&recaptured.snapshot_id)
        .join("manifest.json");
    let manifest: pvisor::environment_snapshot::EnvironmentManifest =
        serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
    let sealed = first_store
        .open_for_restore(&recaptured.snapshot_id, &manifest.compatibility)
        .unwrap();
    let machine: serde_json::Value =
        serde_json::from_slice(&sealed.machine_bytes().unwrap()).unwrap();
    let recaptured_private_frames = sealed
        .manifest()
        .filesystem_blocks
        .as_ref()
        .unwrap()
        .files
        .iter()
        .flat_map(|file| file.blocks.iter().flatten())
        .collect::<std::collections::BTreeSet<_>>();
    let recapture_work = filesystem_work("source-worker", "remote-source");
    assert_eq!(recapture_work.len(), 2);
    assert_eq!(
        recapture_work[1]["encoded_frames"], 0,
        "unchanged native private frames must be inherited without invoking the encoder"
    );
    assert_eq!(
        recapture_work[1]["reused_frames"],
        sealed
            .manifest()
            .filesystem_blocks
            .as_ref()
            .unwrap()
            .files
            .iter()
            .flat_map(|file| file.blocks.iter().flatten())
            .count() as u64
    );
    assert_eq!(
        recapture_work[1]["payload_bytes"],
        first_work[0]["payload_bytes"]
    );
    eprintln!("native private encoding work: {first_work:?} -> {recapture_work:?}");
    assert_eq!(
        recaptured_private_frames.len(),
        first_private_frames.len(),
        "unchanged native private files must reuse all frame identities"
    );
    for (id, inode) in &first_private_frames {
        assert!(recaptured_private_frames.contains(id));
        assert_eq!(
            fs::metadata(
                recaptured
                    .store
                    .join("objects")
                    .join(&recaptured.snapshot_id)
                    .join("filesystem-blocks")
                    .join(id)
            )
            .unwrap()
            .ino(),
            *inode,
            "native private frame was copied after parent retirement/GC"
        );
    }
    eprintln!(
        "native private filesystem: {} encoded frame inodes retained after parent deletion/GC, no sealed private tree",
        first_private_frames.len()
    );
    for (id, path, probe, inode) in &fresh_lowers {
        let logical = Path::new(std::ffi::OsStr::from_bytes(path));
        let actual = sealed.owned_layer_path(logical).unwrap();
        assert_eq!(
            fs::metadata(actual.join(std::ffi::OsStr::from_bytes(probe)))
                .unwrap()
                .ino(),
            *inode,
            "fresh VM recapture copied its immutable lower"
        );
        assert!(
            !recaptured
                .store
                .join("objects")
                .join(&recaptured.snapshot_id)
                .join("rootfs")
                .join(logical)
                .exists()
        );
        assert!(
            machine["filesystem_layers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|layer| layer["id"] == *id)
        );
    }
    assert!(
        machine["ram_delta"].is_null(),
        "fresh VM still requires full RAM capture"
    );
    eprintln!(
        "native initial-VM filesystem: {} lower inodes retained after deletion/GC of its first checkpoint, no private lower copies",
        fresh_lowers.len()
    );
    drop(sealed);
    // Hold the real S3 producer beyond a controller lease. Its independent poll
    // loop must renew and conservatively keep the entire admission reservation.
    tokio::time::sleep(Duration::from_millis(3300)).await;
    let stalled = admin.task("remote-source").await.unwrap();
    assert!(!stalled.phase.terminal());
    assert_eq!(stalled.lease.as_ref().unwrap().key, source_key);
    assert_eq!(stalled.current_reservation(), source_task.resources);
    source_worker.0.kill().unwrap();
    source_worker.0.wait().unwrap();
    remote.pause_checkpoint_put.store(false, Ordering::SeqCst);
    source_worker = launch("source-worker", true);
    let stopped = finished_with_log(
        &admin,
        "remote-source",
        Some(&root.join("source-worker.log")),
    )
    .await;
    assert_eq!(
        stopped.phase,
        TaskPhase::Suspended,
        "{stopped:?}\n{}",
        fs::read_to_string(root.join("source-worker.log")).unwrap()
    );
    assert_eq!(
        serde_json::to_value(stopped.result.as_ref().unwrap()).unwrap(),
        serde_json::to_value(&native_result).unwrap()
    );
    assert_eq!(
        stopped.generation, 1,
        "recovery must never re-execute guest argv"
    );
    assert_eq!(stopped.lease.as_ref().unwrap().key, source_key);
    assert!(
        fs::read_to_string(root.join("source-worker.log"))
            .unwrap()
            .contains("recovering 1 durable terminal outbox records")
    );
    let publication = stopped
        .checkpoint_publication
        .clone()
        .expect("controller must durably retain the verified publication");
    assert_eq!(
        publication.checkpoint,
        pvisor_core::operation::ExecutionSuspension::from_result(&native_result)
            .unwrap()
            .checkpoint
    );
    // A second VM starts from its environment, with no restore/owner hints.
    // Its FIRST capture must hit the independently retained immutable pool.
    // Watch actual temporary tree creation too, including copies later deleted.
    let mut pool_events = {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        assert!(fd >= 0);
        let pending =
            std::ffi::CString::new(source_pool.join("pending").as_os_str().as_bytes()).unwrap();
        assert!(unsafe { libc::inotify_add_watch(fd, pending.as_ptr(), libc::IN_CREATE) } >= 0);
        unsafe { fs::File::from_raw_fd(fd) }
    };
    let mut fresh = source_task.clone();
    fresh.id = "remote-fresh-pool-hit".into();
    fresh.run.run_id = "remote-fresh-pool-hit".into();
    fresh.retain_artifacts = None;
    assert!(fresh.restore.is_none());
    admin.submit(&fresh).await.unwrap();
    let fresh_directory = root.join("source-worker/state/tasks/remote-fresh-pool-hit-1");
    guest_ready(
        &admin,
        &fresh.id,
        &fresh_directory.join("upper/env/ready"),
        &root.join("source-worker.log"),
    )
    .await;
    let fresh_checkpoint = controlled(
        &admin,
        &fresh.id,
        "first-capture-pool-hit",
        ControlAction::Checkpoint,
        &root.join("source-worker.log"),
    )
    .await;
    let Some(ControlOutcome::Checkpointed {
        checkpoint: fresh_checkpoint,
    }) = fresh_checkpoint.outcome
    else {
        panic!("fresh VM must capture its state");
    };
    let fresh_store = pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(
        &fresh_checkpoint.store,
        &source_pool,
    )
    .unwrap();
    let fresh_snapshot = fresh_store
        .open_for_restore(&fresh_checkpoint.snapshot_id, &publication.compatibility)
        .unwrap();
    let fresh_machine: serde_json::Value =
        serde_json::from_slice(&fresh_snapshot.machine_bytes().unwrap()).unwrap();
    let fresh_work = filesystem_work("source-worker", &fresh.id);
    assert_eq!(fresh_work.len(), 1);
    let retained_private_ids = first_private_frames
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let fresh_private_ids = fresh_snapshot
        .manifest()
        .filesystem_blocks
        .as_ref()
        .unwrap()
        .files
        .iter()
        .flat_map(|file| file.blocks.iter().flatten())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fresh_work[0]["encoded_frames"],
        fresh_private_ids.difference(&retained_private_ids).count() as u64,
        "another VM must encode only content absent from the shared private-frame pool"
    );
    assert!(fresh_work[0]["reused_frames"] > 0);
    eprintln!("native different-VM encoding work: {fresh_work:?}");
    assert_eq!(
        fresh_snapshot.manifest().filesystem_layers.len(),
        fresh_lowers.len()
    );
    for (id, path, probe, inode) in &fresh_lowers {
        let logical = Path::new(std::ffi::OsStr::from_bytes(path));
        let actual = fresh_snapshot.owned_layer_path(logical).unwrap();
        assert_eq!(
            fs::metadata(actual.join(std::ffi::OsStr::from_bytes(probe)))
                .unwrap()
                .ino(),
            *inode,
            "a different VM's first capture copied the lower"
        );
        assert!(
            !fresh_checkpoint
                .store
                .join("objects")
                .join(&fresh_checkpoint.snapshot_id)
                .join("rootfs")
                .join(logical)
                .exists()
        );
        assert!(
            fresh_machine["filesystem_layers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|layer| layer["id"] == *id)
        );
    }
    assert!(fresh_machine["ram_delta"].is_null());
    let mut events = [0; 65536];
    use std::io::Read;
    assert!(
        matches!(pool_events.read(&mut events), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "pool hit must create no temporary data tree"
    );
    eprintln!(
        "native different-VM first capture: {} existing lower inodes reused, no private or temporary lower copies",
        fresh_lowers.len()
    );
    drop(fresh_snapshot);
    fs::write(
        fresh_directory.join("upper/env/allow-continue"),
        "continue\n",
    )
    .unwrap();
    let fresh_result =
        finished_with_log(&admin, &fresh.id, Some(&root.join("source-worker.log"))).await;
    assert_eq!(fresh_result.phase, TaskPhase::Succeeded, "{fresh_result:?}");
    assert_eq!(
        fresh_result.result.unwrap().output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint\n")
    );
    assert!(
        !root
            .join("source-worker/state/tasks/remote-source-1/upper/env/continued")
            .exists()
    );
    let source_artifacts = root.join("source-artifacts");
    admin
        .download_artifacts("remote-source", &source_artifacts)
        .await
        .unwrap();
    assert_eq!(
        retained_upper_file(&source_artifacts, "upper/env/owned"),
        Some(b"owned-before-checkpoint\n".to_vec()),
        "suspended v5 workspace upper must remain downloadable after Worker restart"
    );
    let retained: CheckpointPublication = serde_json::from_slice(
        &fs::read(source_artifacts.join("execution-checkpoint.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(retained, publication);
    drop(source_worker);
    fs::remove_dir_all(root.join("source-worker")).unwrap();
    let source_pool = root.join("source-worker-filesystem-pool");
    pvisor::environment_snapshot::SnapshotStore::new(&source_pool)
        .unwrap()
        .collect_abandoned()
        .unwrap();
    fs::remove_dir_all(&source_pool).unwrap();
    fs::remove_dir_all(&source).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    fs::remove_dir_all(source_artifacts).unwrap();
    assert!(!publication.checkpoint.store.exists());
    let gc = admin
        .plan_artifact_gc(&ArtifactGcRequest {
            version: CLUSTER_VERSION,
            retire_before_ms: Some(stopped.updated_at_ms + 1),
            max_objects: 4096,
        })
        .await
        .unwrap();
    assert_eq!(
        admin.apply_artifact_gc(&gc.id).await.unwrap().retired_tasks,
        1
    );
    assert!(admin.artifacts("remote-source").await.is_err());
    assert_eq!(
        admin
            .task("remote-source")
            .await
            .unwrap()
            .checkpoint_publication,
        Some(publication.clone())
    );
    remote.read_only.store(true, Ordering::SeqCst);
    let writes = remote.puts.load(Ordering::SeqCst);
    // Reconstruct controller authority solely from its WAL before placement.
    shutdown.send(()).unwrap();
    (&mut server).await.unwrap();
    // The HTTP listener is drained, but an already running lease-reaper
    // blocking task may still own the scheduler for its final transaction.
    // Reopen only once the real authority locks are released. Other failures
    // remain fatal, and a retained controller owner must fail this bounded wait.
    let scheduler = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match Scheduler::open(&journal, settings.clone()) {
                Ok(scheduler) => break scheduler,
                Err(error) => {
                    assert!(
                        error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
                        "controller restart failed: {error:#}"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    })
    .await
    .expect("retired controller must release its authority locks before restart");
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap();
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    assert_eq!(
        admin
            .task("remote-source")
            .await
            .unwrap()
            .checkpoint_publication,
        Some(publication.clone())
    );
    let mut branch = source_task;
    branch.id = "remote-branch".into();
    branch.run.run_id = "remote-branch".into();
    branch.run.parent_run_id = Some("remote-source".into());
    branch.labels.insert("node".into(), "target-worker".into());
    branch.restore = Some(ExecutionRestore {
        task_id: "remote-source".into(),
        request_id: "suspend".into(),
    });
    branch.retain_artifacts = None;
    admin.submit(&branch).await.unwrap();
    let directory = root.join("target-worker/state/tasks/remote-branch-1");
    let upper = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let task = admin.task("remote-branch").await.unwrap();
            assert!(
                !task.phase.terminal(),
                "restore failed: {task:?}\n{}",
                fs::read_to_string(root.join("target-worker.log")).unwrap()
            );
            if let Ok(record) = pvisor::RunRecord::read(&directory)
                && let Some(overlay) = record.overlay
                && overlay.upper.path().join("env/ready").exists()
            {
                break overlay.upper.path().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("independent target must import and restore the checkpoint");
    assert!(
        fs::read(upper.join("env/seed")).unwrap().is_empty(),
        "argv re-execution cannot reconstruct the lost RAM token"
    );
    let imported = root
        .join("target-worker/state/checkpoint-imports")
        .join(&publication.checkpoint.snapshot_id);
    let target_pool = root.join("target-worker-filesystem-pool");
    let import_store =
        pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(&imported, &target_pool)
            .unwrap();
    let parent = import_store
        .open_for_restore(
            &publication.checkpoint.snapshot_id,
            &publication.compatibility,
        )
        .unwrap();
    let baseline = parent.manifest().ram_blocks.clone().unwrap();
    let baseline_hash = parent.manifest().ram_sha256.clone();
    assert_eq!(parent.manifest().version, 5);
    assert!(parent.manifest().filesystem_blocks.is_some());
    assert!(
        !imported
            .join("objects")
            .join(&publication.checkpoint.snapshot_id)
            .join("rootfs")
            .exists()
    );
    assert!(!parent.manifest().filesystem_layers.is_empty());
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let baseline_lowers = parent
        .manifest()
        .filesystem_layers
        .iter()
        .map(|layer| {
            let path = Path::new(std::ffi::OsStr::from_bytes(&layer.path));
            let actual = parent.owned_layer_path(path).unwrap();
            let probe = layer
                .filesystem
                .entries
                .iter()
                .find(|entry| {
                    matches!(
                        entry.object,
                        pvisor::environment_snapshot::TreeObject::File { .. }
                    )
                })
                .unwrap()
                .path
                .clone();
            (
                layer.id.clone(),
                layer.path.clone(),
                probe.clone(),
                fs::metadata(actual.join(std::ffi::OsStr::from_bytes(&probe)))
                    .unwrap()
                    .ino(),
            )
        })
        .collect::<Vec<_>>();
    drop(parent);
    // The running VM owns authenticated frame references independently of the
    // imported object. Recapture must work after the parent has been retired.
    import_store
        .delete(&publication.checkpoint.snapshot_id)
        .unwrap();
    import_store.collect_abandoned().unwrap();
    let derived = controlled(
        &admin,
        "remote-branch",
        "derive-after-import",
        ControlAction::Checkpoint,
        &root.join("target-worker.log"),
    )
    .await;
    let Some(ControlOutcome::Checkpointed {
        checkpoint: derived,
    }) = derived.outcome
    else {
        panic!("independent Worker failed to capture its restored VM");
    };
    let derived_store = pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(
        &derived.store,
        &target_pool,
    )
    .unwrap();
    let child = derived_store
        .open_for_restore(&derived.snapshot_id, &publication.compatibility)
        .unwrap();
    assert_incremental_ram(&child, &baseline, &baseline_hash);
    assert_eq!(child.manifest().version, 5);
    assert!(child.manifest().filesystem_blocks.is_some());
    assert!(
        !derived
            .store
            .join("objects")
            .join(&derived.snapshot_id)
            .join("rootfs")
            .exists()
    );
    let saved: serde_json::Value = serde_json::from_slice(&child.machine_bytes().unwrap()).unwrap();
    assert_eq!(
        saved["filesystem_layers"].as_array().unwrap().len(),
        baseline_lowers.len()
    );
    for (id, path, probe, inode) in &baseline_lowers {
        assert!(
            child
                .manifest()
                .filesystem_layers
                .iter()
                .any(|layer| &layer.id == id && &layer.path == path)
        );
        let logical = Path::new(std::ffi::OsStr::from_bytes(path));
        let actual = child.owned_layer_path(logical).unwrap();
        assert_eq!(
            fs::metadata(actual.join(std::ffi::OsStr::from_bytes(probe)))
                .unwrap()
                .ino(),
            *inode,
            "native recapture copied an immutable lower"
        );
        assert!(
            !derived
                .store
                .join("objects")
                .join(&derived.snapshot_id)
                .join("rootfs")
                .join(logical)
                .exists()
        );
        assert!(
            saved["filesystem_layers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|layer| layer["id"] == *id)
        );
    }
    eprintln!(
        "native immutable filesystem: {} lowers retained with unchanged inodes, no private lower copies",
        baseline_lowers.len()
    );
    drop(child);
    fs::write(upper.join("env/allow-continue"), "continue\n").unwrap();
    let finished = finished_with_log(
        &admin,
        "remote-branch",
        Some(&root.join("target-worker.log")),
    )
    .await;
    assert_eq!(finished.phase, TaskPhase::Succeeded, "{finished:?}");
    let result = finished.result.as_ref().unwrap();
    assert_ne!(result.attempt_id, native_result.attempt_id);
    assert_eq!(
        result.output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint\n")
    );
    assert_eq!(finished.lease.unwrap().key.worker_id, "target-worker");
    let imported = root
        .join("target-worker/state/checkpoint-imports")
        .join(&publication.checkpoint.snapshot_id);
    assert_eq!(
        serde_json::from_slice::<CheckpointPublication>(
            &fs::read(imported.join("source.json")).unwrap()
        )
        .unwrap(),
        publication
    );
    let record =
        pvisor::RunRecord::read(&root.join("target-worker/state/tasks/remote-branch-1")).unwrap();
    assert_eq!(
        record.lineage.unwrap().checkpoint_id,
        publication.checkpoint.snapshot_id
    );
    let download = root.join("target-artifacts");
    admin
        .download_artifacts("remote-branch", &download)
        .await
        .unwrap();
    let bundle: pvisor::RunBundle =
        serde_json::from_slice(&fs::read(download.join("run-bundle.json")).unwrap()).unwrap();
    assert_eq!(
        bundle.executor_plan.unwrap().isolation,
        IsolationKind::VirtualMachine
    );
    assert_eq!(
        bundle.orchestration["pvisor.orchestration.checkpoint_publication"],
        serde_json::to_value(&publication).unwrap()
    );
    // Remove the complete imported store after its VM exits. The derived
    // snapshot must restore CPU/RAM/open FDs without any ancestor or cache.
    fs::remove_dir_all(&imported).unwrap();
    let mut grandchild = branch.clone();
    grandchild.id = "remote-derived-branch".into();
    grandchild.run.run_id = "remote-derived-branch".into();
    grandchild.run.parent_run_id = Some("remote-branch".into());
    grandchild.restore = Some(ExecutionRestore {
        task_id: "remote-branch".into(),
        request_id: "derive-after-import".into(),
    });
    admin.submit(&grandchild).await.unwrap();
    let grandchild_directory = root.join("target-worker/state/tasks/remote-derived-branch-1");
    let upper = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let task = admin.task("remote-derived-branch").await.unwrap();
            assert!(
                !task.phase.terminal(),
                "derived restore failed: {task:?}\n{}",
                fs::read_to_string(root.join("target-worker.log")).unwrap()
            );
            if let Ok(record) = pvisor::RunRecord::read(&grandchild_directory)
                && let Some(overlay) = record.overlay
                && overlay.upper.path().join("env/ready").exists()
            {
                break overlay.upper.path().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("self-contained incremental child restore deadline");
    assert!(fs::read(upper.join("env/seed")).unwrap().is_empty());
    fs::write(upper.join("env/allow-continue"), "continue\n").unwrap();
    let restored = finished_with_log(
        &admin,
        "remote-derived-branch",
        Some(&root.join("target-worker.log")),
    )
    .await;
    assert_eq!(restored.phase, TaskPhase::Succeeded, "{restored:?}");
    let result = restored.result.as_ref().unwrap();
    assert_ne!(result.attempt_id.as_str(), derived.source_attempt_id);
    assert_eq!(
        result.output.stdout.as_deref(),
        Some("memory-before-checkpoint|owned-before-checkpoint\n")
    );
    assert_eq!(
        pvisor::RunRecord::read(&grandchild_directory)
            .unwrap()
            .lineage
            .unwrap()
            .checkpoint_id,
        derived.snapshot_id
    );
    assert_eq!(
        remote.puts.load(Ordering::SeqCst),
        writes,
        "read-only target must not publish remotely"
    );
    assert!(
        !root
            .join("target-worker/unavailable-environments/rootfs-v3")
            .exists()
    );
    drop(target_worker);
    server.abort();
}
