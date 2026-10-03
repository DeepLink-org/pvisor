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
async fn worker_final_admission_declines_without_starting_and_requeues_the_exact_task() {
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
        "[admission]\nmode = 'linux_pressure'\nmemory_reserve_bytes = 18446744073709551615\n",
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
    assert_eq!(
        admin.cancel("declined").await.unwrap().phase,
        TaskPhase::Cancelled
    );
    assert!(!marker.exists());
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
