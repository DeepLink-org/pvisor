//! Actual immutable Python/scaffold layers, KVM guests and model/tool execution.
#![cfg(all(feature = "gateway", target_os = "linux", target_arch = "x86_64"))]
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
#[path = "common/model_service.rs"]
mod model_service;
#[path = "common/native_cache.rs"]
mod native_cache;
const ADMIN: &str = "native-agent-admin-0123456789012345";
const WORKER: &str = "native-agent-worker-0123456789012345";
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn python_layer(source: &Path, cache: &Path) -> EnvironmentLayer {
    fn copy_python_sources(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if ["site-packages", "__pycache__", "lib-dynload"]
                .iter()
                .any(|s| name == *s)
            {
                continue;
            }
            let metadata = fs::metadata(entry.path()).unwrap();
            if metadata.is_dir() {
                copy_python_sources(&entry.path(), &target.join(&name));
            } else if metadata.is_file() && entry.path().extension().is_some_and(|e| e == "py") {
                fs::copy(entry.path(), target.join(&name)).unwrap();
            }
        }
    }
    let output = Command::new("/usr/bin/python3").args(["-c", "import urllib.request,json,subprocess,pathlib,sys,sysconfig,encodings.idna; print(json.dumps({'stdlib':sysconfig.get_path('stdlib'),'extensions':sorted({m.__file__ for m in sys.modules.values() if (getattr(m,'__file__','') or '').endswith('.so')})}))"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    native_cache::publish_layer_prepared(source, cache, "agent-python", &[], false, |root| {
        native_cache::copy_program(root, "/usr/bin/python3");
        let stdlib = Path::new(paths["stdlib"].as_str().unwrap());
        copy_python_sources(stdlib, &root.join(stdlib.strip_prefix("/").unwrap()));
        for extension in paths["extensions"].as_array().unwrap() {
            native_cache::copy_program(root, extension.as_str().unwrap());
        }
    })
}

fn task(id: &str, environment: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "test-scaffold", "/usr/bin/python3");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.args = vec!["/toolkit/agent.py".into()];
    process.cwd = Some("/env".into());
    process.inherit_env = false;
    process
        .env
        .insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    process
        .env
        .insert("PVISOR_TEST_BINARY_ARTIFACT".into(), "1".into());
    run.runtime.timeout_ms = Some(30_000);
    run.runtime.max_output_bytes = 8192;
    run.capabilities.models = vec!["test-model".into()];
    TaskSpec {
        retain_artifacts: Some(ArtifactRetention {
            execution_checkpoint: None,
            version: ARTIFACT_EXPORT_VERSION,
            trace: true,
            workspace_upper: true,
        }),
        gateway: Some(GatewayRequirement {
            version: CLUSTER_VERSION,
            level: pvisor_core::gateway::CaptureLevel::Dialogue,
            models: vec!["test-model".into()],
        }),
        cpu_qos: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "agents".into(),
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
        restore: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE, Python3 and firmware; run just test-cluster-vm-gateway"]
async fn concurrent_native_agent_model_tool_loops_keep_credentials_trace_and_workspaces_private() {
    use std::os::unix::fs::FileTypeExt;
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
    let source = root.join("source");
    let cache = root.join("cache");
    let (base, python, scaffold) = tokio::task::spawn_blocking({
        let source = source.clone();
        let cache = cache.clone();
        move || {
            let base = native_cache::publish_layer(
                &source,
                &cache,
                "agent-base",
                &[("env/input", "Implement multiply(a,b).\n")],
                true,
            );
            let python = python_layer(&source, &cache);
            let scaffold = native_cache::publish_layer(
                &source,
                &cache,
                "agent-scaffold",
                &[(
                    "toolkit/agent.py",
                    include_str!("fixtures/cluster_agent_loop.py"),
                )],
                false,
            );
            (base, python, scaffold)
        }
    })
    .await
    .unwrap();
    let model = model_service::ModelService::start_held().await;
    let scheduler = Scheduler::open(
        &root.join("journal"),
        SchedulerConfig {
            lease_duration_ms: 3000,
            artifact_storage_limits: Some(ArtifactStorageLimits {
                version: CLUSTER_VERSION,
                max_bytes: Some(128 * 1024 * 1024),
                max_objects: Some(128),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    async fn hold_artifact(
        axum::extract::State(mut release): axum::extract::State<tokio::sync::watch::Receiver<bool>>,
        request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        if request.uri().path().starts_with("/v1/workers/artifacts/") {
            release.wait_for(|released| *released).await.unwrap();
        }
        next.run(request).await
    }
    let (artifact_release, artifact_gate) = tokio::sync::watch::channel(false);
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            artifact_gate,
            hold_artifact,
        ));
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
            base: base.clone(),
            workspace: None,
            toolkits: vec![python, scaffold],
        })
        .await
        .unwrap();
    let firmware =
        std::env::var_os("PVISOR_TEST_LIBKRUNFW_DIR").expect("regular firmware directory required");
    assert!(
        fs::symlink_metadata(Path::new(&firmware).join("libkrunfw.so.5"))
            .unwrap()
            .is_file()
    );
    let profile = root.join("worker.toml");
    fs::write(&profile, format!("[environments]\nenabled = true\n[memory_sampling]\nenabled = true\ninterval_ms = 1000\n[vm]\nlibrary_dir = {}\n[overlaynet]\nmode = 'auto'\n[gateway]\nenabled = true\nlevel = 'dialogue'\n[[gateway.routes]]\nname = '*'\nupstream = {}\napi_key_env = 'PVISOR_TEST_MODEL_KEY'\n", serde_json::to_string(&firmware.to_string_lossy()).unwrap(), serde_json::to_string(&model.base_url).unwrap())).unwrap();
    let binary = root.join("pvisor-worker");
    fs::copy(env!("CARGO_BIN_EXE_pvisor-worker"), &binary).unwrap();
    let worker_log = root.join("worker.log");
    let worker = ChildGuard(
        Command::new(binary)
            .args([
                "--url",
                &url,
                "--id",
                "agent-vm-worker",
                "--backend",
                "vm",
                "--poll-ms",
                "50",
                "--slots",
                "2",
                "--cpu-millis",
                "2000",
            ])
            .arg("--state")
            .arg("worker")
            .current_dir(root)
            .arg("--config")
            .arg(&profile)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CLUSTER_TOKEN", ADMIN)
            .env("PVISOR_TEST_MODEL_KEY", model_service::KEY)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .env("XDG_CACHE_HOME", root.join("local-cache"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&worker_log).unwrap()))
            .spawn()
            .unwrap(),
    );
    for id in ["agent-a", "agent-b"] {
        admin.submit(&task(id, &environment.digest)).await.unwrap();
    }
    // Hold model replies while proving both requests originate from distinct
    // live native VMMs, including bound RAM and actual vCPU threads.
    let native = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a = admin.task("agent-a").await.unwrap();
            let b = admin.task("agent-b").await.unwrap();
            assert!(
                !a.phase.terminal() && !b.phase.terminal(),
                "native agent failed before model reply: {a:?} {b:?}\n{}",
                fs::read_to_string(&worker_log).unwrap()
            );
            let usages = [&a, &b]
                .into_iter()
                .map(|t| {
                    t.memory_sample
                        .as_ref()
                        .and_then(|s| s.report.sample.usage.clone())
                })
                .collect::<Option<Vec<_>>>();
            if model.calls.lock().unwrap().len() == 2
                && let Some(usages) = usages
            {
                assert_ne!(usages[0].pid, usages[1].pid);
                for usage in &usages {
                    assert!(usage.guest_ram.rss_bytes > 0);
                    assert!(
                        fs::read_dir(format!("/proc/{}/task", usage.pid))
                            .unwrap()
                            .any(
                                |entry| fs::read_to_string(entry.unwrap().path().join("comm"))
                                    .unwrap()
                                    .contains("vcpu")
                            )
                    );
                    let children = fs::read_dir(format!("/proc/{}/task", worker.0.id()))
                        .unwrap()
                        .filter_map(|entry| {
                            fs::read_to_string(entry.ok()?.path().join("children")).ok()
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert_eq!(
                        children
                            .split_whitespace()
                            .filter(|p| *p == usage.pid.to_string())
                            .count(),
                        1
                    );
                }
                break usages;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("native agent request/identity deadline");
    model.release();
    let staged = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a = admin.task("agent-a").await.unwrap();
            let b = admin.task("agent-b").await.unwrap();
            if a.phase == TaskPhase::RetainingArtifacts && b.phase == TaskPhase::RetainingArtifacts
            {
                break [a, b];
            }
            assert!(
                !a.phase.terminal() && !b.phase.terminal(),
                "native delivery ended before hold"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let charge = artifact_delivery_reservation(staged[0].spec.resources).unwrap();
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        charge.checked_add(charge).unwrap()
    );
    for (index, record) in staged.iter().enumerate() {
        assert!(record.artifacts.is_none() && record.result.is_some());
        assert!(!Path::new(&format!("/proc/{}", native[index].pid)).exists());
        assert!(
            root.join(format!(
                "worker/tasks/{}-1/retained/manifest.json",
                record.spec.id
            ))
            .exists()
        );
    }
    // Both execution slots were occupied above. A third actual model/tool VM
    // now completes while both predecessor artifact deliveries remain held.
    let mut replacement = task("agent-c", &environment.digest);
    replacement.retain_bundle = false;
    replacement.retain_artifacts = None;
    admin.submit(&replacement).await.unwrap();
    let replacement = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let record = admin.task("agent-c").await.unwrap();
            if record.phase.terminal() {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(replacement.phase, TaskPhase::Succeeded);
    assert_eq!(
        replacement.result.unwrap().output.stdout.as_deref(),
        Some("agent loop completed: 3 tests passed; unauthorized model denied\n")
    );
    let replacement_bundle: pvisor::RunBundle = serde_json::from_slice(
        &fs::read(root.join("worker/tasks/agent-c-1/run-bundle.json")).unwrap(),
    )
    .unwrap();
    let replacement_plan = replacement_bundle.executor_plan.as_ref().unwrap();
    assert_eq!(replacement_plan.kind, ExecutorKind::VirtualMachine);
    assert_eq!(replacement_plan.isolation, IsolationKind::VirtualMachine);
    tokio::time::sleep(Duration::from_millis(3100)).await;
    for record in &staged {
        let renewed = admin.task(&record.spec.id).await.unwrap();
        assert_eq!(renewed.phase, TaskPhase::RetainingArtifacts);
        assert!(
            renewed.lease.unwrap().expires_at_ms > record.lease.as_ref().unwrap().expires_at_ms
        );
    }
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        charge.checked_add(charge).unwrap()
    );
    let gc = admin
        .plan_artifact_gc(&pvisor_cluster::ArtifactGcRequest {
            version: CLUSTER_VERSION,
            retire_before_ms: None,
            max_objects: 4096,
        })
        .await
        .unwrap();
    assert!(gc.retire.is_empty());
    assert_eq!(
        admin
            .apply_artifact_gc(&gc.id)
            .await
            .unwrap()
            .deleted_objects,
        0
    );
    artifact_release.send_replace(true);
    let mut outcomes = vec![];
    for id in ["agent-a", "agent-b"] {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let t = admin.task(id).await.unwrap();
                if t.phase.terminal() {
                    break t;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            result.phase,
            TaskPhase::Succeeded,
            "{result:?}\n{}",
            fs::read_to_string(&worker_log).unwrap()
        );
        assert_eq!(
            result.result.as_ref().unwrap().output.stdout.as_deref(),
            Some("agent loop completed: 3 tests passed; unauthorized model denied\n")
        );
        assert_eq!(
            fs::read_to_string(root.join(format!("worker/tasks/{id}-1/upper/env/answer.py")))
                .unwrap(),
            "def multiply(a, b):\n    return a * b\n"
        );
        let destination = root.join(format!("download-{id}"));
        let manifest = admin.download_artifacts(id, &destination).await.unwrap();
        assert!(
            manifest
                .files
                .iter()
                .find(|f| f.name == "workspace-upper.tar")
                .unwrap()
                .chunks
                .len()
                >= 3
        );
        let trace = pvisor::trace::Journal::read(&destination.join("trace")).unwrap();
        let serialized = serde_json::to_string(&trace).unwrap();
        assert!(serialized.contains("write_and_test") && !serialized.contains(model_service::KEY));
        let mut archive =
            tar::Archive::new(fs::File::open(destination.join("workspace-upper.tar")).unwrap());
        let mut answer = None;
        let mut binary = None;
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() == Path::new("upper/env/answer.py") {
                use std::io::Read;
                let mut contents = String::new();
                entry.read_to_string(&mut contents).unwrap();
                answer = Some(contents);
            }
            if entry.path().unwrap() == Path::new("upper/env/binary-result") {
                use std::io::Read;
                let mut bytes = vec![];
                entry.read_to_end(&mut bytes).unwrap();
                binary = Some(bytes);
            }
        }
        assert_eq!(
            answer.as_deref(),
            Some("def multiply(a, b):\n    return a * b\n")
        );
        assert_eq!(
            binary.unwrap(),
            (0..8193).flat_map(|_| 0..=255_u8).collect::<Vec<_>>()
        );
        let bundle: pvisor::RunBundle =
            serde_json::from_slice(&fs::read(destination.join("run-bundle.json")).unwrap())
                .unwrap();
        assert!(!bundle.safety.host_process);
        assert_eq!(
            bundle.executor_plan.unwrap().isolation,
            IsolationKind::VirtualMachine
        );
        assert_eq!(bundle.run.run_id, id);
        assert_eq!(
            bundle.run.attempt_id,
            result.result.as_ref().unwrap().attempt_id.as_str()
        );
        outcomes.push(result);
    }
    assert_eq!(model.calls.lock().unwrap().len(), 6);
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources::default()
    );
    let storage_usage = admin.artifact_storage().await.unwrap();
    assert!(storage_usage.stored_bytes > 0 && storage_usage.stored_bytes <= 128 * 1024 * 1024);
    assert!(storage_usage.stored_objects > 0 && storage_usage.stored_objects <= 128);
    assert_eq!(storage_usage.limits.max_bytes, Some(128 * 1024 * 1024));
    assert_eq!(
        (
            storage_usage.reserved_bytes,
            storage_usage.reserved_objects,
            storage_usage.failed_reserved_bytes,
            storage_usage.failed_reserved_objects
        ),
        (0, 0, 0, 0)
    );
    let retirement = admin
        .plan_artifact_gc(&pvisor_cluster::ArtifactGcRequest {
            version: CLUSTER_VERSION,
            retire_before_ms: Some(
                outcomes
                    .iter()
                    .map(|task| task.updated_at_ms)
                    .max()
                    .unwrap()
                    + 1,
            ),
            max_objects: 4096,
        })
        .await
        .unwrap();
    assert_eq!(retirement.retire.len(), 2);
    let retired = admin.apply_artifact_gc(&retirement.id).await.unwrap();
    assert_eq!(retired.deleted_objects, storage_usage.stored_objects);
    assert_eq!(retired.deleted_bytes, storage_usage.stored_bytes);
    assert_eq!(admin.artifact_storage().await.unwrap().stored_objects, 0);
    for id in ["agent-a", "agent-b"] {
        let task = admin.task(id).await.unwrap();
        assert_eq!(task.phase, TaskPhase::Succeeded);
        assert!(task.artifact_retired_at_ms.is_some());
    }
    drop(worker);
    let mut listeners = vec![];
    for (index, id) in ["agent-a", "agent-b"].into_iter().enumerate() {
        assert!(!Path::new(&format!("/proc/{}", native[index].pid)).exists());
        let storage = root.join(format!("worker/tasks/{id}-1"));
        let records = pvisor::trace::Journal::read(&storage.join("trace")).unwrap();
        let trace = serde_json::to_string(&records).unwrap();
        assert!(trace.contains("write_and_test") && trace.contains("test-model"));
        assert!(!trace.contains(model_service::KEY));
        let bundle: pvisor::RunBundle =
            serde_json::from_slice(&fs::read(storage.join("run-bundle.json")).unwrap()).unwrap();
        assert_eq!(
            bundle.orchestration["pvisor.orchestration.gateway"],
            serde_json::to_value(outcomes[index].spec.gateway.as_ref().unwrap()).unwrap()
        );
        let record = pvisor::RunRecord::read(&storage).unwrap();
        let listen = record.gateway_listen.unwrap();
        assert!(!listen.ends_with(":0"));
        assert!(std::net::TcpStream::connect(&listen).is_err());
        listeners.push(listen);
    }
    assert_ne!(listeners[0], listeners[1]);
    let base_root = source
        .join("rootfs-v3/sha256")
        .join(&base.manifest_digest[7..]);
    assert!(!base_root.join("env/answer.py").exists());
    server.abort();
}
