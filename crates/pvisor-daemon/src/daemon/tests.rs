use super::{Config, CreateRequest, Daemon};
use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use tokio::sync::Barrier;

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
}

#[async_trait::async_trait]
impl Runtime for FakeRuntime {
    async fn create(&self, spec: &RuntimeSpec) -> anyhow::Result<()> {
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
