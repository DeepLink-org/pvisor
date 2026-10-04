//! Real HTTP controller + independent worker processes + pVisor execution.
//! These exercise communication failure and cancellation, not VM fidelity.
use pvisor_cluster::{
    client::Client,
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{
    collections::BTreeMap,
    process::{Child, Command, Stdio},
    time::Duration,
};

const ADMIN: &str = "test-admin-token-0123456789";
const WORKER: &str = "test-worker-token-0123456789";
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn_worker(url: &str, id: &str, root: &std::path::Path) -> ChildGuard {
    spawn_worker_config(url, id, root, None)
}
fn spawn_worker_config(
    url: &str,
    id: &str,
    root: &std::path::Path,
    config: Option<&std::path::Path>,
) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor-worker"));
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    ChildGuard(
        command
            .args([
                "--url",
                url,
                "--id",
                id,
                "--backend",
                "host",
                "--poll-ms",
                "100",
                "--slots",
                "2",
            ])
            .arg("--state")
            .arg(root.join(id))
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CLUSTER_TOKEN", ADMIN)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_final_admission_declines_without_starting_and_another_node_completes_it() {
    use axum::{
        body::{Body, to_bytes},
        extract::{Request, State},
        middleware::Next,
        response::Response,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    // Adversarial test service deliberately ignores the first local budget.
    // The real worker must still reject it before any native side effect.
    async fn force_first_assignment(
        State(first): State<Arc<AtomicBool>>,
        request: Request,
        next: Next,
    ) -> Response {
        if request.uri().path() == "/v1/workers/poll" && first.swap(false, Ordering::SeqCst) {
            let (mut parts, body) = request.into_parts();
            let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
            let mut poll: PollRequest = serde_json::from_slice(&bytes).unwrap();
            poll.admission = None;
            poll.available = Resources {
                slots: 2,
                memory_bytes: 8 * 1024 * 1024 * 1024,
                cpu_millis: 4000,
            };
            parts.headers.remove(axum::http::header::CONTENT_LENGTH);
            return next
                .run(Request::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&poll).unwrap()),
                ))
                .await;
        }
        next.run(request).await
    }
    let temp = tempfile::tempdir().unwrap();
    let scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(AtomicBool::new(true)),
            force_first_assignment,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let marker = temp.path().join("must-not-run");
    admin
        .submit(&spec(
            "declined",
            &format!("printf started > '{}'", marker.display()),
        ))
        .await
        .unwrap();
    let profile = temp.path().join("pressure.toml");
    std::fs::write(
        &profile,
        "[admission]\nmode = 'linux_pressure'\nmemory_reserve_bytes = 9223372036854775807\n",
    )
    .unwrap();
    let _worker = spawn_worker_config(&url, "pressure", temp.path(), Some(&profile));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = admin.task("declined").await.unwrap();
            let workers = admin.workers().await.unwrap();
            if task.admission_rejections == 1
                && workers.first().and_then(|w| w.admission.as_ref()).is_some()
            {
                assert_eq!(task.phase, TaskPhase::Queued);
                assert_eq!(task.generation, 1);
                assert!(task.lease.is_none());
                let report = workers[0].admission.as_ref().unwrap();
                assert_eq!(report.mode, AdmissionMode::LinuxPressure);
                assert_eq!(report.available.memory_bytes, 0);
                assert_eq!(report.available.cpu_millis, 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let evidence = temp.path().join("pressure/tasks/declined-1");
    assert!(evidence.join("assignment.json").exists());
    assert!(evidence.join("admission-rejection.json").exists());
    assert!(!evidence.join("trace").exists());
    assert!(!marker.exists());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        admin.task("declined").await.unwrap().admission_rejections,
        1
    );
    let _healthy = spawn_worker(&url, "healthy", temp.path());
    let completed = wait(&admin, "declined", true).await;
    assert_eq!(completed.phase, TaskPhase::Succeeded);
    assert_eq!(completed.generation, 2);
    assert_eq!(completed.admission_rejections, 1);
    assert_eq!(completed.lease.unwrap().key.worker_id, "healthy");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "started");
    assert!(temp.path().join("healthy/tasks/declined-2/trace").exists());
    server.abort();
}
fn spec(id: &str, command: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "cluster-test", "/bin/sh");
    let RunInvocation::Process(p) = &mut run.invocation;
    p.args = vec!["-c".into(), command.into()];
    p.inherit_env = false;
    run.runtime.max_output_bytes = 4096;
    run.runtime.termination_grace_ms = 100;
    TaskSpec {
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "test".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
        },
        resources: Resources {
            slots: 1,
            memory_bytes: 64 * 1024 * 1024,
            cpu_millis: 100,
        },
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: false,
        environment: None,
    }
}
async fn wait(client: &Client, id: &str, terminal: bool) -> TaskRecord {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = client.task(id).await.unwrap();
            if if terminal {
                task.phase.terminal()
            } else {
                task.phase == TaskPhase::Running
            } {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("task transition timed out")
}
async fn controller(root: &std::path::Path) -> (Client, String, tokio::task::JoinHandle<()>) {
    let s = Scheduler::open(
        &root.join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(s, ADMIN.into(), WORKER.into()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (Client::new(&url, ADMIN.into()).unwrap(), url, server)
}

async fn pending_record(root: &std::path::Path, ready: bool) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        'outer: loop {
            if let Ok(files) = std::fs::read_dir(root.join("outbox/pending")) {
                for file in files.flatten() {
                    // A complete JSON body in a .write-* file is not yet a
                    // published outbox record. Killing here would test a crash
                    // before commit rather than durable completion recovery.
                    if file.file_name().to_string_lossy().starts_with(".write-") {
                        continue;
                    }
                    if let Ok(bytes) = std::fs::read(file.path())
                        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && value["ready"] == ready
                    {
                        break 'outer value;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn reregistered(admin: &Client, id: &str, old: &str, root: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let workers = admin.workers().await.unwrap();
            if workers
                .iter()
                .any(|w| w.registration.id == id && w.registration.incarnation != old)
                && std::fs::read_dir(root.join("outbox/pending"))
                    .unwrap()
                    .all(|e| {
                        e.unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".write-")
                    })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("worker restart did not finish delivery/register");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_restart_replays_committed_completion_after_wrong_ack_without_reexecuting() {
    use axum::{
        body::to_bytes,
        extract::{Request, State},
        middleware::Next,
        response::{IntoResponse, Response},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    async fn corrupt_ack(
        State(corrupt): State<Arc<AtomicBool>>,
        request: Request,
        next: Next,
    ) -> Response {
        let complete = request.uri().path() == "/v1/workers/complete";
        let response = next.run(request).await;
        if complete && response.status().is_success() && corrupt.load(Ordering::SeqCst) {
            let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            let mut task: TaskRecord = serde_json::from_slice(&bytes).unwrap();
            task.lease.as_mut().unwrap().key.incarnation = "wrong-ack".into();
            return axum::Json(task).into_response();
        }
        response
    }
    let temp = tempfile::tempdir().unwrap();
    let corrupt = Arc::new(AtomicBool::new(true));
    let scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            corrupt.clone(),
            corrupt_ack,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let marker = temp.path().join("executions");
    let mut task = spec(
        "wrong-ack",
        &format!("printf 'once\\n' >> {}; printf result", marker.display()),
    );
    task.retain_bundle = true;
    admin.submit(&task).await.unwrap();
    let worker = spawn_worker(&url, "restart", temp.path());
    let done = wait(&admin, "wrong-ack", true).await;
    let state = temp.path().join("restart");
    let pending = pending_record(&state, true).await;
    assert_eq!(
        pending["completion"]["key"]["incarnation"],
        done.lease.as_ref().unwrap().key.incarnation
    );
    assert_eq!(
        std::fs::read_dir(state.join("outbox/receipts"))
            .unwrap()
            .count(),
        0
    );
    let original = std::fs::read(state.join("tasks/wrong-ack-1/run-bundle.json")).unwrap();
    drop(worker);
    // A terminal exact result remains replayable even after its old lease TTL.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    corrupt.store(false, Ordering::SeqCst);
    let _restarted = spawn_worker(&url, "restart", temp.path());
    reregistered(
        &admin,
        "restart",
        &done.lease.as_ref().unwrap().key.incarnation,
        &state,
    )
    .await;
    let recovered = admin.task("wrong-ack").await.unwrap();
    assert_eq!(recovered.phase, TaskPhase::Succeeded);
    assert_eq!(recovered.generation, 1);
    assert_eq!(
        serde_json::to_value(recovered.result).unwrap(),
        serde_json::to_value(done.result).unwrap()
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "once\n");
    assert_eq!(
        std::fs::read_dir(state.join("outbox/receipts"))
            .unwrap()
            .count(),
        1
    );
    let output = temp.path().join("download-recovered");
    admin
        .download_artifacts("wrong-ack", &output)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(output.join("run-bundle.json")).unwrap(),
        original
    );
    let second_incarnation = admin.workers().await.unwrap()[0]
        .registration
        .incarnation
        .clone();
    drop(_restarted);
    let _twice = spawn_worker(&url, "restart", temp.path());
    reregistered(&admin, "restart", &second_incarnation, &state).await;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("executions")).unwrap(),
        "once\n"
    );
    assert_eq!(
        std::fs::read_dir(state.join("outbox/receipts"))
            .unwrap()
            .count(),
        1
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_restart_finishes_interrupted_bundle_upload_and_renews_only_terminal_evidence() {
    use axum::{
        extract::{Request, State},
        middleware::Next,
        response::{IntoResponse, Response},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Faults {
        blocked: AtomicBool,
        uploads: AtomicUsize,
        recoveries: AtomicUsize,
    }
    async fn lost_upload_ack(
        State(faults): State<Arc<Faults>>,
        request: Request,
        next: Next,
    ) -> Response {
        let upload = request.uri().path().starts_with("/v1/workers/artifacts/");
        if request.uri().path() == "/v1/workers/recover" {
            faults.recoveries.fetch_add(1, Ordering::SeqCst);
        }
        let response = next.run(request).await;
        if upload && response.status().is_success() {
            let attempt = faults.uploads.fetch_add(1, Ordering::SeqCst);
            if faults.blocked.load(Ordering::SeqCst) || attempt < 6 {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
        response
    }
    let temp = tempfile::tempdir().unwrap();
    let faults = Arc::new(Faults {
        blocked: AtomicBool::new(true),
        uploads: AtomicUsize::new(0),
        recoveries: AtomicUsize::new(0),
    });
    let scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            faults.clone(),
            lost_upload_ack,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let marker = temp.path().join("executions");
    let mut task = spec(
        "upload-restart",
        &format!("printf 'once\\n' >> {}; printf completed", marker.display()),
    );
    task.retain_bundle = true;
    admin.submit(&task).await.unwrap();
    let worker = spawn_worker(&url, "restart-upload", temp.path());
    let state = temp.path().join("restart-upload");
    let pending = pending_record(&state, false).await;
    assert_eq!(pending["completion"]["result"]["state"], "completed");
    let old = pending["completion"]["key"]["incarnation"]
        .as_str()
        .unwrap();
    let attempt = pending["completion"]["result"]["attempt_id"].clone();
    let original = std::fs::read(state.join("tasks/upload-restart-1/run-bundle.json")).unwrap();
    drop(worker);
    faults.uploads.store(0, Ordering::SeqCst);
    faults.blocked.store(false, Ordering::SeqCst);
    let _restarted = spawn_worker(&url, "restart-upload", temp.path());
    let done = wait(&admin, "upload-restart", true).await;
    assert_eq!(done.phase, TaskPhase::Succeeded, "{done:?}");
    assert_eq!(done.generation, 1);
    assert_eq!(
        serde_json::to_value(done.result.as_ref().unwrap()).unwrap()["attempt_id"],
        attempt
    );
    assert!(done.artifacts.is_some());
    assert!(done.artifact_error.is_none());
    assert!(faults.recoveries.load(Ordering::SeqCst) >= 10);
    reregistered(&admin, "restart-upload", old, &state).await;
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "once\n");
    let output = temp.path().join("download-recovered");
    admin
        .download_artifacts("upload-restart", &output)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(output.join("run-bundle.json")).unwrap(),
        original
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_restart_fences_expired_uncommitted_completion_without_retrying_command() {
    use axum::{
        extract::{Request, State},
        middleware::Next,
        response::{IntoResponse, Response},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    async fn reject_completion(
        State(blocked): State<Arc<AtomicBool>>,
        request: Request,
        next: Next,
    ) -> Response {
        if request.uri().path() == "/v1/workers/complete" && blocked.load(Ordering::SeqCst) {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        next.run(request).await
    }
    let temp = tempfile::tempdir().unwrap();
    let blocked = Arc::new(AtomicBool::new(true));
    let scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            blocked.clone(),
            reject_completion,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let marker = temp.path().join("executions");
    admin
        .submit(&spec(
            "expired-restart",
            &format!("printf 'once\\n' >> {}; printf completed", marker.display()),
        ))
        .await
        .unwrap();
    let worker = spawn_worker(&url, "restart-expired", temp.path());
    let state = temp.path().join("restart-expired");
    let pending = pending_record(&state, true).await;
    drop(worker);
    let lost = wait(&admin, "expired-restart", true).await;
    assert_eq!(lost.phase, TaskPhase::Lost);
    blocked.store(false, Ordering::SeqCst);
    let _restarted = spawn_worker(&url, "restart-expired", temp.path());
    reregistered(
        &admin,
        "restart-expired",
        pending["completion"]["key"]["incarnation"]
            .as_str()
            .unwrap(),
        &state,
    )
    .await;
    assert_eq!(
        admin.task("expired-restart").await.unwrap().phase,
        TaskPhase::Lost
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "once\n");
    let receipt = std::fs::read_dir(state.join("outbox/receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt.path()).unwrap()).unwrap();
    assert_eq!(value["disposition"]["status"], "fenced");
    admin
        .submit(&spec("fresh-after-restart", "printf fresh"))
        .await
        .unwrap();
    assert_eq!(
        wait(&admin, "fresh-after-restart", true).await.phase,
        TaskPhase::Succeeded
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn environment_http_is_admin_owned_and_missing_revision_never_executes_on_host() {
    let temp = tempfile::tempdir().unwrap();
    let (admin, url, server) = controller(temp.path()).await;
    let worker_api = Client::new(&url, WORKER.into()).unwrap();
    let (architecture, platform) = match std::env::consts::ARCH {
        "x86_64" => ("amd64", "linux-amd64"),
        "aarch64" => ("arm64", "linux-arm64-v8"),
        other => panic!("unsupported VM test architecture: {other}"),
    };
    let template = EnvironmentTemplate {
        version: CLUSTER_VERSION,
        architecture: architecture.into(),
        base: EnvironmentLayer {
            handle: format!("pvisor-v1:{}:{platform}:{}", "a".repeat(64), "b".repeat(64)),
            manifest_digest: format!("sha256:{}", "c".repeat(64)),
        },
        workspace: None,
        toolkits: vec![],
    };
    assert!(worker_api.publish_environment(&template).await.is_err());
    let record = admin.publish_environment(&template).await.unwrap();
    assert_eq!(admin.publish_environment(&template).await.unwrap(), record);
    assert_eq!(admin.environment(&record.digest).await.unwrap(), record);
    assert!(worker_api.environment(&record.digest).await.is_err());
    let mut tampered = record.clone();
    tampered.template.base.manifest_digest = format!("sha256:{}", "d".repeat(64));
    assert!(pvisor_cluster::environment::validate(&tampered).is_err());
    let marker = temp.path().join("must-not-execute");
    let mut task = spec(
        "missing-revision",
        &format!("printf escaped > '{}'", marker.display()),
    );
    task.execution = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    task.environment = Some(record.digest.clone());
    admin.submit(&task).await.unwrap();
    let profile = temp.path().join("environment.toml");
    std::fs::write(&profile, "[environments]\nenabled = true\nmax_layers = 8\n").unwrap();
    let cache = temp.path().join("empty-cache");
    std::fs::create_dir(&cache).unwrap();
    let _worker = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_pvisor-worker"))
            .args([
                "--url",
                &url,
                "--id",
                "environment",
                "--backend",
                "vm",
                "--poll-ms",
                "100",
                "--slots",
                "1",
            ])
            .arg("--config")
            .arg(&profile)
            .arg("--state")
            .arg(temp.path().join("environment"))
            .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
            .env("PVISOR_CACHE_BACKEND", "filesystem")
            .env("PVISOR_CACHE_LOCATION", &cache)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let record = wait(&admin, "missing-revision", true).await;
    assert_eq!(record.phase, TaskPhase::Failed);
    assert!(
        record.result.is_none(),
        "failure precedes native runtime.run"
    );
    assert!(record.error.is_some());
    assert!(!marker.exists());
    let assignment: Assignment = serde_json::from_slice(
        &std::fs::read(
            temp.path()
                .join("environment/tasks/missing-revision-1/assignment.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        assignment.environment.unwrap().digest,
        task.environment.unwrap()
    );
    assert!(
        !temp
            .path()
            .join("environment/tasks/missing-revision-1/run-bundle.json")
            .exists()
    );
    let workers = admin.workers().await.unwrap();
    assert_eq!(
        workers[0]
            .registration
            .environment_support
            .as_ref()
            .unwrap()
            .architecture,
        architecture
    );
    assert_eq!(workers[0].reserved, Resources::default());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_bundle_retries_lost_upload_ack_renews_lease_and_downloads_after_restart() {
    use axum::{
        extract::{Request, State},
        middleware::Next,
        response::{IntoResponse, Response},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    // Lose successful acknowledgements for longer than one lease interval.
    // The worker must renew while publishing and safely resend stored bytes.
    async fn lose_ack(
        State(attempts): State<Arc<AtomicUsize>>,
        request: Request,
        next: Next,
    ) -> Response {
        let artifact = request.uri().path().starts_with("/v1/workers/artifacts/");
        let response = next.run(request).await;
        if artifact && response.status().is_success() && attempts.fetch_add(1, Ordering::SeqCst) < 6
        {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        response
    }
    let temp = tempfile::tempdir().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 1500,
            ..Default::default()
        },
    )
    .unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            attempts.clone(),
            lose_ack,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let worker = spawn_worker(&url, "publisher", temp.path());
    let mut task = spec(
        "retained",
        "/bin/dd if=/dev/zero bs=800000 count=1 2>/dev/null | /usr/bin/tr '\\000' '\\377'",
    );
    task.run.runtime.max_output_bytes = ARTIFACT_CHUNK_BYTES;
    task.retain_bundle = true;
    admin.submit(&task).await.unwrap();
    let started = wait(&admin, "retained", false).await;
    let finished = wait(&admin, "retained", true).await;
    assert_eq!(finished.phase, TaskPhase::Succeeded);
    assert!(finished.artifact_error.is_none());
    assert!(attempts.load(Ordering::SeqCst) >= 9);
    assert!(finished.lease.as_ref().unwrap().expires_at_ms > started.lease.unwrap().expires_at_ms);
    let result = finished.result.as_ref().unwrap();
    assert!(result.output.stdout.as_ref().unwrap().len() <= ARTIFACT_CHUNK_BYTES);
    assert!(result.output.stdout_truncated);
    let manifest = admin.artifacts("retained").await.unwrap();
    assert_eq!(manifest.key, finished.lease.as_ref().unwrap().key);
    assert!(manifest.files[0].chunks.len() >= 3);
    let local = std::fs::read(
        temp.path()
            .join("publisher/tasks/retained-1/run-bundle.json"),
    )
    .unwrap();
    let out = temp.path().join("download");
    admin.download_artifacts("retained", &out).await.unwrap();
    assert_eq!(std::fs::read(out.join("run-bundle.json")).unwrap(), local);
    assert!(admin.download_artifacts("retained", &out).await.is_err());
    assert_eq!(std::fs::read(out.join("run-bundle.json")).unwrap(), local);
    let bundle: pvisor::RunBundle = serde_json::from_slice(&local).unwrap();
    assert_eq!(bundle.schema_version, pvisor::RUN_BUNDLE_SCHEMA_VERSION);
    assert_eq!(bundle.run.run_id, result.run_id.as_str());
    assert_eq!(bundle.run.attempt_id, result.attempt_id.as_str());
    assert_eq!(bundle.run.state, pvisor_core::RunState::Completed);
    assert!(bundle.run.output.stdout.unwrap().len() > ARTIFACT_CHUNK_BYTES);
    drop(worker);
    drop(admin);
    server.abort();
    let _ = server.await;
    // Wait only for old server connection/reaper ownership to be released.
    let reopened = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match Scheduler::open(&temp.path().join("journal"), SchedulerConfig::default()) {
                Ok(s) => break s,
                Err(error) if error.to_string().contains("already owned") => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("controller recovery failed: {error:#}"),
            }
        }
    })
    .await
    .unwrap();
    let router = pvisor_cluster::server::router(reopened, ADMIN.into(), WORKER.into()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let recovered = Client::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        ADMIN.into(),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    assert_eq!(
        recovered.task("retained").await.unwrap().artifacts,
        finished.artifacts
    );
    assert_eq!(
        recovered
            .download_artifacts("retained", &temp.path().join("recovered"))
            .await
            .unwrap(),
        manifest
    );
    assert_eq!(
        std::fs::read(temp.path().join("recovered/run-bundle.json")).unwrap(),
        local
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_http_requires_live_lease_exact_hash_bounded_body_and_separate_roles() {
    let temp = tempfile::tempdir().unwrap();
    let (admin, url, server) = controller(temp.path()).await;
    let worker = Client::new(&url, WORKER.into()).unwrap();
    let task = spec("wire-artifacts", "true");
    worker
        .register(&WorkerRegistration {
            version: CLUSTER_VERSION,
            id: "publisher".into(),
            incarnation: "epoch".into(),
            capacity: task.resources,
            execution: vec![task.execution.clone()],
            labels: BTreeMap::new(),
            cache_keys: vec![],
            vm_control_protocol: None,
            vm_control_actions: vec![],
            artifact_protocol: Some(CLUSTER_VERSION),
            environment_support: None,
        })
        .await
        .unwrap();
    admin.submit(&task).await.unwrap();
    let mut poll = PollRequest {
        worker_id: "publisher".into(),
        incarnation: "epoch".into(),
        active: vec![],
        available: task.resources,
        max_assignments: 1,
        admission: None,
    };
    let key = worker
        .poll(&poll)
        .await
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let bytes = b"evidence".to_vec();
    assert!(admin.upload_artifact(&key, bytes.clone()).await.is_err());
    let mut forged = key.clone();
    forged.generation += 1;
    assert!(
        worker
            .upload_artifact(&forged, bytes.clone())
            .await
            .is_err()
    );
    forged = key.clone();
    forged.incarnation = "other".into();
    assert!(
        worker
            .upload_artifact(&forged, bytes.clone())
            .await
            .is_err()
    );
    let reference = worker.upload_artifact(&key, bytes.clone()).await.unwrap();
    assert_eq!(
        worker.upload_artifact(&key, bytes.clone()).await.unwrap(),
        reference
    );
    assert_eq!(admin.artifact_bytes(&reference).await.unwrap(), bytes);
    assert!(worker.artifact_bytes(&reference).await.is_err());
    assert!(worker.artifacts("wire-artifacts").await.is_err());
    let path = format!(
        "{url}/v1/workers/artifacts/{}/{}/{}/{}/{}",
        key.task_id, key.generation, key.worker_id, key.incarnation, reference.digest
    );
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(&path)
            .bearer_auth(WORKER)
            .body("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        http.post(&path)
            .bearer_auth(WORKER)
            .body(vec![0; ARTIFACT_CHUNK_BYTES + 1])
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE
    );
    worker
        .complete(&Completion {
            key: key.clone(),
            result: None,
            error: Some("stopped".into()),
            artifacts: None,
            artifact_error: None,
        })
        .await
        .unwrap();
    assert!(worker.upload_artifact(&key, bytes.clone()).await.is_err());
    admin
        .submit(&spec("expired-artifacts", "true"))
        .await
        .unwrap();
    poll.available = task.resources;
    let expired = worker
        .poll(&poll)
        .await
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(worker.upload_artifact(&expired, bytes).await.is_err());
    assert_eq!(
        admin.task("expired-artifacts").await.unwrap().phase,
        TaskPhase::Lost
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_failure_reports_native_completion_and_does_not_repeat_command() {
    use axum::{
        extract::Request,
        middleware::Next,
        response::{IntoResponse, Response},
    };
    async fn reject_upload(request: Request, next: Next) -> Response {
        if request.uri().path().starts_with("/v1/workers/artifacts/") {
            return axum::http::StatusCode::CONFLICT.into_response();
        }
        next.run(request).await
    }
    let temp = tempfile::tempdir().unwrap();
    let scheduler =
        Scheduler::open(&temp.path().join("journal"), SchedulerConfig::default()).unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn(reject_upload));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let _worker = spawn_worker(&url, "failed-export", temp.path());
    let marker = temp.path().join("side-effect");
    let mut task = spec(
        "failed-export",
        &format!("printf x >> '{}'", marker.display()),
    );
    task.retain_bundle = true;
    admin.submit(&task).await.unwrap();
    let record = wait(&admin, "failed-export", true).await;
    assert_eq!(record.phase, TaskPhase::Failed);
    assert_eq!(
        record.result.unwrap().state,
        pvisor_core::RunState::Completed
    );
    assert!(record.error.is_none());
    assert!(record.artifacts.is_none());
    assert!(record.artifact_error.is_some());
    assert!(
        temp.path()
            .join("failed-export/tasks/failed-export-1/run-bundle.json")
            .exists()
    );
    assert!(
        temp.path()
            .join("failed-export/tasks/failed-export-1/completion.json")
            .exists()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
    assert_eq!(admin.task("failed-export").await.unwrap().generation, 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_interrupts_upload_and_preserves_already_completed_native_result() {
    use axum::{
        extract::{Request, State},
        middleware::Next,
        response::Response,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct UploadGate {
        entered: AtomicBool,
        release: tokio::sync::Notify,
    }
    async fn delay_ack(
        State(gate): State<Arc<UploadGate>>,
        request: Request,
        next: Next,
    ) -> Response {
        let artifact = request.uri().path().starts_with("/v1/workers/artifacts/");
        let response = next.run(request).await;
        if artifact && response.status().is_success() {
            gate.entered.store(true, Ordering::SeqCst);
            gate.release.notified().await;
        }
        response
    }
    let temp = tempfile::tempdir().unwrap();
    let gate = Arc::new(UploadGate {
        entered: AtomicBool::new(false),
        release: tokio::sync::Notify::new(),
    });
    let scheduler =
        Scheduler::open(&temp.path().join("journal"), SchedulerConfig::default()).unwrap();
    let router = pvisor_cluster::server::router(scheduler, ADMIN.into(), WORKER.into())
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            gate.clone(),
            delay_ack,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let admin = Client::new(&url, ADMIN.into()).unwrap();
    let _worker = spawn_worker(&url, "cancel-export", temp.path());
    let mut task = spec("cancel-export", "printf completed");
    task.retain_bundle = true;
    admin.submit(&task).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    admin.cancel("cancel-export").await.unwrap();
    let record = tokio::time::timeout(Duration::from_secs(2), wait(&admin, "cancel-export", true))
        .await
        .unwrap();
    assert_eq!(record.phase, TaskPhase::Cancelled);
    let result = record.result.unwrap();
    assert_eq!(result.state, pvisor_core::RunState::Completed);
    assert_eq!(result.output.stdout.as_deref(), Some("completed"));
    assert!(record.artifacts.is_none());
    assert!(record.artifact_error.unwrap().contains("cancellation"));
    gate.release.notify_one();
    let publisher = Client::new(&url, WORKER.into()).unwrap();
    assert!(
        publisher
            .upload_artifact(&record.lease.unwrap().key, b"late".to_vec())
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_execute_tasks_and_credentials_never_reach_the_agent() {
    let temp = tempfile::tempdir().unwrap();
    let (client, url, server) = controller(temp.path()).await;
    let _one = spawn_worker(&url, "one", temp.path());
    let _two = spawn_worker(&url, "two", temp.path());
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.workers().await.unwrap().len() != 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    for i in 0..6 {
        client.submit(&spec(&format!("task-{i}"),"sleep 0.2; printf '%s' \"${PVISOR_CLUSTER_TOKEN-unset}/${PVISOR_CLUSTER_WORKER_TOKEN-unset}\"")).await.unwrap();
    }
    let mut workers = std::collections::BTreeSet::new();
    for i in 0..6 {
        let task = wait(&client, &format!("task-{i}"), true).await;
        assert_eq!(task.phase, TaskPhase::Succeeded);
        workers.insert(task.lease.unwrap().key.worker_id);
        assert_eq!(
            task.result.unwrap().output.stdout.as_deref(),
            Some("unset/unset")
        );
    }
    assert_eq!(workers.len(), 2);
    assert!(
        client
            .workers()
            .await
            .unwrap()
            .iter()
            .all(|w| w.reserved.slots == 0)
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_stops_live_process_tree_and_preserves_result_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let (client, url, server) = controller(temp.path()).await;
    let _worker = spawn_worker(&url, "cancel", temp.path());
    client.submit(&spec("long", "sleep 30")).await.unwrap();
    wait(&client, "long", false).await;
    assert_eq!(
        client.cancel("long").await.unwrap().phase,
        TaskPhase::Cancelling
    );
    let task = wait(&client, "long", true).await;
    assert_eq!(task.phase, TaskPhase::Cancelled);
    assert_eq!(task.result.unwrap().state, pvisor_core::RunState::Cancelled);
    assert!(
        temp.path()
            .join("cancel/tasks/long-1/completion.json")
            .exists()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn controller_outage_triggers_worker_monotonic_lease_watchdog() {
    let temp = tempfile::tempdir().unwrap();
    let (client, url, server) = controller(temp.path()).await;
    let _worker = spawn_worker(&url, "outage", temp.path());
    client.submit(&spec("long", "sleep 30")).await.unwrap();
    wait(&client, "long", false).await;
    server.abort();
    let _ = server.await;
    let path = temp.path().join("outage/tasks/long-1/completion.json");
    let completion: Completion = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(data) = std::fs::read(&path)
                && let Ok(c) = serde_json::from_slice(&data)
            {
                break c;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("worker did not stop after controller outage");
    assert_eq!(
        completion.result.unwrap().state,
        pvisor_core::RunState::Cancelled
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_credentials_cannot_submit_or_read_tenant_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let (admin, url, server) = controller(temp.path()).await;
    let worker = Client::new(&url, WORKER.into()).unwrap();
    let missing = Client::new(&url, "incorrect".into()).unwrap();
    admin.submit(&spec("private", "true")).await.unwrap();
    assert!(worker.submit(&spec("forged", "true")).await.is_err());
    assert!(worker.task("private").await.is_err());
    assert!(missing.workers().await.is_err());
    let recovery = RecoveryRequest {
        worker_id: "recovery-role".into(),
        incarnation: "epoch".into(),
        completed: vec![],
    };
    for denied in [&admin, &missing] {
        let error = denied.recover(&recovery).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(reqwest::StatusCode::UNAUTHORIZED)
        );
    }
    worker
        .register(&WorkerRegistration {
            version: CLUSTER_VERSION,
            id: "recovery-role".into(),
            incarnation: "epoch".into(),
            capacity: Resources {
                slots: 1,
                memory_bytes: 64 * 1024 * 1024,
                cpu_millis: 100,
            },
            execution: vec![ExecutionClass {
                executor: ExecutorKind::Process,
                isolation: IsolationKind::HostProcess,
            }],
            labels: BTreeMap::new(),
            cache_keys: vec![],
            vm_control_protocol: None,
            vm_control_actions: vec![],
            artifact_protocol: None,
            environment_support: None,
        })
        .await
        .unwrap();
    let renewed = worker.recover(&recovery).await.unwrap();
    assert_eq!(renewed.version, CLUSTER_VERSION);
    assert!(renewed.renewed.is_empty() && renewed.stop.is_empty());
    assert_eq!(
        admin.task("private").await.unwrap().phase,
        TaskPhase::Queued
    );
    assert!(
        admin
            .poll(&PollRequest {
                worker_id: "w".into(),
                incarnation: "i".into(),
                active: vec![],
                available: Resources::default(),
                max_assignments: 1,
                admission: None,
            })
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_utf8_output_remains_bounded_and_deliverable() {
    let temp = tempfile::tempdir().unwrap();
    let (client, url, server) = controller(temp.path()).await;
    let _worker = spawn_worker(&url, "output", temp.path());
    let mut task = spec(
        "binary",
        "printf '\\377\\377\\377\\377\\377\\377\\377\\377'",
    );
    task.run.runtime.max_output_bytes = 7;
    client.submit(&task).await.unwrap();
    let task = wait(&client, "binary", true).await;
    assert_eq!(task.phase, TaskPhase::Succeeded);
    let output = task.result.unwrap().output;
    assert!(output.stdout.unwrap().len() <= 7);
    assert!(output.stdout_truncated);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn json_expanded_binary_output_fits_completion_body_and_retains_full_native_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let (client, url, server) = controller(temp.path()).await;
    let _worker = spawn_worker(&url, "binary-json", temp.path());
    let mut task = spec(
        "binary-json",
        "/bin/dd if=/dev/zero bs=1048576 count=1 2>/dev/null",
    );
    task.run.runtime.max_output_bytes = 1024 * 1024;
    task.retain_bundle = true;
    client.submit(&task).await.unwrap();
    let done = wait(&client, "binary-json", true).await;
    assert_eq!(done.phase, TaskPhase::Succeeded);
    let result = done.result.as_ref().unwrap();
    assert!(result.output.stdout_truncated);
    let stdout = result.output.stdout.as_ref().unwrap();
    assert!(stdout.bytes().all(|byte| byte == 0));
    assert!(!stdout.is_empty());
    assert!(serde_json::to_vec(stdout).unwrap().len() <= 1024 * 1024 + 2);
    assert!(serde_json::to_vec(result).unwrap().len() < 4 * 1024 * 1024);
    let output = temp.path().join("download-binary");
    client
        .download_artifacts("binary-json", &output)
        .await
        .unwrap();
    let bundle: pvisor::RunBundle =
        serde_json::from_slice(&std::fs::read(output.join("run-bundle.json")).unwrap()).unwrap();
    let captured = bundle.run.output.stdout.unwrap();
    assert_eq!(captured.len(), 1024 * 1024);
    assert!(captured.bytes().all(|byte| byte == 0));
    assert!(captured.len() > stdout.len());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_control_wire_protocol_enforces_roles_and_resume_admission() {
    // Synthetic worker observations verify the real HTTP protocol, not VM hardware.
    let temp = tempfile::tempdir().unwrap();
    let (admin, url, server) = controller(temp.path()).await;
    let worker = Client::new(&url, WORKER.into()).unwrap();
    let mut task = spec("vm-wire", "true");
    task.execution = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    let capacity = task.resources;
    worker
        .register(&WorkerRegistration {
            version: CLUSTER_VERSION,
            id: "wire".into(),
            incarnation: "epoch".into(),
            capacity,
            execution: vec![task.execution.clone()],
            labels: BTreeMap::new(),
            cache_keys: vec![],
            vm_control_protocol: Some(CLUSTER_VERSION),
            artifact_protocol: None,
            environment_support: None,
            vm_control_actions: vec![
                ControlAction::Pause,
                ControlAction::Offload,
                ControlAction::Resume,
            ],
        })
        .await
        .unwrap();
    admin.submit(&task).await.unwrap();
    let mut poll = PollRequest {
        worker_id: "wire".into(),
        incarnation: "epoch".into(),
        active: vec![],
        available: capacity,
        max_assignments: 1,
        admission: None,
    };
    let key = worker
        .poll(&poll)
        .await
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    poll.active.push(key);
    poll.available = Resources::default();
    poll.max_assignments = 0;
    let pause = ControlRequest {
        request_id: "pause".into(),
        action: ControlAction::Pause,
    };
    let unauthorized = worker.control("vm-wire", &pause).await.unwrap_err();
    assert_eq!(
        unauthorized
            .downcast_ref::<reqwest::Error>()
            .and_then(|e| e.status()),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    let requested = admin.control("vm-wire", &pause).await.unwrap();
    assert_eq!(admin.control("vm-wire", &pause).await.unwrap(), requested);
    let command = worker.poll(&poll).await.unwrap().controls.remove(0);
    let acknowledged = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Succeeded {
            state: pvisor_core::VmState::Paused,
            memory: None,
        },
    };
    let unauthorized = admin.acknowledge_control(&acknowledged).await.unwrap_err();
    assert_eq!(
        unauthorized
            .downcast_ref::<reqwest::Error>()
            .and_then(|e| e.status()),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    let observed = worker.acknowledge_control(&acknowledged).await.unwrap();
    assert_eq!(
        worker.acknowledge_control(&acknowledged).await.unwrap(),
        observed
    );
    assert_eq!(
        admin.task("vm-wire").await.unwrap().phase,
        TaskPhase::Paused
    );
    assert_eq!(
        admin.workers().await.unwrap()[0].reserved,
        Resources {
            cpu_millis: 0,
            ..capacity
        }
    );
    admin
        .control(
            "vm-wire",
            &ControlRequest {
                request_id: "resume".into(),
                action: ControlAction::Resume,
            },
        )
        .await
        .unwrap();
    assert!(worker.poll(&poll).await.unwrap().controls.is_empty());
    poll.available.cpu_millis = capacity.cpu_millis;
    let command = worker.poll(&poll).await.unwrap().controls.remove(0);
    assert_eq!(admin.workers().await.unwrap()[0].reserved, capacity);
    worker
        .acknowledge_control(&ControlAcknowledgement {
            command,
            outcome: ControlOutcome::Succeeded {
                state: pvisor_core::VmState::Running,
                memory: None,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        admin.task("vm-wire").await.unwrap().phase,
        TaskPhase::Running
    );
    server.abort();
}
