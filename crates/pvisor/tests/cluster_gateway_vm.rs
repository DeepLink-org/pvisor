//! Actual immutable Python/scaffold layers, KVM guests and model/tool execution.
#![cfg(all(feature = "gateway", target_os = "linux", target_arch = "x86_64"))]
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
#[path = "common/agent_fixture.rs"]
mod agent_fixture;
use agent_fixture::{python_layer, task};
#[path = "common/controller_process.rs"]
mod controller_process;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE, Python3 and firmware; run just test-cluster-vm-gateway"]
async fn concurrent_native_agent_model_tool_loops_keep_credentials_trace_and_workspaces_private() {
    native_agent_gate(false, Restart::None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE, Python3 and firmware; run just test-cluster-vm-gateway"]
async fn cooperative_model_wait_releases_cpu_and_preserves_manual_pause_before_delivery() {
    native_agent_gate(true, Restart::None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE, Python3 and firmware; run just test-cluster-vm-gateway"]
async fn cooperative_model_wait_survives_controller_restart_with_same_native_execution() {
    native_agent_gate(true, Restart::Server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux KVM/FUSE, Python3, firmware and controller binary; run just test-cluster-vm-gateway"]
async fn cooperative_model_wait_survives_controller_sigkill_with_same_native_execution() {
    native_agent_gate(true, Restart::Process).await;
}

#[derive(Clone, Copy, PartialEq)]
enum Restart {
    None,
    Server,
    Process,
}

async fn native_agent_gate(idle: bool, restart: Restart) {
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
                    &if idle {
                        include_str!("fixtures/cluster_agent_loop.py").replace(
                            "headers={\"Content-Type\":",
                            "headers={\"x-pvisor-inference-idle\": \"true\", \"Content-Type\":",
                        )
                    } else {
                        include_str!("fixtures/cluster_agent_loop.py").to_owned()
                    },
                )],
                false,
            );
            (base, python, scaffold)
        }
    })
    .await
    .unwrap();
    let model = model_service::ModelService::start_held().await;
    let scheduler_config = SchedulerConfig {
        lease_duration_ms: 3000,
        artifact_storage_limits: Some(ArtifactStorageLimits {
            version: CLUSTER_VERSION,
            max_bytes: Some(128 * 1024 * 1024),
            max_objects: Some(128),
        }),
        ..Default::default()
    };
    let scheduler = Scheduler::open(&root.join("journal"), scheduler_config.clone()).unwrap();
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
    fn controller_router(
        scheduler: Scheduler,
        artifact_gate: tokio::sync::watch::Receiver<bool>,
    ) -> axum::Router {
        gate_artifacts(
            pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into()).unwrap(),
            artifact_gate,
        )
    }
    fn gate_artifacts(
        router: axum::Router,
        artifact_gate: tokio::sync::watch::Receiver<bool>,
    ) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(
            artifact_gate,
            hold_artifact,
        ))
    }
    let (artifact_release, artifact_gate) = tokio::sync::watch::channel(false);
    let (mut controller_process, mut ready_failure, router) = if restart == Restart::Process {
        drop(scheduler);
        let controller = controller_process::Controller::start(
            root,
            Path::new(env!("CARGO_BIN_EXE_pvisor-worker")),
            scheduler_config.lease_duration_ms,
            scheduler_config.artifact_storage_limits.as_ref().unwrap(),
            ADMIN,
            WORKER,
        )
        .await;
        let (proxy, failure) = controller.proxy();
        let router = gate_artifacts(proxy, artifact_gate.clone());
        (Some(controller), Some(failure), router)
    } else {
        (
            None,
            None,
            controller_router(scheduler, artifact_gate.clone()),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}");
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let mut server_stop = Some(stop);
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = shutdown.await;
            })
            .await
            .unwrap();
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
    fs::write(&profile, format!("[environments]\nenabled = true\n[memory_sampling]\nenabled = true\ninterval_ms = 1000\n[vm]\nlibrary_dir = {}\n[overlaynet]\nmode = 'auto'\n[gateway]\nenabled = true\nrelease_cpu_on_idle = {idle}\nlevel = 'dialogue'\n[[gateway.routes]]\nname = '*'\nupstream = {}\napi_key_env = 'PVISOR_TEST_MODEL_KEY'\n", serde_json::to_string(&firmware.to_string_lossy()).unwrap(), serde_json::to_string(&model.base_url).unwrap())).unwrap();
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
                if idle { "3" } else { "2" },
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
                && (!idle || (a.phase == TaskPhase::Paused && b.phase == TaskPhase::Paused))
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
    if idle {
        fn vcpu_ticks(pid: u32) -> BTreeMap<String, u64> {
            fs::read_dir(format!("/proc/{pid}/task"))
                .unwrap()
                .filter_map(|entry| {
                    let entry = entry.unwrap();
                    if !fs::read_to_string(entry.path().join("comm"))
                        .unwrap()
                        .contains("vcpu")
                    {
                        return None;
                    }
                    let stat = fs::read_to_string(entry.path().join("stat")).unwrap();
                    let fields = stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .split_whitespace()
                        .collect::<Vec<_>>();
                    Some((
                        entry.file_name().to_string_lossy().into_owned(),
                        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap(),
                    ))
                })
                .collect()
        }
        let frozen = native.iter().map(|u| vcpu_ticks(u.pid)).collect::<Vec<_>>();
        assert!(frozen.iter().all(|v| !v.is_empty()));
        assert_eq!(admin.workers().await.unwrap()[0].reserved.cpu_millis, 0);
        for id in ["agent-a", "agent-b"] {
            let wait = admin.inference_wait_record(id).await.unwrap().unwrap();
            assert!(wait.pause_revision > 0 && !wait.ready && !wait.interrupted);
        }
        assert_eq!(
            admin.workers().await.unwrap()[0].reserved.memory_bytes,
            512 * 1024 * 1024
        );
        let mut competitor = task("cpu-competitor", &environment.digest);
        competitor.gateway = None;
        competitor.retain_bundle = false;
        competitor.retain_artifacts = None;
        competitor.resources.cpu_millis = 2000;
        let RunInvocation::Process(process) = &mut competitor.run.invocation;
        process.program = "/bin/sh".into();
        process.args = vec!["-c".into(), "while :; do :; done".into()];
        admin.submit(&competitor).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let record = admin.task("cpu-competitor").await.unwrap();
                assert!(!record.phase.terminal(), "{record:?}");
                if record.phase == TaskPhase::Running
                    && record
                        .memory_sample
                        .as_ref()
                        .is_some_and(|s| s.report.sample.usage.is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("competitor admitted using released CPU");
        admin
            .control(
                "agent-a",
                &ControlRequest {
                    request_id: "human-idle-pause".into(),
                    action: ControlAction::Pause,
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if admin
                    .task("agent-a")
                    .await
                    .unwrap()
                    .controls
                    .last()
                    .is_some_and(|c| c.phase == ControlPhase::Succeeded)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        model.release();
        tokio::time::sleep(Duration::from_millis(300)).await;
        for (index, id) in ["agent-a", "agent-b"].iter().enumerate() {
            let record = admin.task(id).await.unwrap();
            assert_eq!(record.phase, TaskPhase::Paused);
            assert_eq!(record.current_reservation().cpu_millis, 0);
            assert_eq!(vcpu_ticks(native[index].pid), frozen[index]);
        }
        assert_eq!(
            model.calls.lock().unwrap().len(),
            2,
            "no guest tool/result request before resume admission"
        );
        let overridden = admin
            .inference_wait_record("agent-a")
            .await
            .unwrap()
            .unwrap();
        assert!(overridden.ready && overridden.interrupted && overridden.resume_revision.is_none());
        let waiting = admin
            .inference_wait_record("agent-b")
            .await
            .unwrap()
            .unwrap();
        assert!(waiting.ready && waiting.resume_revision.is_some());
        if restart != Restart::None {
            if let Some(failure) = &mut ready_failure {
                let uncertain = failure.committed().await;
                assert!(uncertain == overridden.key || uncertain == waiting.key);
            }
            let before = [
                admin.task("agent-a").await.unwrap(),
                admin.task("agent-b").await.unwrap(),
                admin.task("cpu-competitor").await.unwrap(),
            ];
            if let Some(controller) = &mut controller_process {
                controller.kill();
            } else {
                server_stop.take().unwrap().send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(1), &mut server)
                    .await
                    .expect("controller HTTP shutdown within Worker watchdog")
                    .unwrap();
            }
            let scheduler = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Ok(scheduler) =
                        Scheduler::open(&root.join("journal"), scheduler_config.clone())
                    {
                        break scheduler;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("old controller releases journal authority");
            assert_eq!(
                scheduler.inference_wait_record("agent-a").unwrap(),
                Some(overridden)
            );
            assert_eq!(
                scheduler.inference_wait_record("agent-b").unwrap(),
                Some(waiting)
            );
            for record in &before {
                let replayed = scheduler.task(&record.spec.id).unwrap();
                assert!(replayed.reconciliation_pending);
                assert_eq!(
                    replayed.lease.as_ref().unwrap().key,
                    record.lease.as_ref().unwrap().key
                );
                assert_eq!(replayed.current_reservation(), record.current_reservation());
            }
            if let Some(controller) = &mut controller_process {
                drop(scheduler);
                controller.restart().await;
                ready_failure.as_ref().unwrap().release_as_failure().await;
            } else {
                let router = controller_router(scheduler, artifact_gate.clone());
                let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                let (stop, shutdown) = tokio::sync::oneshot::channel();
                server_stop = Some(stop);
                server = tokio::spawn(async move {
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async {
                            let _ = shutdown.await;
                        })
                        .await
                        .unwrap();
                });
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let mut confirmed = true;
                    for record in &before {
                        let after = admin.task(&record.spec.id).await.unwrap();
                        assert_eq!(
                            after.lease.as_ref().unwrap().key,
                            record.lease.as_ref().unwrap().key
                        );
                        if !after.reconciliation_pending {
                            assert_eq!(after.phase, record.phase);
                        }
                        confirmed &= !after.reconciliation_pending;
                        let before_pid = record
                            .memory_sample
                            .as_ref()
                            .unwrap()
                            .report
                            .sample
                            .usage
                            .as_ref()
                            .unwrap()
                            .pid;
                        if let Some(usage) = after
                            .memory_sample
                            .as_ref()
                            .and_then(|s| s.report.sample.usage.as_ref())
                        {
                            assert_eq!(usage.pid, before_pid);
                        } else {
                            confirmed = false;
                        }
                    }
                    if confirmed {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("surviving Worker reconciles native executions");
            for (index, id) in ["agent-a", "agent-b"].iter().enumerate() {
                assert!(Path::new(&format!("/proc/{}", native[index].pid)).exists());
                assert_eq!(vcpu_ticks(native[index].pid), frozen[index]);
                assert_eq!(
                    admin
                        .task(id)
                        .await
                        .unwrap()
                        .current_reservation()
                        .cpu_millis,
                    0
                );
            }
            assert_eq!(model.calls.lock().unwrap().len(), 2);
        }
        admin.cancel("cpu-competitor").await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if admin.task("cpu-competitor").await.unwrap().phase == TaskPhase::Cancelled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            admin.task("agent-a").await.unwrap().phase,
            TaskPhase::Paused
        );
        admin
            .control(
                "agent-a",
                &ControlRequest {
                    request_id: "human-idle-resume".into(),
                    action: ControlAction::Resume,
                },
            )
            .await
            .unwrap();
    } else {
        model.release();
    }
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
    server_stop.take().unwrap().send(()).unwrap();
    server.abort();
}
