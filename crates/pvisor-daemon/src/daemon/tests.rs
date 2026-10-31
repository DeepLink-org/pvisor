use super::store::CommitPoint;
use super::{Config, CreateRequest, Daemon};
use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use std::{future::Future, time::Duration};
use tempfile::TempDir;
use tokio::sync::{Barrier, oneshot};

fn claim_identity() -> super::models::ClaimIdentity {
    super::models::ClaimIdentity {
        claim_id: uuid::Uuid::new_v4().to_string(),
        home: "home-a".into(),
        task_id: "task-a".into(),
        generation: 1,
    }
}

#[tokio::test]
async fn claim_reservation_races_ordinary_create_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut cfg = config(&directory);
    cfg.max_sandboxes = 1;
    let daemon = open(cfg, &runtime).await;
    let identity = claim_identity();
    let (claim, ordinary) = tokio::join!(
        daemon.reserve_claim(identity.clone(), request()),
        daemon.create(request())
    );
    assert_ne!(claim.is_ok(), ordinary.is_ok());
    assert_eq!(daemon.capacity_usage().await.unwrap(), (1, 1000, 64 * MIB));
    match claim {
        Ok(_) => {
            assert_eq!(ordinary.unwrap_err().status, 429);
            assert!(daemon.list().await.unwrap().is_empty());
            assert_eq!(runtime.state.lock().unwrap().creates, 0);
        }
        Err(error) => assert_eq!(error.status, 429),
    }
}

#[tokio::test]
async fn claim_release_is_durable_hidden_and_rejects_mismatched_replay() {
    use super::models::ClaimPhase;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    let sandbox = daemon
        .reserve_claim(identity.clone(), request())
        .await
        .unwrap();
    assert_eq!(sandbox.id, format!("sb-{}", identity.claim_id));
    assert!(sandbox.expires_at.is_none());
    assert_eq!(daemon.get(&sandbox.id).await.unwrap_err().status, 404);
    assert_eq!(daemon.delete(&sandbox.id).await.unwrap_err().status, 404);
    assert!(
        daemon
            .renew(
                &sandbox.id,
                super::RenewRequest {
                    expires_at: chrono::Utc::now() + chrono::Duration::seconds(700)
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        daemon
            .reserve_claim(identity.clone(), request())
            .await
            .unwrap()
            .id,
        sandbox.id
    );
    let mut wrong = identity.clone();
    wrong.generation += 1;
    assert_eq!(
        daemon
            .reserve_claim(wrong.clone(), request())
            .await
            .unwrap_err()
            .code,
        "CLAIM_MISMATCH"
    );
    assert!(daemon.launch_claim(&wrong).await.is_err());
    assert!(daemon.release_claim(&wrong).await.is_err());
    assert!(daemon.inspect_claim(&wrong).await.is_err());
    let mut changed = request();
    changed.env.insert("CHANGED".into(), "yes".into());
    assert_eq!(
        daemon
            .reserve_claim(identity.clone(), changed)
            .await
            .unwrap_err()
            .code,
        "CLAIM_MISMATCH"
    );
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(runtime.state.lock().unwrap().inspections, 0);
    assert_eq!(daemon.pending_claims().await.len(), 1);
    assert_eq!(
        daemon.inspect_claim(&identity).await.unwrap().phase,
        ClaimPhase::Reserved
    );
    daemon.release_claim(&identity).await.unwrap();
    daemon.release_claim(&identity).await.unwrap();
    assert_eq!(daemon.capacity_usage().await.unwrap(), (0, 0, 0));
    assert!(daemon.pending_claims().await.is_empty());
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(
        daemon.inspect_claim(&identity).await.unwrap().phase,
        ClaimPhase::Released
    );
    assert!(daemon.launch_claim(&identity).await.is_err());
    assert!(
        daemon
            .reserve_claim(identity.clone(), request())
            .await
            .is_err()
    );
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
    assert_eq!(runtime.state.lock().unwrap().inspections, 0);
    create(&daemon).await;
    create(&daemon).await;
}

#[tokio::test]
async fn claim_launch_replay_and_deletion_never_recreate() {
    use super::models::ClaimPhase;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    daemon
        .reserve_claim(identity.clone(), request())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        daemon.launch_claim(&identity),
        daemon.launch_claim(&identity)
    );
    let sandbox = a.unwrap();
    assert_eq!(b.unwrap().id, sandbox.id);
    assert!(sandbox.expires_at.is_some());
    assert_eq!(runtime.state.lock().unwrap().creates, 1);
    assert_eq!(
        daemon.inspect_claim(&identity).await.unwrap().phase,
        ClaimPhase::Finished
    );
    assert!(daemon.release_claim(&identity).await.is_err());
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    daemon.launch_claim(&identity).await.unwrap();
    daemon.delete(&sandbox.id).await.unwrap();
    assert_eq!(daemon.capacity_usage().await.unwrap(), (0, 0, 0));
    let record = daemon.record(&sandbox.id).await.unwrap();
    assert!(record.env.is_empty());
    assert!(record.sandbox.entrypoint.is_empty());
    assert!(daemon.list().await.unwrap().is_empty());
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(daemon.launch_claim(&identity).await.is_err());
    assert!(
        daemon
            .inspect_claim(&identity)
            .await
            .unwrap()
            .error
            .is_some()
    );
    assert_eq!(runtime.state.lock().unwrap().creates, 1);
}

#[tokio::test]
async fn claim_restart_launch_gap_never_reissues_runtime_create() {
    use super::models::ClaimPhase;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    let sandbox = daemon
        .reserve_claim(identity.clone(), request())
        .await
        .unwrap();
    // Model the durable launch fence immediately before the process crashes.
    daemon
        .update(&sandbox.id, |registry| {
            let mut record = registry.sandboxes[&sandbox.id].clone();
            record.claim.as_mut().unwrap().phase = ClaimPhase::Launching;
            record.sandbox.status.state = "Pending".into();
            Ok((Some(record), ()))
        })
        .await
        .unwrap();
    runtime.state.lock().unwrap().inspect_error = true;
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
    assert_eq!(
        daemon.launch_claim(&identity).await.unwrap_err().code,
        "CLAIM_LAUNCH_UNCERTAIN"
    );
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
    runtime.state.lock().unwrap().inspect_error = false;
    daemon.get(&sandbox.id).await.unwrap();
    assert_eq!(
        daemon.inspect_claim(&identity).await.unwrap().phase,
        ClaimPhase::Released
    );
    assert!(daemon.launch_claim(&identity).await.is_err());
    assert_eq!(daemon.capacity_usage().await.unwrap().0, 0);
}

#[tokio::test]
async fn claim_cancelled_reservation_still_commits_before_launch() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    let (disk, entered) = block_disk(&daemon);
    let task = {
        let daemon = daemon.clone();
        let identity = identity.clone();
        tokio::spawn(async move { daemon.reserve_claim(identity, request()).await })
    };
    bounded(entered).await.unwrap();
    task.abort();
    disk.release();
    bounded(async {
        while daemon.inspect_claim(&identity).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
    daemon.release_claim(&identity).await.unwrap();
}

#[tokio::test]
async fn claim_uncertain_launch_commit_failstops_without_runtime_create() {
    use super::models::ClaimPhase;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    daemon
        .reserve_claim(identity.clone(), request())
        .await
        .unwrap();
    daemon.store.set_commit_hook(|point| {
        anyhow::ensure!(
            point != CommitPoint::AfterRename,
            "injected uncertain launch fence"
        );
        Ok(())
    });
    assert_eq!(
        daemon.launch_claim(&identity).await.unwrap_err().code,
        "STORAGE_UNAVAILABLE"
    );
    assert!(daemon.capacity_usage().await.is_err());
    assert!(daemon.launch_claim(&identity).await.is_err());
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
    runtime.state.lock().unwrap().inspect_error = true;
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(
        daemon.inspect_claim(&identity).await.unwrap().phase,
        ClaimPhase::Launching
    );
    assert!(daemon.launch_claim(&identity).await.is_err());
    assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
    assert_eq!(runtime.state.lock().unwrap().creates, 0);
}

async fn claim_ledger_charge(daemon: &Daemon) -> usize {
    let registry = daemon.registry.lock().await;
    let header = super::models::Registry {
        version: 1,
        owner: registry.owner.clone(),
        sandboxes: BTreeMap::new(),
    };
    registry
        .sandboxes
        .iter()
        .map(|(id, record)| super::store::record_weight(id, record).unwrap())
        .sum::<usize>()
        + serde_json::to_vec(&header).unwrap().len()
        - usize::from(!registry.sandboxes.is_empty())
}

#[tokio::test]
async fn claim_full_ledger_preserves_lifecycle_headroom_and_immutable_result() {
    use super::models::ClaimPhase;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let cfg = || {
        let mut cfg = config(&directory);
        cfg.max_sandboxes = 5;
        cfg.cpu_millis = 5000;
        cfg.memory_bytes = 512 * MIB;
        cfg
    };
    let daemon = open(cfg(), &runtime).await;
    let filler = create(&daemon).await;
    let launched = claim_identity();
    let rejected = claim_identity();
    let expired = claim_identity();
    for identity in [&launched, &rejected, &expired] {
        daemon
            .reserve_claim(identity.clone(), request())
            .await
            .unwrap();
    }
    daemon
        .update(&filler.id, |registry| {
            let mut record = registry.sandboxes[&filler.id].clone();
            record.env.insert("PADDING".into(), String::new());
            Ok((Some(record), ()))
        })
        .await
        .unwrap();
    let remaining = 16 * 1024 * 1024 - claim_ledger_charge(&daemon).await;
    daemon
        .update(&filler.id, |registry| {
            let mut record = registry.sandboxes[&filler.id].clone();
            record.env.insert("PADDING".into(), "x".repeat(remaining));
            Ok((Some(record), ()))
        })
        .await
        .unwrap();
    assert_eq!(claim_ledger_charge(&daemon).await, 16 * 1024 * 1024);
    assert_eq!(
        daemon
            .reserve_claim(claim_identity(), request())
            .await
            .unwrap_err()
            .code,
        "REGISTRY_CAPACITY_EXCEEDED"
    );
    // Reopen the full registry before exercising all growth transitions.
    drop(daemon);
    let daemon = open(cfg(), &runtime).await;
    assert_eq!(claim_ledger_charge(&daemon).await, 16 * 1024 * 1024);
    daemon.release_claim(&rejected).await.unwrap();
    let snapshot = daemon.launch_claim(&launched).await.unwrap();
    let expected = serde_json::to_value(&snapshot).unwrap();
    daemon.pause(&snapshot.id).await.unwrap();
    daemon
        .renew(
            &snapshot.id,
            super::RenewRequest {
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(1200),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(daemon.launch_claim(&launched).await.unwrap()).unwrap(),
        expected
    );
    daemon.delete(&snapshot.id).await.unwrap();
    let ttl_snapshot = daemon.launch_claim(&expired).await.unwrap();
    daemon
        .update(&ttl_snapshot.id, |registry| {
            let mut record = registry.sandboxes[&ttl_snapshot.id].clone();
            record.sandbox.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
            Ok((Some(record), ()))
        })
        .await
        .unwrap();
    daemon
        .delete_conditionally(&ttl_snapshot.id, true)
        .await
        .unwrap();
    assert_eq!(claim_ledger_charge(&daemon).await, 16 * 1024 * 1024);
    drop(daemon);
    let daemon = open(cfg(), &runtime).await;
    assert_eq!(claim_ledger_charge(&daemon).await, 16 * 1024 * 1024);
    for (identity, snapshot) in [(&launched, snapshot), (&expired, ttl_snapshot)] {
        let view = daemon.inspect_claim(identity).await.unwrap();
        assert_eq!(view.phase, ClaimPhase::Released);
        assert!(view.sandbox.metadata.is_empty());
        let result = view.creation_result.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::to_value(snapshot).unwrap()
        );
    }
    assert!(
        daemon
            .inspect_claim(&rejected)
            .await
            .unwrap()
            .creation_result
            .unwrap()
            .is_err()
    );
    assert_eq!(runtime.state.lock().unwrap().creates, 3);
}

#[tokio::test]
async fn claim_uncertainty_has_no_terminal_result_until_reconciled() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let identity = claim_identity();
    let sandbox = daemon
        .reserve_claim(identity.clone(), request())
        .await
        .unwrap();
    assert!(
        daemon
            .inspect_claim(&identity)
            .await
            .unwrap()
            .creation_result
            .is_none()
    );
    daemon
        .update(&sandbox.id, |registry| {
            let mut record = registry.sandboxes[&sandbox.id].clone();
            record.claim.as_mut().unwrap().phase = super::models::ClaimPhase::Launching;
            record.sandbox.status.state = "Pending".into();
            Ok((Some(record), ()))
        })
        .await
        .unwrap();
    runtime.state.lock().unwrap().inspect_error = true;
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert!(
        daemon
            .inspect_claim(&identity)
            .await
            .unwrap()
            .creation_result
            .is_none()
    );
    runtime.state.lock().unwrap().inspect_error = false;
    runtime
        .state
        .lock()
        .unwrap()
        .sandboxes
        .insert(sandbox.id.clone(), RuntimeState::Running);
    daemon.get(&sandbox.id).await.unwrap();
    let result = daemon
        .inspect_claim(&identity)
        .await
        .unwrap()
        .creation_result
        .unwrap()
        .unwrap();
    assert_eq!(result.status.state, "Running");
    daemon.delete(&sandbox.id).await.unwrap();
    assert_eq!(
        serde_json::to_value(
            daemon
                .inspect_claim(&identity)
                .await
                .unwrap()
                .creation_result
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(result).unwrap()
    );
}

#[tokio::test]
async fn claim_terminal_create_failure_is_immutable_but_uncertainty_is_not_terminal() {
    for uncertain in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        runtime.state.lock().unwrap().create_failure = Some(uncertain);
        let daemon = open(config(&directory), &runtime).await;
        let identity = claim_identity();
        let sandbox = daemon
            .reserve_claim(identity.clone(), request())
            .await
            .unwrap();
        let failure = daemon.launch_claim(&identity).await.unwrap_err();
        let view = daemon.inspect_claim(&identity).await.unwrap();
        if uncertain {
            assert!(view.creation_result.is_none());
            assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
            daemon.delete(&sandbox.id).await.unwrap();
        } else {
            assert_eq!(
                serde_json::to_value(view.creation_result.unwrap().unwrap_err()).unwrap(),
                serde_json::to_value(failure).unwrap()
            );
        }
        let expected = serde_json::to_value(
            daemon
                .inspect_claim(&identity)
                .await
                .unwrap()
                .creation_result,
        )
        .unwrap();
        drop(daemon);
        let daemon = open(config(&directory), &runtime).await;
        daemon.release_claim(&identity).await.unwrap();
        assert!(daemon.launch_claim(&identity).await.is_err());
        assert_eq!(
            serde_json::to_value(
                daemon
                    .inspect_claim(&identity)
                    .await
                    .unwrap()
                    .creation_result
            )
            .unwrap(),
            expected
        );
        assert_eq!(runtime.state.lock().unwrap().creates, 1);
    }
}

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
    create_failure: Option<bool>,
    inspections: usize,
    pauses: usize,
    resumes: usize,
    deletes: usize,
    endpoints: usize,
    delete_outcome: DeleteOutcome,
    control_reply: Option<anyhow::Result<RuntimeState>>,
    inspect_error: bool,
    endpoint_override: Option<String>,
}

#[derive(Default)]
struct FakeRuntime {
    state: Mutex<FakeState>,
    create_gate: Mutex<Option<NativeCreateGate>>,
    inspect_delays: Mutex<BTreeMap<String, Duration>>,
    delete_ids: Mutex<std::collections::BTreeSet<String>>,
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
        if let Some(uncertain) = state.create_failure {
            return Err(
                crate::runtime::CreateError::new("injected create failure", uncertain).into(),
            );
        }
        anyhow::ensure!(!state.sandboxes.contains_key(&spec.id), "duplicate sandbox");
        state.specs.push(spec.clone());
        state
            .sandboxes
            .insert(spec.id.clone(), RuntimeState::Running);
        Ok(())
    }

    async fn inspect(&self, id: &str) -> anyhow::Result<RuntimeState> {
        let delay = self.inspect_delays.lock().unwrap().get(id).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut state = self.state.lock().unwrap();
        state.inspections += 1;
        anyhow::ensure!(!state.inspect_error, "inspection unavailable");
        Ok(state
            .sandboxes
            .get(id)
            .copied()
            .unwrap_or(RuntimeState::Missing))
    }

    async fn pause(&self, id: &str) -> anyhow::Result<RuntimeState> {
        let mut state = self.state.lock().unwrap();
        state.pauses += 1;
        let reply = state.control_reply.take();
        let sandbox = state
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
        anyhow::ensure!(*sandbox == RuntimeState::Running, "sandbox not running");
        *sandbox = RuntimeState::Paused;
        reply.unwrap_or(Ok(*sandbox))
    }

    async fn resume(&self, id: &str) -> anyhow::Result<RuntimeState> {
        let mut state = self.state.lock().unwrap();
        state.resumes += 1;
        let reply = state.control_reply.take();
        let sandbox = state
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("sandbox missing"))?;
        anyhow::ensure!(*sandbox == RuntimeState::Paused, "sandbox not paused");
        *sandbox = RuntimeState::Running;
        reply.unwrap_or(Ok(*sandbox))
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.delete_ids.lock().unwrap().insert(id.to_owned());
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
        Ok(state
            .endpoint_override
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}")))
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
    let daemon = open_unreconciled(config, runtime).await;
    bounded(async {
        while !daemon.reconciled.load(std::sync::atomic::Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    daemon
}

async fn open_unreconciled(config: Config, runtime: &Arc<FakeRuntime>) -> Arc<Daemon> {
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
async fn budget_rejection_does_not_poison_other_sandbox_operations() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    let before = disk_inventory(&directory);
    let error = daemon
        .update(&sandbox.id, |registry| {
            let mut record = registry.sandboxes[&sandbox.id].clone();
            record
                .sandbox
                .metadata
                .insert("oversized".into(), "x".repeat(16 * 1024 * 1024));
            Ok((Some(record), ()))
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, "REGISTRY_CAPACITY_EXCEEDED");
    assert_eq!(disk_inventory(&directory), before);
    assert_eq!(daemon.list().await.unwrap().len(), 1);
    let other = create(&daemon).await;
    daemon.pause(&other.id).await.unwrap();
    daemon.resume(&other.id).await.unwrap();
    daemon.delete(&other.id).await.unwrap();
    daemon.delete(&sandbox.id).await.unwrap();
}

#[tokio::test]
async fn maintenance_rotates_past_more_than_eight_persistent_failures() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut cfg = config(&directory);
    cfg.max_sandboxes = 18;
    cfg.cpu_millis = 18000;
    cfg.memory_bytes = 2048 * MIB;
    let daemon = open(cfg, &runtime).await;
    for _ in 0..18 {
        let sandbox = create(&daemon).await;
        daemon
            .transition(&sandbox.id, "Stopping", None)
            .await
            .unwrap();
    }
    runtime.state.lock().unwrap().delete_outcome = DeleteOutcome::Error;
    let maintenance = daemon.start_maintenance();
    bounded(async {
        while runtime.delete_ids.lock().unwrap().len() != 18 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(daemon.list().await.unwrap().len(), 18);
    maintenance.abort();
    let _ = maintenance.await;
}

#[tokio::test]
async fn startup_is_background_retains_capacity_and_recovers_healthy_items() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let slow = create(&daemon).await;
    let healthy = create(&daemon).await;
    runtime
        .inspect_delays
        .lock()
        .unwrap()
        .insert(slow.id.clone(), Duration::from_secs(1));
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .sandboxes
            .insert(healthy.id.clone(), RuntimeState::Paused);
        state
            .sandboxes
            .insert(slow.id.clone(), RuntimeState::Paused);
    }
    drop(daemon);
    let daemon = tokio::time::timeout(
        Duration::from_millis(250),
        open_unreconciled(config(&directory), &runtime),
    )
    .await
    .expect("startup must not wait for the slow supervisor");
    assert_eq!(daemon.list().await.unwrap().len(), 2);
    assert_eq!(daemon.create(request()).await.unwrap_err().status, 429);
    // The slow record still says Running, but endpoint resolution must use
    // native state, not that stale durable observation.
    assert!(daemon.upstream(&slow.id, 44772).await.is_err());
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if daemon
                .list()
                .await
                .unwrap()
                .iter()
                .any(|s| s.id == healthy.id && s.status.state == "Paused")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("healthy supervisor must recover independently");
    assert!(!daemon.reconciled.load(std::sync::atomic::Ordering::Acquire));
    assert!(daemon.upstream(&healthy.id, 44772).await.is_err());
    let weak = Arc::downgrade(&daemon);
    drop(daemon);
    bounded(async {
        while weak.upgrade().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // Background completion releases the store lock, rather than retaining a
    // strong Arc in an idle coordinator forever.
    let _reopened = open(config(&directory), &runtime).await;
}

#[tokio::test]
async fn control_consumes_confirmation_or_retains_durable_intention() {
    for pause in [true, false] {
        let desired = if pause {
            RuntimeState::Paused
        } else {
            RuntimeState::Running
        };
        let confirmed = if pause { "Paused" } else { "Running" };
        for reply in [
            Ok(desired),
            Ok(RuntimeState::Stopped),
            Err(anyhow::anyhow!("ack lost after control")),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let runtime = Arc::new(FakeRuntime::default());
            let mut cfg = config(&directory);
            cfg.max_sandboxes = 1;
            let daemon = open(cfg, &runtime).await;
            let sandbox = create(&daemon).await;
            if !pause {
                bounded(daemon.pause(&sandbox.id)).await.unwrap();
            }
            let expected = if reply.as_ref().is_ok_and(|state| *state == desired) {
                confirmed
            } else if pause {
                "Pausing"
            } else {
                "Resuming"
            };
            let before = {
                let mut state = runtime.state.lock().unwrap();
                state.control_reply = Some(reply);
                state.inspections
            };
            let result = bounded(daemon.control(&sandbox.id, pause)).await;
            if expected == confirmed {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    (error.status, error.code.as_str()),
                    (503, "RUNTIME_UNAVAILABLE")
                );
            }
            {
                let state = runtime.state.lock().unwrap();
                assert_eq!(state.inspections, before + 1, "pre-inspect only");
                assert_eq!(
                    state.sandboxes[&sandbox.id], desired,
                    "control applied even without confirmation"
                );
                assert_eq!((state.pauses, state.resumes), (1, usize::from(!pause)));
            }
            assert_eq!(daemon.list().await.unwrap()[0].status.state, expected);
            let calls = runtime_calls(&runtime);
            assert_eq!(
                daemon.create(request()).await.unwrap_err().code,
                "CAPACITY_EXCEEDED"
            );
            assert_eq!(runtime_calls(&runtime), calls);
            drop(daemon);
            let (store, disk) = super::store::Store::open(directory.path()).unwrap();
            assert_eq!(disk.sandboxes[&sandbox.id].sandbox.status.state, expected);
            drop(store);
            let restarted = bounded(open(config(&directory), &runtime)).await;
            assert_eq!(restarted.list().await.unwrap()[0].status.state, confirmed);
        }
    }
}

#[tokio::test]
async fn lost_control_ack_retry_commits_observation_without_get() {
    for pause in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let daemon = open(config(&directory), &runtime).await;
        let sandbox = create(&daemon).await;
        if !pause {
            daemon.pause(&sandbox.id).await.unwrap();
        }
        runtime.state.lock().unwrap().control_reply =
            Some(Err(anyhow::anyhow!("ack lost after control")));
        assert_eq!(
            daemon.control(&sandbox.id, pause).await.unwrap_err().status,
            503
        );
        let intention = if pause { "Pausing" } else { "Resuming" };
        assert_eq!(daemon.list().await.unwrap()[0].status.state, intention);
        let calls = runtime_calls(&runtime);
        daemon.control(&sandbox.id, pause).await.unwrap();
        let confirmed = if pause { "Paused" } else { "Running" };
        assert_eq!(daemon.list().await.unwrap()[0].status.state, confirmed);
        let after = runtime_calls(&runtime);
        assert_eq!(after.1, calls.1 + 1, "retry inspects once");
        assert_eq!((after.2, after.3), (calls.2, calls.3), "no second control");
        if !pause {
            daemon.endpoint(&sandbox.id, 44772, false).await.unwrap();
        }
        drop(daemon);
        let (_store, disk) = super::store::Store::open(directory.path()).unwrap();
        assert_eq!(disk.sandboxes[&sandbox.id].sandbox.status.state, confirmed);
    }
}

#[tokio::test]
async fn control_retry_preserves_uncertainty_stopping_and_storage_fences() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    daemon.pause(&sandbox.id).await.unwrap();
    runtime.state.lock().unwrap().control_reply = Some(Err(anyhow::anyhow!("lost ACK")));
    daemon.resume(&sandbox.id).await.unwrap_err();
    runtime.state.lock().unwrap().inspect_error = true;
    daemon.resume(&sandbox.id).await.unwrap_err();
    assert_eq!(daemon.list().await.unwrap()[0].status.state, "Resuming");
    runtime.state.lock().unwrap().inspect_error = false;
    runtime
        .state
        .lock()
        .unwrap()
        .sandboxes
        .insert(sandbox.id.clone(), RuntimeState::Missing);
    assert_eq!(daemon.resume(&sandbox.id).await.unwrap_err().status, 409);
    assert_eq!(daemon.list().await.unwrap()[0].status.state, "Resuming");
    runtime
        .state
        .lock()
        .unwrap()
        .sandboxes
        .insert(sandbox.id.clone(), RuntimeState::Running);
    daemon
        .transition(&sandbox.id, "Stopping", None)
        .await
        .unwrap();
    let calls = runtime_calls(&runtime);
    assert_eq!(daemon.resume(&sandbox.id).await.unwrap_err().status, 409);
    assert_eq!(
        runtime_calls(&runtime),
        calls,
        "Stopping is not inspected or overwritten"
    );
    assert_eq!(daemon.list().await.unwrap()[0].status.state, "Stopping");
    daemon
        .storage_failed
        .store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(
        daemon.resume(&sandbox.id).await.unwrap_err().code,
        "STORAGE_UNAVAILABLE"
    );
    assert_eq!(runtime_calls(&runtime), calls);
}

#[tokio::test]
async fn shared_proxy_admission_fences_control_and_queued_writers_block_new_proxies() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    let first = daemon.proxy_guard(&sandbox.id).await.unwrap();
    let second = bounded(daemon.proxy_guard(&sandbox.id)).await.unwrap();
    let gate = daemon.operation(&sandbox.id).await.unwrap();
    let mut writer = Box::pin(gate.write());
    std::future::poll_fn(|cx| {
        assert!(writer.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), daemon.proxy_guard(&sandbox.id))
            .await
            .is_err()
    );
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), writer.as_mut())
            .await
            .is_err()
    );
    drop(second);
    let exclusive = bounded(writer).await;
    assert!(gate.try_read().is_err());
    drop(exclusive);
    daemon.pause(&sandbox.id).await.unwrap();
    assert_eq!(daemon.list().await.unwrap()[0].status.state, "Paused");
}

// Exercise actual reqwest send, not just lock acquisition. The listener retains
// sockets without responding, so each proxy is stalled waiting for headers.
#[tokio::test]
async fn concurrent_stalled_proxy_headers_bound_delete_delay() {
    use axum::{body::Body, http::Request};
    use tokio::io::AsyncReadExt;
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    runtime.state.lock().unwrap().endpoint_override =
        Some(format!("http://{}", listener.local_addr().unwrap()));
    let app = super::api::router(daemon.clone());
    let mut requests = Vec::new();
    for _ in 0..2 {
        let app = app.clone();
        let request = Request::builder()
            .uri(format!("/v1/sandboxes/{}/proxy/44772/ping", sandbox.id))
            .header("open-sandbox-api-key", API_KEY)
            .body(Body::empty())
            .unwrap();
        requests.push(tokio::spawn(app.oneshot(request)));
    }
    let (mut first, _) = bounded(listener.accept()).await.unwrap();
    let (mut second, _) = bounded(listener.accept()).await.unwrap();
    // Complete real connection establishment before advancing virtual time.
    let mut buffer = [0; 4096];
    assert!(bounded(first.read(&mut buffer)).await.unwrap() > 0);
    assert!(bounded(second.read(&mut buffer)).await.unwrap() > 0);
    let deleting = {
        let daemon = daemon.clone();
        let id = sandbox.id.clone();
        tokio::spawn(async move { daemon.delete(&id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        runtime.state.lock().unwrap().deletes,
        0,
        "in-flight sends retain reuse fence"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(119)).await;
    assert!(
        requests.iter().all(|request| !request.is_finished()),
        "preserve the 120-second header allowance"
    );
    assert_eq!(runtime.state.lock().unwrap().deletes, 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::resume();
    for request in requests {
        let response = bounded(request).await.unwrap().unwrap();
        assert_eq!(response.status(), 504);
    }
    bounded(deleting).await.unwrap().unwrap();
    assert!(daemon.list().await.unwrap().is_empty());
    drop((first, second));
}

#[tokio::test]
async fn established_proxy_response_stream_does_not_block_delete() {
    use axum::{body::Body, http::Request};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let sandbox = create(&daemon).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    runtime.state.lock().unwrap().endpoint_override =
        Some(format!("http://{}", listener.local_addr().unwrap()));
    let request = Request::builder()
        .uri(format!("/v1/sandboxes/{}/proxy/44772/events", sandbox.id))
        .header("open-sandbox-api-key", API_KEY)
        .body(Body::empty())
        .unwrap();
    let proxy = tokio::spawn(super::api::router(daemon.clone()).oneshot(request));
    let (mut connection, _) = bounded(listener.accept()).await.unwrap();
    let mut buffer = [0; 4096];
    assert!(bounded(connection.read(&mut buffer)).await.unwrap() > 0);
    bounded(connection.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
    ))
    .await
    .unwrap();
    let response = bounded(proxy).await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        daemon.proxy_slots.available_permits(),
        super::MAX_PROXY_STREAMS - 1
    );
    let occupied = Arc::clone(&daemon.proxy_slots)
        .acquire_many_owned((super::MAX_PROXY_STREAMS - 1) as u32)
        .await
        .unwrap();
    let request = Request::builder()
        .uri(format!("/v1/sandboxes/{}/proxy/44772/events", sandbox.id))
        .header("open-sandbox-api-key", API_KEY)
        .body(Body::empty())
        .unwrap();
    let denied = bounded(super::api::router(daemon.clone()).oneshot(request))
        .await
        .unwrap();
    assert_eq!(denied.status(), 429);
    drop(denied);
    // Keep both the upstream socket and unconsumed response body alive.
    bounded(daemon.delete(&sandbox.id)).await.unwrap();
    drop((response, connection));
    assert_eq!(daemon.proxy_slots.available_permits(), 1);
    drop(occupied);
    assert_eq!(
        daemon.proxy_slots.available_permits(),
        super::MAX_PROXY_STREAMS
    );
}

#[tokio::test]
async fn proxy_connections_are_not_reused_across_sandboxes_and_cancellation_releases_admission() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    runtime.state.lock().unwrap().endpoint_override =
        Some(format!("http://{}", listener.local_addr().unwrap()));
    let mut connections = Vec::new();
    for _ in 0..2 {
        let sandbox = create(&daemon).await;
        let request = Request::builder()
            .uri(format!("/v1/sandboxes/{}/proxy/44772/ping", sandbox.id))
            .header("open-sandbox-api-key", API_KEY)
            .body(Body::empty())
            .unwrap();
        let proxy = tokio::spawn(super::api::router(daemon.clone()).oneshot(request));
        // Even though the prior server socket remains open and advertises
        // keep-alive, the next sandbox must establish a fresh connection.
        let (mut connection, _) = bounded(listener.accept()).await.unwrap();
        let mut buffer = [0; 4096];
        assert!(bounded(connection.read(&mut buffer)).await.unwrap() > 0);
        bounded(connection.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
        ))
        .await
        .unwrap();
        let response = bounded(proxy).await.unwrap().unwrap();
        assert_eq!(
            bounded(to_bytes(response.into_body(), 16)).await.unwrap(),
            "ok"
        );
        assert_eq!(
            daemon.proxy_slots.available_permits(),
            super::MAX_PROXY_STREAMS
        );
        bounded(daemon.delete(&sandbox.id)).await.unwrap();
        connections.push(connection);
    }
    let sandbox = create(&daemon).await;
    let request = Request::builder()
        .uri(format!("/v1/sandboxes/{}/proxy/44772/ping", sandbox.id))
        .header("open-sandbox-api-key", API_KEY)
        .body(Body::empty())
        .unwrap();
    let proxy = tokio::spawn(super::api::router(daemon.clone()).oneshot(request));
    let (connection, _) = bounded(listener.accept()).await.unwrap();
    proxy.abort();
    assert!(bounded(proxy).await.unwrap_err().is_cancelled());
    assert_eq!(
        daemon.proxy_slots.available_permits(),
        super::MAX_PROXY_STREAMS
    );
    bounded(daemon.delete(&sandbox.id)).await.unwrap();
    drop((connections, connection));
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
    // The helper waits for the background startup pass; list itself does not inspect.
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
async fn pending_restart_releases_only_confirmed_absence_and_keeps_live_or_uncertain_capacity() {
    for outcome in ["missing", "running", "unknown", "delete-failed"] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Arc::new(FakeRuntime::default());
        let mut cfg = config(&directory);
        cfg.max_sandboxes = 1;
        let daemon = open(cfg, &runtime).await;
        let sandbox = create(&daemon).await;
        daemon
            .transition(&sandbox.id, "Pending", None)
            .await
            .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            if outcome != "running" {
                state.sandboxes.remove(&sandbox.id);
            }
            state.inspect_error = outcome == "unknown";
            if outcome == "delete-failed" {
                state.delete_outcome = DeleteOutcome::Error;
            }
        }
        drop(daemon);
        let mut cfg = config(&directory);
        cfg.max_sandboxes = 1;
        let daemon = bounded(open(cfg, &runtime)).await;
        let records = daemon.list().await.unwrap();
        if outcome == "missing" {
            assert!(records.is_empty());
            bounded(create(&daemon)).await;
        } else {
            assert_eq!(records.len(), 1, "{outcome}");
            assert_eq!(
                records[0].status.state,
                match outcome {
                    "running" => "Running",
                    "unknown" => "Pending",
                    _ => "Stopping",
                }
            );
            assert_eq!(
                bounded(daemon.create(request())).await.unwrap_err().status,
                429
            );
            if outcome != "delete-failed" {
                assert_eq!(runtime.state.lock().unwrap().deletes, 0);
            }
            if outcome == "delete-failed" {
                runtime.state.lock().unwrap().delete_outcome = DeleteOutcome::Confirmed;
                drop(daemon);
                let mut cfg = config(&directory);
                cfg.max_sandboxes = 1;
                let daemon = bounded(open(cfg, &runtime)).await;
                assert!(daemon.list().await.unwrap().is_empty());
                bounded(create(&daemon)).await;
            }
        }
    }
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
        endpoint.endpoint,
        format!("localhost:8080/v1/sandboxes/{}/proxy/44772", first.id)
    );
    assert_eq!(
        daemon.upstream(&first.id, 44772).await.unwrap(),
        "http://127.0.0.1:44772"
    );
    let token = &endpoint.headers.as_ref().unwrap()["X-PVISOR-SANDBOX-TOKEN"];
    assert!(token.len() >= 32);
    let second_endpoint = daemon.endpoint(&second.id, 44772, false).await.unwrap();
    assert_ne!(endpoint.headers, second_endpoint.headers);
    assert!(
        daemon
            .endpoint(&first.id, 44772, true)
            .await
            .unwrap()
            .headers
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
        daemon
            .endpoint(&first.id, 44772, false)
            .await
            .unwrap()
            .headers,
        endpoint.headers
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
            e2b: None,
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
        if matches!(
            case,
            BeforeIntention | Intention | DeleteIntention | Unlinked
        ) {
            assert!(restored.is_empty());
            bounded(create(&restarted)).await;
        } else {
            assert_eq!(restored.len(), 1);
            let saved = disk.sandboxes.values().next().unwrap();
            assert_eq!(restored[0].id, saved.sandbox.id);
            assert_eq!(
                restored[0].status.state,
                match case {
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
    assert!(operation.try_write().is_err());
    native_release.send(()).unwrap();
    let finished = bounded(operation.write()).await;
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
    let mut actual = state.specs[0].env.clone();
    let credential = actual.remove("PVISOR_EXECD_ACCESS_TOKEN").unwrap();
    assert_eq!(credential.len(), 64);
    assert!(credential.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(actual, expected_env);
}

#[tokio::test]
async fn execd_credentials_are_generated_per_sandbox_and_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let daemon = open(config(&directory), &runtime).await;
    let mut payload = request_json();
    payload["env"] = json!({"PVISOR_EXECD_ACCESS_TOKEN": "caller-chosen"});
    let first = daemon
        .create(serde_json::from_value(payload).unwrap())
        .await
        .unwrap();
    let second = create(&daemon).await;
    let a = daemon.record(&first.id).await.unwrap().env["PVISOR_EXECD_ACCESS_TOKEN"].clone();
    let b = daemon.record(&second.id).await.unwrap().env["PVISOR_EXECD_ACCESS_TOKEN"].clone();
    assert_ne!(a, "caller-chosen");
    assert_ne!(a, b);
    assert_eq!(a.len(), 64);
    drop(daemon);
    let daemon = open(config(&directory), &runtime).await;
    assert_eq!(
        daemon.record(&first.id).await.unwrap().env["PVISOR_EXECD_ACCESS_TOKEN"],
        a
    );
    assert_eq!(
        daemon.record(&second.id).await.unwrap().env["PVISOR_EXECD_ACCESS_TOKEN"],
        b
    );
}
