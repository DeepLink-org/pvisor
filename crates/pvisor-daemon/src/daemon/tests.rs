use super::store::CommitPoint;
use super::{Config, CreateRequest, Daemon};
use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
use serde_json::{Value, json};
use std::time::Duration;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::sync::{Barrier, oneshot};

const IO_TIMEOUT: Duration = Duration::from_secs(5);

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(IO_TIMEOUT, future)
        .await
        .expect("persistence test synchronization timed out")
}

// Drop releases the blocking worker even if an assertion unwinds the test.
struct DiskGate(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl DiskGate {
    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

impl Drop for DiskGate {
    fn drop(&mut self) {
        self.release();
    }
}

fn block_disk(daemon: &Daemon) -> (DiskGate, oneshot::Receiver<()>) {
    let gate = DiskGate(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
    let worker = gate.0.clone();
    let (entered, receiver) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    daemon.store.set_commit_hook(move |point| {
        if point == CommitPoint::BeforeDisk {
            let sender = entered.lock().unwrap().take();
            if let Some(sender) = sender {
                let _ = sender.send(());
                let (released, _) = worker
                    .1
                    .wait_timeout_while(worker.0.lock().unwrap(), IO_TIMEOUT, |released| !*released)
                    .unwrap();
                anyhow::ensure!(*released, "blocked commit was never released");
            }
        }
        Ok(())
    });
    (gate, receiver)
}

struct NativeCreateGate {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

const API_KEY: &str = "test-api-key-at-least-thirty-two-bytes-long";
const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Default)]
enum DeleteOutcome {
    #[default]
    Confirmed,
    Error,
    Unconfirmed,
}

#[derive(Default)]
struct FakeState {
    sandboxes: BTreeMap<String, RuntimeState>,
    specs: Vec<RuntimeSpec>,
    owners: Vec<String>,
    creates: usize,
    inspections: usize,
    pauses: usize,
    resumes: usize,
    deletes: usize,
    endpoints: usize,
    delete_outcome: DeleteOutcome,
}

#[derive(Default)]
struct FakeRuntime {
    state: Mutex<FakeState>,
    create_gate: Mutex<Option<NativeCreateGate>>,
}

#[async_trait::async_trait]
impl Runtime for FakeRuntime {
    async fn create(&self, spec: &RuntimeSpec) -> anyhow::Result<()> {
        let gate = self.create_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            tokio::time::timeout(IO_TIMEOUT, gate.release).await??;
        }
        let mut state = self.state.lock().unwrap();
        state.creates += 1;
        anyhow::ensure!(!state.sandboxes.contains_key(&spec.id), "duplicate sandbox");
        state.specs.push(spec.clone());
        state
            .sandboxes
            .insert(spec.id.clone(), RuntimeState::Running);
        Ok(())
    }

    async fn inspect(&self, id: &str) -> anyhow::Result<RuntimeState> {
        let mut state = self.state.lock().unwrap();
        state.inspections += 1;
        Ok(state
            .sandboxes
            .get(id)
            .copied()
            .unwrap_or(RuntimeState::Missing))
    }

    async fn pause(&self, id: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.pauses += 1;
        let sandbox = state
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
        anyhow::ensure!(*sandbox == RuntimeState::Running, "sandbox not running");
        *sandbox = RuntimeState::Paused;
        Ok(())
    }

    async fn resume(&self, id: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.resumes += 1;
        let sandbox = state
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
        anyhow::ensure!(*sandbox == RuntimeState::Paused, "sandbox not paused");
        *sandbox = RuntimeState::Running;
        Ok(())
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.deletes += 1;
        match state.delete_outcome {
            DeleteOutcome::Confirmed => {
                state.sandboxes.remove(id);
            }
            DeleteOutcome::Error => anyhow::bail!("native deletion outcome unknown"),
            // A successful command is not proof that the sandbox disappeared.
            DeleteOutcome::Unconfirmed => {}
        }
        Ok(())
    }

    async fn endpoint(&self, id: &str, port: u16) -> anyhow::Result<String> {
        let mut state = self.state.lock().unwrap();
        state.endpoints += 1;
        anyhow::ensure!(
            state.sandboxes.get(id) == Some(&RuntimeState::Running),
            "sandbox not running"
        );
        Ok(format!("http://127.0.0.1:{port}"))
    }
}

fn config(directory: &TempDir) -> Config {
    Config {
        state_dir: directory.path().to_path_buf(),
        api_key: API_KEY.to_owned(),
        public_endpoint: "localhost:8080".to_owned(),
        max_sandboxes: 2,
        cpu_millis: 2000,
        memory_bytes: 256 * MIB,
        max_timeout_seconds: 86400,
    }
}

async fn open(config: Config, runtime: &Arc<FakeRuntime>) -> Arc<Daemon> {
    let runtime = runtime.clone();
    Daemon::open(config, move |owner| {
        runtime.state.lock().unwrap().owners.push(owner);
        Ok(runtime as Arc<dyn Runtime>)
    })
    .await
    .expect("open private temporary daemon state")
}

fn request_json() -> Value {
    json!({
        "image": {"uri": "execd-fixture"},
        "entrypoint": ["tail", "-f", "/dev/null"],
        "resourceLimits": {"cpu": "1", "memory": "64Mi"},
        "timeout": 600,
        "metadata": {"test": "fake-runtime", "owner": "domain-tests"}
    })
}

fn request() -> CreateRequest {
    serde_json::from_value(request_json()).expect("valid OpenSandbox create fixture")
}

async fn create(daemon: &Arc<Daemon>) -> super::Sandbox {
    daemon
        .create(request())
        .await
        .unwrap_or_else(|error| panic!("create failed: {}", error.message))
}

#[tokio::test]
async fn persistent_sandbox_create_pause_resume_delete() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    assert_eq!(sandbox.status.state, "Running");
    assert_eq!(sandbox.entrypoint, ["tail", "-f", "/dev/null"]);
    assert_eq!(
        sandbox.metadata.get("test").map(String::as_str),
        Some("fake-runtime")
    );
    assert_eq!(
        sandbox.expires_at.unwrap() - sandbox.created_at,
        chrono::Duration::seconds(600)
    );
    {
        let state = runtime.state.lock().unwrap();
        assert_eq!(state.creates, 1);
        let spec = &state.specs[0];
        assert_eq!(spec.id, sandbox.id);
        assert_eq!(spec.image, "execd-fixture");
        assert_eq!(spec.entrypoint, sandbox.entrypoint);
        assert_eq!(spec.cpu_millis, 1000);
        assert_eq!(spec.memory_bytes, 64 * MIB);
    }

    daemon.pause(&sandbox.id).await.unwrap();
    assert_eq!(
        daemon.get(&sandbox.id).await.unwrap().status.state,
        "Paused"
    );
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    let paused = daemon.get(&sandbox.id).await.unwrap();
    assert_eq!(paused.status.state, "Paused");
    assert_eq!(paused.created_at, sandbox.created_at);
    assert_eq!(paused.expires_at, sandbox.expires_at);
    assert_eq!(paused.metadata, sandbox.metadata);

    daemon.resume(&sandbox.id).await.unwrap();
    assert_eq!(
        daemon.get(&sandbox.id).await.unwrap().status.state,
        "Running"
    );
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(daemon.list().await.unwrap().len(), 1);
    assert_eq!(
        daemon.get(&sandbox.id).await.unwrap().status.state,
        "Running"
    );
    daemon.delete(&sandbox.id).await.unwrap();
    assert!(daemon.list().await.unwrap().is_empty());
    assert_eq!(daemon.get(&sandbox.id).await.err().unwrap().status, 404);
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.list().await.unwrap().is_empty());
    let state = runtime.state.lock().unwrap();
    assert!(state.sandboxes.is_empty());
    assert_eq!(
        (state.creates, state.pauses, state.resumes, state.deletes),
        (1, 1, 1, 1)
    );
}

#[tokio::test]
async fn concurrent_creation_atomically_enforces_each_node_capacity() {
    // Isolate CPU, memory and slot admission as well as the normal node profile.
    for (cpu, memory, slots, admitted) in [
        (2000, 256 * MIB, 2, 2),
        (1000, 256 * MIB, 2, 1),
        (2000, 64 * MIB, 2, 1),
        (8000, 512 * MIB, 2, 2),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let mut settings = config(&directory);
        settings.cpu_millis = cpu;
        settings.memory_bytes = memory;
        settings.max_sandboxes = slots;
        let daemon = open(settings, &runtime).await;
        let barrier = Arc::new(Barrier::new(9));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let daemon = daemon.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                daemon.create(request()).await
            });
        }
        barrier.wait().await;
        let mut accepted = Vec::new();
        let mut rejected = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(sandbox) => accepted.push(sandbox.id),
                Err(error) => {
                    assert_eq!(error.status, 429);
                    assert_eq!(error.code, "CAPACITY_EXCEEDED");
                    rejected += 1;
                }
            }
        }
        assert_eq!(accepted.len(), admitted);
        assert_eq!(rejected, 8 - admitted);
        assert_eq!(daemon.list().await.unwrap().len(), admitted);
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(state.creates, admitted);
            assert_eq!(state.sandboxes.len(), admitted);
            assert!(state.specs.iter().map(|spec| spec.cpu_millis).sum::<u64>() <= cpu);
            assert!(
                state
                    .specs
                    .iter()
                    .map(|spec| spec.memory_bytes)
                    .sum::<u64>()
                    <= memory
            );
        }
        daemon.delete(&accepted[0]).await.unwrap();
        let replacement = create(&daemon).await;
        assert!(!accepted.contains(&replacement.id));
        assert_eq!(daemon.list().await.unwrap().len(), admitted);
    }
}

#[tokio::test]
async fn restart_reconciles_stored_state_against_shared_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let paused = create(&daemon).await;
    let missing = create(&daemon).await;
    let inspections_before = {
        let mut state = runtime.state.lock().unwrap();
        state
            .sandboxes
            .insert(paused.id.clone(), RuntimeState::Paused);
        state.sandboxes.remove(&missing.id);
        state.inspections
    };
    drop(daemon);

    let daemon = open(config(&directory), &runtime).await;
    // list does not inspect: these states must have been reconciled by open.
    let sandboxes = daemon.list().await.unwrap();
    assert_eq!(sandboxes.len(), 2);
    assert_eq!(
        sandboxes
            .iter()
            .find(|item| item.id == paused.id)
            .unwrap()
            .status
            .state,
        "Paused"
    );
    let failed = sandboxes.iter().find(|item| item.id == missing.id).unwrap();
    assert_eq!(failed.status.state, "Failed");
    assert!(failed.status.message.is_some());
    {
        let state = runtime.state.lock().unwrap();
        assert_eq!(state.inspections, inspections_before + 2);
        assert_eq!(state.creates, 2);
        assert_eq!(state.owners.len(), 2);
        assert!(!state.owners[0].is_empty());
        assert_eq!(state.owners[0], state.owners[1]);
    }
    assert_eq!(daemon.create(request()).await.err().unwrap().status, 429);
    daemon.delete(&missing.id).await.unwrap();
    create(&daemon).await;
    daemon.resume(&paused.id).await.unwrap();
    assert_eq!(
        daemon.get(&paused.id).await.unwrap().status.state,
        "Running"
    );
}

#[tokio::test]
async fn uncertain_delete_retains_durable_reservation_until_confirmed() {
    for outcome in [DeleteOutcome::Error, DeleteOutcome::Unconfirmed] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let daemon = open(config(&directory), &runtime).await;
        let sandbox = create(&daemon).await;
        create(&daemon).await;
        runtime.state.lock().unwrap().delete_outcome = outcome;
        let error = daemon.delete(&sandbox.id).await.err().unwrap();
        assert_eq!(error.status, 503);
        assert_eq!(error.code, "RUNTIME_UNAVAILABLE");
        let stored = daemon.list().await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(
            stored
                .iter()
                .find(|item| item.id == sandbox.id)
                .unwrap()
                .status
                .state,
            "Stopping"
        );
        assert_eq!(daemon.create(request()).await.err().unwrap().status, 429);
        assert_eq!(runtime.state.lock().unwrap().creates, 2);
        drop(daemon);

        let daemon = open(config(&directory), &runtime).await;
        assert_eq!(daemon.list().await.unwrap().len(), 2);
        assert_eq!(daemon.create(request()).await.err().unwrap().status, 429);
        runtime.state.lock().unwrap().delete_outcome = DeleteOutcome::Confirmed;
        daemon.delete(&sandbox.id).await.unwrap();
        create(&daemon).await;
        let state = runtime.state.lock().unwrap();
        assert_eq!(state.creates, 3);
        assert_eq!(state.deletes, 3);
        assert!(!state.sandboxes.contains_key(&sandbox.id));
    }
}

#[tokio::test]
async fn unsupported_network_policy_is_rejected_before_runtime_or_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let mut payload = request_json();
    payload["networkPolicy"] = json!({
        "defaultAction": "deny",
        "egress": [{"action": "allow", "target": "example.com"}]
    });
    let request = serde_json::from_value::<CreateRequest>(payload)
        .expect("networkPolicy is a recognized request field");
    let error = daemon.create(request).await.err().unwrap();
    assert_eq!(error.status, 501);
    assert_eq!(error.code, "NOT_SUPPORTED");
    assert!(daemon.list().await.unwrap().is_empty());
    {
        let state = runtime.state.lock().unwrap();
        assert_eq!(
            (
                state.creates,
                state.inspections,
                state.pauses,
                state.resumes,
                state.deletes,
                state.endpoints
            ),
            (0, 0, 0, 0, 0, 0)
        );
        assert!(state.sandboxes.is_empty());
    }
    create(&daemon).await;
    create(&daemon).await;
}

#[tokio::test]
async fn auth_sandbox_token_is_scoped_persistent_and_not_a_control_key() {
    use axum::http::{HeaderMap, HeaderValue};

    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let first = create(&daemon).await;
    let second = create(&daemon).await;
    let endpoint = daemon.endpoint(&first.id, 44772, false).await.unwrap();
    assert_eq!(
        endpoint["endpoint"],
        format!("localhost:8080/v1/sandboxes/{}/proxy/44772", first.id)
    );
    assert_eq!(
        daemon.upstream(&first.id, 44772).await.unwrap(),
        "http://127.0.0.1:44772"
    );
    let token = endpoint["headers"]["X-PVISOR-SANDBOX-TOKEN"]
        .as_str()
        .unwrap();
    assert!(token.len() >= 32);
    let second_endpoint = daemon.endpoint(&second.id, 44772, false).await.unwrap();
    assert_ne!(endpoint["headers"], second_endpoint["headers"]);
    assert!(
        daemon
            .endpoint(&first.id, 44772, true)
            .await
            .unwrap()
            .get("headers")
            .is_none()
    );

    let empty = HeaderMap::new();
    assert!(!daemon.authorize(&empty));
    assert!(!daemon.authorize_proxy(&first.id, &empty).await);
    let mut scoped = HeaderMap::new();
    scoped.insert(
        "X-PVISOR-SANDBOX-TOKEN",
        HeaderValue::from_str(token).unwrap(),
    );
    assert!(!daemon.authorize(&scoped));
    assert!(daemon.authorize_proxy(&first.id, &scoped).await);
    assert!(!daemon.authorize_proxy(&second.id, &scoped).await);
    assert!(!daemon.authorize_proxy("sb-unknown", &scoped).await);
    let mut wrong = scoped.clone();
    wrong.insert(
        "X-PVISOR-SANDBOX-TOKEN",
        HeaderValue::from_static("wrong-sandbox-token"),
    );
    assert!(!daemon.authorize_proxy(&first.id, &wrong).await);
    let mut control = HeaderMap::new();
    control.insert("OPEN-SANDBOX-API-KEY", HeaderValue::from_static(API_KEY));
    assert!(daemon.authorize(&control));
    assert!(daemon.authorize_proxy(&first.id, &control).await);
    assert!(daemon.authorize_proxy(&second.id, &control).await);
    control.insert(
        "OPEN-SANDBOX-API-KEY",
        HeaderValue::from_static("incorrect-api-key"),
    );
    assert!(!daemon.authorize(&control));
    assert!(!daemon.authorize_proxy(&first.id, &control).await);

    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.authorize_proxy(&first.id, &scoped).await);
    assert!(!daemon.authorize_proxy(&second.id, &scoped).await);
    assert_eq!(
        daemon.endpoint(&first.id, 44772, false).await.unwrap()["headers"],
        endpoint["headers"]
    );
    daemon.delete(&first.id).await.unwrap();
    assert!(!daemon.authorize_proxy(&first.id, &scoped).await);
}

#[tokio::test]
async fn pending_delete_survives_get_and_is_replayed_on_restart() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    runtime.state.lock().unwrap().delete_outcome = DeleteOutcome::Error;

    let error = daemon.delete(&sandbox.id).await.unwrap_err();
    assert_eq!(error.status, 503);
    assert_eq!(error.code, "RUNTIME_UNAVAILABLE");
    let inspections_before = runtime.state.lock().unwrap().inspections;
    assert_eq!(
        daemon.get(&sandbox.id).await.unwrap().status.state,
        "Stopping"
    );
    assert_eq!(daemon.list().await.unwrap()[0].status.state, "Stopping");
    {
        let state = runtime.state.lock().unwrap();
        assert_eq!(
            state.sandboxes.get(&sandbox.id),
            Some(&RuntimeState::Running)
        );
        assert_eq!(state.inspections, inspections_before);
        assert_eq!(state.deletes, 1);
    }
    drop(daemon);

    runtime.state.lock().unwrap().delete_outcome = DeleteOutcome::Confirmed;
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.list().await.unwrap().is_empty());
    assert_eq!(daemon.get(&sandbox.id).await.unwrap_err().status, 404);
    {
        let state = runtime.state.lock().unwrap();
        assert!(state.sandboxes.is_empty());
        assert_eq!(state.deletes, 2);
        assert_eq!(state.creates, 1);
    }
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.list().await.unwrap().is_empty());
    assert_eq!(runtime.state.lock().unwrap().deletes, 2);
}

#[tokio::test]
async fn stale_maintenance_expiration_does_not_delete_renewed_sandbox() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    let mut stale_scan = daemon.list().await.unwrap().remove(0);
    // Model an expired scan result without sleeps or expiring the live record.
    stale_scan.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
    assert!(stale_scan.expires_at.unwrap() <= chrono::Utc::now());
    let renewed_until = sandbox.expires_at.unwrap() + chrono::Duration::seconds(600);
    let renewed = daemon
        .renew(
            &sandbox.id,
            super::RenewRequest {
                expires_at: renewed_until,
            },
        )
        .await
        .unwrap();
    assert_eq!(renewed.expires_at, renewed_until);

    daemon
        .delete_conditionally(&stale_scan.id, true)
        .await
        .unwrap();
    let stored = daemon.list().await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, sandbox.id);
    assert_eq!(stored[0].status.state, "Running");
    assert_eq!(stored[0].expires_at, Some(renewed_until));
    let state = runtime.state.lock().unwrap();
    assert_eq!(state.deletes, 0);
    assert_eq!(
        state.sandboxes.get(&sandbox.id),
        Some(&RuntimeState::Running)
    );
}

#[tokio::test]
async fn unknown_get_ids_do_not_grow_weak_operation_table() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.operations.lock().await.is_empty());
    for index in 0..128 {
        let error = daemon
            .get(&format!("sb-unknown-{index}"))
            .await
            .unwrap_err();
        assert_eq!(error.status, 404);
        assert_eq!(error.code, "SANDBOX_NOT_FOUND");
        assert!(daemon.operations.lock().await.is_empty());
    }
    assert_eq!(runtime.state.lock().unwrap().inspections, 0);

    let sandbox = create(&daemon).await;
    daemon.get(&sandbox.id).await.unwrap();
    let entries_before = {
        let operations = daemon.operations.lock().await;
        assert!(operations.values().all(|gate| gate.upgrade().is_none()));
        operations.len()
    };
    let inspections_before = runtime.state.lock().unwrap().inspections;
    for index in 0..128 {
        assert_eq!(
            daemon
                .get(&format!("sb-other-unknown-{index}"))
                .await
                .unwrap_err()
                .status,
            404
        );
        assert_eq!(daemon.operations.lock().await.len(), entries_before);
    }
    assert_eq!(
        runtime.state.lock().unwrap().inspections,
        inspections_before
    );
    assert_eq!(daemon.list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_runtime_spec_is_rejected_before_admission_and_runtime_calls() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let mut invalid = Vec::new();
    for image in [
        "../host",
        "execd/../../host",
        "/rootfs",
        ".hidden",
        "registry.example.test/opensandbox/execd:fixture",
        "https://registry.example.test/opensandbox/execd:fixture",
    ] {
        let mut payload = request_json();
        payload["image"]["uri"] = json!(image);
        invalid.push((payload, "image must be a local prepared-image key"));
    }
    for key in ["1INVALID", "PVISOR_KRUN_RUNNER_SPEC", "AGENTCTL_CONTROL"] {
        let mut payload = request_json();
        payload["env"] = json!({(key): "fixture-value"});
        let message = if key == "1INVALID" {
            "invalid guest environment key"
        } else {
            "reserved native control environment key"
        };
        invalid.push((payload, message));
    }
    for (cpu, message) in [
        ("9m", "native CPU quota must be at least 10 millicores"),
        ("8001m", "native profile supports at most eight CPU quotas"),
    ] {
        let mut payload = request_json();
        payload["resourceLimits"]["cpu"] = json!(cpu);
        invalid.push((payload, message));
    }
    for memory in ["1", "6291455", "4294967296Mi"] {
        let mut payload = request_json();
        payload["resourceLimits"]["memory"] = json!(memory);
        invalid.push((payload, "native memory limit is not representable"));
    }

    for (payload, message) in invalid {
        let request: CreateRequest = serde_json::from_value(payload).unwrap();
        // These inputs pass wire validation; native validation must reject them
        // before even oversized resource requests reach node admission.
        let validated = request.clone().validate(86400).unwrap_or_else(|error| {
            panic!("fixture rejected before runtime spec validation: {error}")
        });
        let spec = RuntimeSpec {
            id: format!("sb-{}", uuid::Uuid::new_v4()),
            image: validated.image.uri,
            entrypoint: validated.entrypoint,
            env: validated.env,
            cpu_millis: validated.cpu_millis,
            memory_bytes: validated.memory_bytes,
        };
        let validation_error = crate::runtime::validate_spec(&spec).unwrap_err();
        assert!(
            validation_error.to_string().contains(message),
            "{validation_error}"
        );
        let error = daemon.create(request).await.unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.code, "INVALID_REQUEST");
        assert_eq!(
            error.message,
            "invalid runtime image, argv, environment or resource limits"
        );
        assert!(daemon.list().await.unwrap().is_empty());
        assert!(daemon.registry.lock().await.sandboxes.is_empty());
        assert!(daemon.operations.lock().await.is_empty());
        let state = runtime.state.lock().unwrap();
        assert_eq!(
            (
                state.creates,
                state.inspections,
                state.pauses,
                state.resumes,
                state.deletes,
                state.endpoints
            ),
            (0, 0, 0, 0, 0, 0)
        );
        assert!(state.sandboxes.is_empty());
        assert!(state.specs.is_empty());
    }
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.list().await.unwrap().is_empty());
    create(&daemon).await;
    create(&daemon).await;
    assert_eq!(daemon.list().await.unwrap().len(), 2);
    assert_eq!(runtime.state.lock().unwrap().creates, 2);
}

fn storage_error<T: std::fmt::Debug>(result: Result<T, super::ApiError>) {
    let error = result.unwrap_err();
    assert_eq!(error.status, 503);
    assert_eq!(error.code, "STORAGE_UNAVAILABLE");
}

fn runtime_calls(runtime: &FakeRuntime) -> (usize, usize, usize, usize, usize, usize) {
    let state = runtime.state.lock().unwrap();
    (
        state.creates,
        state.inspections,
        state.pauses,
        state.resumes,
        state.deletes,
        state.endpoints,
    )
}

fn disk_inventory(directory: &TempDir) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for name in ["sandboxes.json", "records/meta.json"] {
        files.insert(
            name.into(),
            std::fs::read(directory.path().join(name)).unwrap(),
        );
    }
    for entry in std::fs::read_dir(directory.path().join("records")).unwrap() {
        let path = entry.unwrap().path();
        files.insert(
            path.strip_prefix(directory.path()).unwrap().to_path_buf(),
            std::fs::read(path).unwrap(),
        );
    }
    files
}

fn fail_commit(daemon: &Daemon, target: CommitPoint, skip: usize) {
    let remaining = Mutex::new(skip);
    daemon.store.set_commit_hook(move |point| {
        if point == target {
            let mut remaining = remaining.lock().unwrap();
            if *remaining == 0 {
                anyhow::bail!("injected commit failure at {point:?}");
            }
            *remaining -= 1;
        }
        Ok(())
    });
}

#[derive(Clone, Copy, Debug)]
enum UncertainCommit {
    BeforeIntention,
    Intention,
    CreatedState,
    PauseIntention,
    PausedState,
    DeleteIntention,
    Unlinked,
}

#[tokio::test]
async fn uncertain_commits_fail_stop_and_restart_uses_the_disk_winner() {
    use UncertainCommit::*;
    for case in [
        BeforeIntention,
        Intention,
        CreatedState,
        PauseIntention,
        PausedState,
        DeleteIntention,
        Unlinked,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let mut cfg = config(&directory);
        cfg.max_sandboxes = 1;
        let daemon = open(cfg, &runtime).await;
        let existing = if matches!(case, BeforeIntention | Intention | CreatedState) {
            None
        } else {
            Some(bounded(create(&daemon)).await)
        };
        let (point, skip) = match case {
            BeforeIntention => (CommitPoint::BeforeDisk, 0),
            CreatedState | PausedState => (CommitPoint::AfterRename, 1),
            Unlinked => (CommitPoint::AfterUnlink, 0),
            _ => (CommitPoint::AfterRename, 0),
        };
        fail_commit(&daemon, point, skip);
        match case {
            BeforeIntention | Intention | CreatedState => {
                storage_error(bounded(daemon.create(request())).await)
            }
            PauseIntention | PausedState => {
                storage_error(bounded(daemon.pause(&existing.as_ref().unwrap().id)).await)
            }
            DeleteIntention | Unlinked => {
                storage_error(bounded(daemon.delete(&existing.as_ref().unwrap().id)).await)
            }
        }
        // An uncertain disk result must never be published from stale memory.
        {
            let registry = bounded(daemon.registry.lock()).await;
            let expected = match case {
                BeforeIntention | Intention => None,
                CreatedState => Some("Pending"),
                PausedState => Some("Pausing"),
                Unlinked => Some("Stopping"),
                _ => Some("Running"),
            };
            assert_eq!(
                registry
                    .sandboxes
                    .values()
                    .next()
                    .map(|r| r.sandbox.status.state.as_str()),
                expected,
                "{case:?}"
            );
        }
        {
            let state = runtime.state.lock().unwrap();
            assert_eq!(
                state.creates,
                usize::from(!matches!(case, BeforeIntention | Intention)),
                "no native create without a committed intention: {case:?}"
            );
            assert_eq!(state.pauses, usize::from(matches!(case, PausedState)));
            assert_eq!(state.deletes, usize::from(matches!(case, Unlinked)));
        }
        // Disable injection: subsequent failures must come from the daemon latch,
        // not from a fault that happens to keep rejecting later writes.
        daemon.store.set_commit_hook(|_| Ok(()));
        let calls = runtime_calls(&runtime);
        let bytes = disk_inventory(&directory);
        let id = existing
            .as_ref()
            .map(|s| s.id.as_str())
            .unwrap_or("sb-12345678-1234-4234-8234-123456789abc");
        storage_error(bounded(daemon.list()).await);
        storage_error(bounded(daemon.create(request())).await);
        storage_error(bounded(daemon.get(id)).await);
        storage_error(bounded(daemon.pause(id)).await);
        storage_error(bounded(daemon.resume(id)).await);
        storage_error(bounded(daemon.delete(id)).await);
        storage_error(
            bounded(daemon.renew(
                id,
                super::RenewRequest {
                    expires_at: chrono::Utc::now() + chrono::Duration::seconds(1200),
                },
            ))
            .await,
        );
        storage_error(bounded(daemon.endpoint(id, 44772, true)).await);
        assert_eq!(
            runtime_calls(&runtime),
            calls,
            "fail-stop must not call native runtime"
        );
        assert_eq!(
            disk_inventory(&directory),
            bytes,
            "fail-stop must not overwrite disk winner"
        );
        drop(daemon);

        let (store, disk) = super::store::Store::open(directory.path()).unwrap();
        let expected_disk = match case {
            BeforeIntention | Unlinked => None,
            Intention => Some("Pending"),
            CreatedState => Some("Running"),
            PauseIntention => Some("Pausing"),
            PausedState => Some("Paused"),
            DeleteIntention => Some("Stopping"),
        };
        assert_eq!(
            disk.sandboxes
                .values()
                .next()
                .map(|r| r.sandbox.status.state.as_str()),
            expected_disk,
            "{case:?}"
        );
        assert_eq!(disk.sandboxes.len(), usize::from(expected_disk.is_some()));
        drop(store);
        let mut cfg = config(&directory);
        cfg.max_sandboxes = 1;
        let restarted = bounded(open(cfg, &runtime)).await;
        let restored = bounded(restarted.list()).await.unwrap();
        if matches!(case, BeforeIntention | DeleteIntention | Unlinked) {
            assert!(restored.is_empty());
            bounded(create(&restarted)).await;
        } else {
            assert_eq!(restored.len(), 1);
            let saved = disk.sandboxes.values().next().unwrap();
            assert_eq!(restored[0].id, saved.sandbox.id);
            assert_eq!(
                restored[0].status.state,
                match case {
                    Intention => "Failed",
                    PausedState => "Paused",
                    _ => "Running",
                }
            );
            let registry = bounded(restarted.registry.lock()).await;
            let record = &registry.sandboxes[&saved.sandbox.id];
            assert_eq!(
                (record.cpu_millis, record.memory_bytes),
                (saved.cpu_millis, saved.memory_bytes)
            );
            assert_eq!(record.endpoint_token, saved.endpoint_token);
            assert_eq!(record.env, saved.env);
            drop(registry);
            let calls = runtime_calls(&runtime);
            let error = bounded(restarted.create(request())).await.unwrap_err();
            assert_eq!(
                (error.status, error.code.as_str()),
                (429, "CAPACITY_EXCEEDED")
            );
            assert_eq!(runtime_calls(&runtime), calls);
            bounded(restarted.delete(&saved.sandbox.id)).await.unwrap();
            bounded(create(&restarted)).await;
        }
    }
}

#[tokio::test]
async fn blocked_commit_leaves_list_readable_and_serializes_second_admission() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut cfg = config(&directory);
    cfg.max_sandboxes = 1;
    let daemon = open(cfg, &runtime).await;
    let (gate, entered) = block_disk(&daemon);
    let first = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.create(request()).await })
    };
    bounded(entered).await.unwrap();
    assert!(bounded(daemon.list()).await.unwrap().is_empty());
    assert!(daemon.commits.try_lock().is_err());
    let mut second = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.create(request()).await })
    };
    // Both owned creates reached their lifecycle gate before admission.
    bounded(async {
        loop {
            if daemon.operations.lock().await.len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut second)
            .await
            .is_err()
    );
    assert!(bounded(daemon.list()).await.unwrap().is_empty());
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
    gate.release();
    let first = bounded(first).await.unwrap().unwrap();
    let error = bounded(second).await.unwrap().unwrap_err();
    assert_eq!(
        (error.status, error.code.as_str()),
        (429, "CAPACITY_EXCEEDED")
    );
    let listed = bounded(daemon.list()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, first.id);
    assert_eq!(runtime.state.lock().unwrap().creates, 1);
}

#[tokio::test]
async fn cancelling_an_accepted_public_create_does_not_cancel_disk_or_native_work() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let (native_entered, native_receiver) = oneshot::channel();
    let (native_release, release_receiver) = oneshot::channel();
    *runtime.create_gate.lock().unwrap() = Some(NativeCreateGate {
        entered: native_entered,
        release: release_receiver,
    });
    let daemon = open(config(&directory), &runtime).await;
    let (gate, entered) = block_disk(&daemon);
    let caller = {
        let daemon = daemon.clone();
        tokio::spawn(async move { daemon.create(request()).await })
    };
    bounded(entered).await.unwrap();
    caller.abort();
    assert!(bounded(caller).await.unwrap_err().is_cancelled());
    gate.release();
    bounded(native_receiver).await.unwrap();
    let pending = bounded(daemon.list()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status.state, "Pending");
    let id = pending[0].id.clone();
    // Native create is still blocked after its public waiter disappeared.
    let operation = bounded(daemon.operation(&id)).await.unwrap();
    assert!(operation.try_lock().is_err());
    native_release.send(()).unwrap();
    let finished = bounded(operation.lock()).await;
    assert_eq!(
        bounded(daemon.list()).await.unwrap()[0].status.state,
        "Running"
    );
    assert_eq!(runtime.state.lock().unwrap().creates, 1);
    drop(finished);
    drop(operation);
    drop(daemon);
    // Acquiring the store lock also proves the detached owned task released it.
    let restarted = bounded(open(config(&directory), &runtime)).await;
    let restored = bounded(restarted.list()).await.unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, id);
    assert_eq!(restored[0].status.state, "Running");
    assert_eq!(runtime.state.lock().unwrap().creates, 1);
}

async fn occupied_v1(directory: &TempDir) -> (String, BTreeMap<std::path::PathBuf, Vec<u8>>) {
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(directory), &runtime).await;
    bounded(create(&daemon)).await;
    let mut legacy = bounded(daemon.registry.lock()).await.clone();
    legacy.version = 1;
    drop(daemon);
    std::fs::write(
        directory.path().join("sandboxes.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    // Leave a prepared tree in place: rejecting the factory must not retire it.
    (legacy.owner, disk_inventory(directory))
}

#[tokio::test]
async fn rejected_factory_leaves_occupied_v1_and_prepared_records_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, bytes) = occupied_v1(&directory).await;
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = called.clone();
    let result = bounded(Daemon::open(config(&directory), |accepted_owner| {
        observed.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(accepted_owner, owner);
        assert!(
            super::store::Store::open(directory.path()).is_err(),
            "factory must run under exclusive ownership"
        );
        assert_eq!(disk_inventory(&directory), bytes);
        anyhow::bail!("runtime factory rejected legacy ownership")
    }))
    .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("runtime factory rejected")
    );
    assert!(called.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(disk_inventory(&directory), bytes);
    assert!(!directory.path().join("owner.json").exists());
    let (store, legacy) = super::store::Store::open(directory.path()).unwrap();
    assert_eq!(legacy.version, 1);
    assert_eq!(legacy.sandboxes.len(), 1);
    drop(store);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn native_factory_rejects_occupied_v1_before_daemon_migration() {
    let directory = tempfile::tempdir().unwrap();
    let (_, bytes) = occupied_v1(&directory).await;
    let result = bounded(Daemon::open(config(&directory), |owner| {
        assert!(super::store::Store::open(directory.path()).is_err());
        // These paths deliberately do not exist. Constructor ownership rejection
        // must precede preflight; no fake cgroup files or enforcement are involved.
        let runtime = crate::runtime::NativeRuntime::new(crate::runtime::NativeRuntimeConfig {
            state_dir: directory.path().to_path_buf(),
            owner,
            cgroup_root: directory.path().join("unused-cgroup"),
            images_dir: directory.path().join("unused-images"),
            executable: directory.path().join("unused-executable"),
        })?;
        Ok(Arc::new(runtime) as Arc<dyn Runtime>)
    }))
    .await;
    let message = result.err().unwrap().to_string();
    assert!(message.contains("occupied sandbox registry"), "{message}");
    assert!(message.contains("no native owner.json marker"), "{message}");
    assert_eq!(disk_inventory(&directory), bytes);
    assert!(!directory.path().join("owner.json").exists());
    let (_, legacy) = super::store::Store::open(directory.path()).unwrap();
    assert_eq!(legacy.version, 1);
    assert_eq!(legacy.sandboxes.len(), 1);
}

#[tokio::test]
async fn native_guest_environment_is_preserved_without_container_restrictions() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let mut payload = request_json();
    payload["env"] = json!({
        "PATH": "/guest/bin:/usr/bin",
        "LD_PRELOAD": "/guest/lib/fixture.so",
        "CONTAINER_HOST": "unix:///guest/podman.sock",
        "GUEST_VALUE": "literal $HOME; no host expansion"
    });
    let request: CreateRequest = serde_json::from_value(payload).unwrap();
    let expected_env = request.env.clone();
    let sandbox = daemon.create(request).await.unwrap();
    assert_eq!(sandbox.status.state, "Running");
    let state = runtime.state.lock().unwrap();
    assert_eq!(state.creates, 1);
    assert_eq!(state.specs[0].image, "execd-fixture");
    assert_eq!(state.specs[0].env, expected_env);
}
