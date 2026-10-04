//! Opt-in real Linux namespace and KVM/FUSE execution. No simulated VM result.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use pvisor::cache::{CacheBackend, CacheClient, CacheConfig, Response};
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::FileTypeExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

const ADMIN: &str = "vm-test-admin-0123456789012345";
const WORKER: &str = "vm-test-worker-0123456789012345";
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn copy_program(root: &Path, program: &str) {
    let copy = |path: &Path| {
        let target = root.join(path.strip_prefix("/").unwrap());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(path, target).unwrap();
    };
    copy(Path::new(program));
    let output = Command::new("ldd").arg(program).output().unwrap();
    assert!(
        output.status.success(),
        "ldd {program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for word in String::from_utf8(output.stdout).unwrap().split_whitespace() {
        if word.starts_with('/') {
            copy(Path::new(word));
        }
    }
}
fn publish_layer(
    source: &Path,
    cache: &Path,
    name: &str,
    files: &[(&str, &str)],
    base: bool,
) -> EnvironmentLayer {
    // Seed the native prepared-image fixture, then use the real cache publisher.
    // The generated small base contains actual host ELF programs/libraries.
    let manifest = format!("sha256:{}", sha(name.as_bytes()));
    let root = source.join("rootfs-v3/sha256").join(&manifest[7..]);
    fs::create_dir_all(&root).unwrap();
    if base {
        copy_program(&root, "/bin/sh");
        copy_program(&root, "/bin/sleep");
        for dir in ["tmp", "proc", "sys", "dev", "root", "etc"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
    }
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    let metadata = source.join("metadata/prepared-v1");
    fs::create_dir_all(&metadata).unwrap();
    let key = serde_json::to_vec(&[
        "registry-1.docker.io",
        &format!("library/{name}"),
        "test",
        "amd64",
    ])
    .unwrap();
    fs::write(
        metadata.join(format!("{}.json", sha(&key))),
        serde_json::to_vec(&serde_json::json!({
            "checked_at": pvisor_core::unix_now_ms()/1000,
            "prepared": {"digest": manifest, "env": {}, "entrypoint": [], "cmd": []}
        }))
        .unwrap(),
    )
    .unwrap();
    let client = CacheClient::from_config(CacheConfig {
        backend: CacheBackend::Filesystem,
        location: cache.display().to_string(),
        read_only: false,
        image_store: Some(source.to_owned()),
    })
    .unwrap();
    let Response::Prepared {
        image_handle: Some(handle),
        digest,
        ..
    } = client
        .publish(&format!("{name}:test"), "amd64", false)
        .unwrap()
    else {
        panic!("missing native revision handle")
    };
    EnvironmentLayer {
        handle,
        manifest_digest: digest,
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
    .unwrap()
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

async fn controlled(
    client: &Client,
    id: &str,
    request_id: &str,
    action: ControlAction,
) -> ControlRecord {
    let request = ControlRequest {
        request_id: request_id.into(),
        action,
    };
    client.control(id, &request).await.unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = client.task(id).await.unwrap();
            let record = task
                .controls
                .iter()
                .find(|c| c.command.request == request)
                .unwrap();
            if record.phase.terminal() {
                assert_eq!(record.phase, ControlPhase::Succeeded, "{task:?}");
                break record.clone();
            }
            assert!(!task.phase.terminal(), "{task:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("remote native control deadline");
    assert_eq!(client.control(id, &request).await.unwrap(), observed);
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
        Command::new(env!("CARGO_BIN_EXE_pvisor-worker"))
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
    let (first, second) = tokio::join!(finished(&admin, "first"), finished(&admin, "second"));
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
    let fresh = finished(&admin, "fresh").await;
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
    let paused = controlled(&admin, "controlled", "pause", ControlAction::Pause).await;
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
    let offloaded = controlled(&admin, "controlled", "offload", ControlAction::Offload).await;
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
        finished(&admin, "competitor").await.phase,
        TaskPhase::Cancelled
    );
    let resumed = controlled(&admin, "controlled", "resume", ControlAction::Resume).await;
    assert!(matches!(
        resumed.outcome,
        Some(ControlOutcome::Succeeded {
            state: pvisor_core::VmState::Running,
            memory: None
        })
    ));
    let complete = finished(&admin, "controlled").await;
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
    process.cwd = Some(root.display().to_string());
    let uid = unsafe { libc::getuid() };
    process.args = vec![
        "-c".into(),
        format!(
            "set -eu; read -r inside outside count < /proc/self/uid_map; test \"$inside\" = '{uid}'; test \"$outside\" = '{uid}'; test \"$count\" = 1; test -z \"${{PVISOR_CLUSTER_WORKER_TOKEN:-}}\"; printf 'rootless-ready\\n'"
        ),
    ];
    run.runtime.max_output_bytes = 1024;
    run.runtime.timeout_ms = Some(10_000);
    admin
        .submit(&TaskSpec {
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
        Command::new(env!("CARGO_BIN_EXE_pvisor-worker"))
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
