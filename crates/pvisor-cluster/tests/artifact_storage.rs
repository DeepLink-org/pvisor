//! Actual controller processes, persistent quota policy, CLI and role boundaries.
use pvisor_cluster::{client::Client, *};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
const ADMIN: &str = "storage-admin-test-012345678901";
const WORKER: &str = "storage-worker-test-012345678901";
struct Controller {
    child: Child,
    log: PathBuf,
}
impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn spawn(root: &Path, suffix: &str, limits: Option<&Path>) -> Controller {
    let log = root.join(format!("controller-{suffix}.log"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor-cluster"));
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--journal"])
        .arg(root.join(format!("shared.{suffix}")))
        .env("PVISOR_CLUSTER_TOKEN", ADMIN)
        .env("PVISOR_CLUSTER_WORKER_TOKEN", WORKER)
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(&log).unwrap()));
    if let Some(limits) = limits {
        command.arg("--artifact-limits").arg(limits);
    }
    Controller {
        child: command.spawn().unwrap(),
        log,
    }
}
async fn url(controller: &mut Controller) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                controller.child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(&controller.log).unwrap()
            );
            let log = fs::read_to_string(&controller.log).unwrap();
            if let Some(address) = log
                .lines()
                .find_map(|line| line.strip_prefix("pVisor controller listening on "))
            {
                break format!("http://{address}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn full() -> Resources {
    Resources {
        slots: 1,
        memory_bytes: 64 * 1024 * 1024,
        cpu_millis: 250,
    }
}
fn class() -> ExecutionClass {
    ExecutionClass {
        executor: ExecutorKind::Process,
        isolation: IsolationKind::HostProcess,
    }
}
async fn lease(admin: &Client, worker: &Client) -> LeaseKey {
    worker
        .register(&WorkerRegistration {
            version: CLUSTER_VERSION,
            id: "node".into(),
            incarnation: "epoch".into(),
            capacity: full(),
            execution: vec![class()],
            labels: BTreeMap::new(),
            cache_keys: vec![],
            vm_control_protocol: None,
            vm_control_actions: vec![],
            artifact_protocol: Some(CLUSTER_VERSION),
            artifact_export: None,
            gateway: None,
            cpu_observation_protocol: None,
            cpu_qos_classes: vec![],
            execution_restore_protocol: None,
            parked_execution_suspend_protocol: None,
            environment_support: None,
        })
        .await
        .unwrap();
    let mut run = RunSpec::process("storage", "fixture", "/bin/true");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    admin
        .submit(&TaskSpec {
            version: CLUSTER_VERSION,
            id: "storage".into(),
            tenant: "storage".into(),
            run,
            execution: class(),
            resources: full(),
            labels: BTreeMap::new(),
            cache_keys: vec![],
            retain_bundle: false,
            retain_artifacts: None,
            environment: None,
            gateway: None,
            restore: None,
            cpu_qos: None,
        })
        .await
        .unwrap();
    worker
        .poll(&PollRequest {
            worker_id: "node".into(),
            incarnation: "epoch".into(),
            active: vec![],
            available: full(),
            max_assignments: 1,
            admission: None,
        })
        .await
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_http_commits_and_fenced_leases_survive_two_controller_sigkills() {
    // Real HTTP and controller processes, with synthetic worker registrations.
    // This checks durable concurrency, not execution throughput or VM density.
    let temp = tempfile::tempdir().unwrap();
    let mut controller = spawn(temp.path(), "burst", None);
    let address = url(&mut controller).await;
    let admin = Client::new(&address, ADMIN.into()).unwrap();
    let worker = Client::new(&address, WORKER.into()).unwrap();
    let tasks: Vec<_> = (0..96)
        .map(|index| {
            let id = format!("burst-{index:03}");
            let mut run = RunSpec::process(id.as_str(), "burst-test", "/bin/true");
            let RunInvocation::Process(process) = &mut run.invocation;
            process.inherit_env = false;
            TaskSpec {
                version: CLUSTER_VERSION,
                id,
                tenant: "burst".into(),
                run,
                execution: class(),
                resources: full(),
                labels: BTreeMap::new(),
                cache_keys: vec![],
                retain_bundle: false,
                retain_artifacts: None,
                environment: None,
                gateway: None,
                restore: None,
                cpu_qos: None,
            }
        })
        .collect();
    let mut jobs = tokio::task::JoinSet::new();
    for task in tasks.clone() {
        let admin = admin.clone();
        jobs.spawn(async move { admin.submit(&task).await.unwrap() });
    }
    while let Some(result) = jobs.join_next().await {
        assert_eq!(result.unwrap().phase, TaskPhase::Queued);
    }
    let mut cancellations = tokio::task::JoinSet::new();
    for task in &tasks[..16] {
        let (admin, id) = (admin.clone(), task.id.clone());
        cancellations.spawn(async move { admin.cancel(&id).await.unwrap() });
    }
    while let Some(result) = cancellations.join_next().await {
        assert_eq!(result.unwrap().phase, TaskPhase::Cancelled);
    }
    let capacity = Resources {
        slots: 10,
        memory_bytes: full().memory_bytes * 10,
        cpu_millis: full().cpu_millis * 10,
    };
    let mut polls = tokio::task::JoinSet::new();
    for index in 0..8 {
        let worker = worker.clone();
        polls.spawn(async move {
            let id = format!("node-{index}");
            worker
                .register(&WorkerRegistration {
                    version: CLUSTER_VERSION,
                    id: id.clone(),
                    incarnation: "epoch".into(),
                    capacity,
                    execution: vec![class()],
                    labels: BTreeMap::new(),
                    cache_keys: vec![],
                    vm_control_protocol: None,
                    vm_control_actions: vec![],
                    artifact_protocol: None,
                    artifact_export: None,
                    gateway: None,
                    cpu_observation_protocol: None,
                    cpu_qos_classes: vec![],
                    execution_restore_protocol: None,
                    parked_execution_suspend_protocol: None,
                    environment_support: None,
                })
                .await
                .unwrap();
            worker
                .poll(&PollRequest {
                    worker_id: id,
                    incarnation: "epoch".into(),
                    active: vec![],
                    available: capacity,
                    max_assignments: 10,
                    admission: None,
                })
                .await
                .unwrap()
                .assignments
        });
    }
    let mut keys = BTreeMap::new();
    while let Some(result) = polls.join_next().await {
        let assignments = result.unwrap();
        assert_eq!(assignments.len(), 10);
        for assignment in assignments {
            assert_eq!(assignment.lease.key.generation, 1);
            assert!(
                keys.insert(assignment.spec.id, assignment.lease.key)
                    .is_none()
            );
        }
    }
    assert_eq!(keys.len(), 80);
    controller.child.kill().unwrap();
    controller.child.wait().unwrap();
    controller = spawn(temp.path(), "burst", None);
    let address = url(&mut controller).await;
    let admin = Client::new(&address, ADMIN.into()).unwrap();
    let worker = Client::new(&address, WORKER.into()).unwrap();
    for task in &tasks {
        let record = admin.task(&task.id).await.unwrap();
        if let Some(key) = keys.get(&task.id) {
            assert_eq!(record.phase, TaskPhase::Leased);
            assert_eq!(&record.lease.unwrap().key, key);
        } else {
            assert_eq!(record.phase, TaskPhase::Cancelled);
        }
    }
    assert!(
        admin
            .workers()
            .await
            .unwrap()
            .iter()
            .all(|w| w.reserved == capacity)
    );
    let mut finishes = tokio::task::JoinSet::new();
    for key in keys.values().cloned() {
        let worker = worker.clone();
        finishes.spawn(async move {
            worker
                .complete(&Completion {
                    key,
                    result: None,
                    error: Some("synthetic worker terminal failure".into()),
                    artifacts: None,
                    artifact_error: None,
                })
                .await
                .unwrap()
        });
    }
    while let Some(result) = finishes.join_next().await {
        assert_eq!(result.unwrap().phase, TaskPhase::Failed);
    }
    controller.child.kill().unwrap();
    controller.child.wait().unwrap();
    controller = spawn(temp.path(), "burst", None);
    let address = url(&mut controller).await;
    let admin = Client::new(&address, ADMIN.into()).unwrap();
    let worker = Client::new(&address, WORKER.into()).unwrap();
    for task in &tasks {
        let phase = if keys.contains_key(&task.id) {
            TaskPhase::Failed
        } else {
            TaskPhase::Cancelled
        };
        assert_eq!(admin.submit(task).await.unwrap().phase, phase);
    }
    for key in keys.values().cloned() {
        assert_eq!(
            worker
                .complete(&Completion {
                    key,
                    result: None,
                    error: Some("synthetic worker terminal failure".into()),
                    artifacts: None,
                    artifact_error: None
                })
                .await
                .unwrap()
                .phase,
            TaskPhase::Failed
        );
    }
    assert!(
        admin
            .workers()
            .await
            .unwrap()
            .iter()
            .all(|w| w.reserved == Resources::default())
    );
    let wal = fs::read_to_string(temp.path().join("shared.burst")).unwrap();
    let mut submitted = BTreeMap::<String, usize>::new();
    for frame in wal.lines() {
        let transaction: serde_json::Value =
            serde_json::from_str(frame.split_once(' ').unwrap().1).unwrap();
        for change in transaction["changes"].as_array().unwrap() {
            if change["kind"] == "submit" {
                *submitted
                    .entry(change["task"]["spec"]["id"].as_str().unwrap().into())
                    .or_default() += 1;
            }
        }
    }
    assert_eq!(submitted.len(), 96);
    assert!(submitted.values().all(|count| *count == 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_controller_cli_reports_deduplicated_usage_and_restores_limits_without_repeating_startup_flags()
 {
    let temp = tempfile::tempdir().unwrap();
    let limits = temp.path().join("limits.json");
    fs::write(&limits, r#"{"version":1,"max_bytes":8,"max_objects":1}"#).unwrap();
    let mut first = spawn(temp.path(), "one", Some(&limits));
    let first_url = url(&mut first).await;
    let admin = Client::new(&first_url, ADMIN.into()).unwrap();
    let worker = Client::new(&first_url, WORKER.into()).unwrap();
    let key = lease(&admin, &worker).await;
    let reference = worker
        .upload_artifact(&key, b"evidence".to_vec())
        .await
        .unwrap();
    assert_eq!(
        worker
            .upload_artifact(&key, b"evidence".to_vec())
            .await
            .unwrap(),
        reference
    );
    let rejected = worker
        .upload_artifact(&key, b"extra".to_vec())
        .await
        .unwrap_err();
    assert_eq!(
        rejected.downcast_ref::<reqwest::Error>().unwrap().status(),
        Some(reqwest::StatusCode::INSUFFICIENT_STORAGE)
    );
    assert_eq!(
        worker
            .artifact_storage()
            .await
            .unwrap_err()
            .downcast_ref::<reqwest::Error>()
            .unwrap()
            .status(),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-cluster"))
        .args(["--url", &first_url, "artifact-storage"])
        .env("PVISOR_CLUSTER_TOKEN", ADMIN)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut usage: ArtifactStorageUsage = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (
            usage.stored_bytes,
            usage.stored_objects,
            usage.reserved_bytes,
            usage.reserved_objects
        ),
        (8, 1, 0, 0)
    );
    let expanded = ArtifactStorageLimits {
        version: CLUSTER_VERSION,
        max_bytes: Some(16),
        max_objects: Some(2),
    };
    assert_eq!(
        worker
            .update_artifact_storage(&expanded)
            .await
            .unwrap_err()
            .downcast_ref::<reqwest::Error>()
            .unwrap()
            .status(),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    fs::write(&limits, serde_json::to_vec(&expanded).unwrap()).unwrap();
    let changed = Command::new(env!("CARGO_BIN_EXE_pvisor-cluster"))
        .args(["--url", &first_url, "artifact-storage", "--limits"])
        .arg(&limits)
        .env("PVISOR_CLUSTER_TOKEN", ADMIN)
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let receipt: ArtifactStorageUsage = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(receipt.limits, expanded);
    assert_eq!(receipt.stored_bytes, 8);
    worker
        .upload_artifact(&key, b"extra".to_vec())
        .await
        .unwrap();
    usage = admin.artifact_storage().await.unwrap();
    assert_eq!((usage.stored_bytes, usage.stored_objects), (13, 2));
    assert_eq!(usage.limits, expanded);
    drop(first);
    // Same WAL, no --artifact-limits: the durable policy is still authoritative.
    let mut second = spawn(temp.path(), "one", None);
    let second_url = url(&mut second).await;
    let admin = Client::new(&second_url, ADMIN.into()).unwrap();
    let worker = Client::new(&second_url, WORKER.into()).unwrap();
    assert_eq!(admin.artifact_storage().await.unwrap(), usage);
    assert_eq!(admin.artifact_bytes(&reference).await.unwrap(), b"evidence");
    assert!(admin.task("storage").await.unwrap().reconciliation_pending);
    worker
        .poll(&PollRequest {
            worker_id: key.worker_id.clone(),
            incarnation: key.incarnation.clone(),
            active: vec![key.clone()],
            available: Resources::default(),
            max_assignments: 0,
            admission: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker
            .upload_artifact(&key, b"evidence".to_vec())
            .await
            .unwrap(),
        reference
    );
    assert_eq!(
        worker
            .upload_artifact(&key, b"another".to_vec())
            .await
            .unwrap_err()
            .downcast_ref::<reqwest::Error>()
            .unwrap()
            .status(),
        Some(reqwest::StatusCode::INSUFFICIENT_STORAGE)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_controller_journals_cannot_bypass_shared_object_store_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut first = spawn(temp.path(), "one", None);
    let first_url = url(&mut first).await;
    // Distinct journal locks, identical journal.with_extension("artifacts").
    let mut second = spawn(temp.path(), "two", None);
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = second.child.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!status.success());
    assert!(
        fs::read_to_string(&second.log)
            .unwrap()
            .contains("artifact store already owned")
    );
    let admin = Client::new(&first_url, ADMIN.into()).unwrap();
    assert_eq!(admin.artifact_storage().await.unwrap().stored_objects, 0);
}

fn cli(url: &str, arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-cluster"))
        .args(["--url", url])
        .args(arguments)
        .env("PVISOR_CLUSTER_TOKEN", ADMIN)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_gc_cli_protects_live_uploads_and_downloads_reclaims_quota_and_preserves_receipts_after_restart()
 {
    let temp = tempfile::tempdir().unwrap();
    let mut first = spawn(temp.path(), "one", None);
    let endpoint = url(&mut first).await;
    let admin = Client::new(&endpoint, ADMIN.into()).unwrap();
    let worker = Client::new(&endpoint, WORKER.into()).unwrap();
    let key = lease(&admin, &worker).await;
    let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
        "run_id":"storage", "attempt_id":"native", "state":"completed", "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
    })).unwrap();
    let bundle_ref = worker
        .upload_artifact(
            &key,
            serde_json::to_vec(&serde_json::json!({"schema_version":4,"run":result})).unwrap(),
        )
        .await
        .unwrap();
    let binary: Vec<_> = (0..(2 * ARTIFACT_CHUNK_BYTES + 129))
        .map(|i| ((i * 7) % 251) as u8)
        .collect();
    let mut chunks = vec![];
    for chunk in binary.chunks(ARTIFACT_CHUNK_BYTES) {
        chunks.push(worker.upload_artifact(&key, chunk.to_vec()).await.unwrap());
    }
    let orphan = worker
        .upload_artifact(&key, b"unfinished discarded upload".to_vec())
        .await
        .unwrap();
    let manifest = ArtifactManifest {
        version: CLUSTER_VERSION,
        key: key.clone(),
        files: vec![
            ArtifactFile {
                name: "run-bundle.json".into(),
                bytes: bundle_ref.bytes,
                digest: bundle_ref.digest.clone(),
                chunks: vec![bundle_ref],
            },
            ArtifactFile {
                name: "binary.bin".into(),
                bytes: binary.len() as u64,
                digest: blake3::hash(&binary).to_hex().to_string(),
                chunks,
            },
        ],
    };
    let reference = worker
        .upload_artifact(&key, serde_json::to_vec(&manifest).unwrap())
        .await
        .unwrap();
    let live: ArtifactGcPlan = serde_json::from_value(cli(&endpoint, &["artifact-gc"])).unwrap();
    assert!(live.objects.is_empty());
    assert!(live.retire.is_empty());
    cli(&endpoint, &["artifact-gc", "--apply", &live.id]);
    assert!(admin.artifact_bytes(&orphan).await.is_ok());
    assert_eq!(
        worker
            .plan_artifact_gc(&ArtifactGcRequest {
                version: 1,
                retire_before_ms: None,
                max_objects: 1
            })
            .await
            .unwrap_err()
            .downcast_ref::<reqwest::Error>()
            .unwrap()
            .status(),
        Some(reqwest::StatusCode::UNAUTHORIZED)
    );
    let completion = Completion {
        key,
        result: Some(result),
        error: None,
        artifacts: Some(reference.clone()),
        artifact_error: None,
    };
    let finished = worker.complete(&completion).await.unwrap();
    let download = admin.begin_artifact_download("storage").await.unwrap();
    assert_eq!(download.manifest, manifest);
    let usage = admin.artifact_storage().await.unwrap();
    admin
        .update_artifact_storage(&ArtifactStorageLimits {
            version: 1,
            max_bytes: Some(usage.stored_bytes),
            max_objects: Some(usage.stored_objects),
        })
        .await
        .unwrap();
    let mut queued = finished.spec.clone();
    queued.id = "after-gc".into();
    queued.retain_bundle = true;
    queued.run = RunSpec::process("after-gc", "quota admission fixture", "/bin/true");
    let RunInvocation::Process(process) = &mut queued.run.invocation;
    process.inherit_env = false;
    admin.submit(&queued).await.unwrap();
    let poll = PollRequest {
        worker_id: "node".into(),
        incarnation: "epoch".into(),
        active: vec![],
        available: full(),
        max_assignments: 1,
        admission: None,
    };
    assert!(worker.poll(&poll).await.unwrap().assignments.is_empty());
    let cutoff = (finished.updated_at_ms + 1).to_string();
    let preview: ArtifactGcPlan = serde_json::from_value(cli(
        &endpoint,
        &["artifact-gc", "--retire-before-ms", &cutoff],
    ))
    .unwrap();
    assert_eq!(preview.retire.len(), 1);
    assert_eq!(preview.objects, vec![orphan.clone()]);
    assert!(
        admin
            .task("storage")
            .await
            .unwrap()
            .artifact_retired_at_ms
            .is_none()
    );
    let report: ArtifactGcReport =
        serde_json::from_value(cli(&endpoint, &["artifact-gc", "--apply", &preview.id])).unwrap();
    assert_eq!((report.retired_tasks, report.deleted_objects), (1, 1));
    let assigned = worker.poll(&poll).await.unwrap().assignments.remove(0);
    assert_eq!(assigned.spec.id, "after-gc");
    assert_eq!(
        admin
            .artifacts("storage")
            .await
            .unwrap_err()
            .downcast_ref::<reqwest::Error>()
            .unwrap()
            .status(),
        Some(reqwest::StatusCode::GONE)
    );
    let retired = worker.complete(&completion).await.unwrap();
    assert_eq!(retired.phase, TaskPhase::Succeeded);
    assert!(retired.artifact_retired_at_ms.is_some());
    drop(first);
    let mut second = spawn(temp.path(), "one", None);
    let endpoint = url(&mut second).await;
    let admin = Client::new(&endpoint, ADMIN.into()).unwrap();
    let worker = Client::new(&endpoint, WORKER.into()).unwrap();
    assert!(admin.apply_artifact_gc(&preview.id).await.is_err());
    assert_eq!(
        admin
            .renew_artifact_download(&download)
            .await
            .unwrap()
            .manifest,
        manifest
    );
    let mut received = Vec::new();
    for chunk in &manifest.files[1].chunks {
        received.extend(admin.artifact_bytes(chunk).await.unwrap());
    }
    assert_eq!(received, binary);
    admin.release_artifact_download(&download.id).await.unwrap();
    assert!(admin.task("after-gc").await.unwrap().reconciliation_pending);
    // This synthetic Worker never executed the assignment. Explicitly resolve
    // it; stale disk deadlines alone must not authorize destructive GC.
    assert!(
        admin
            .plan_artifact_gc(&ArtifactGcRequest {
                version: 1,
                retire_before_ms: None,
                max_objects: 4096,
            })
            .await
            .is_err()
    );
    assert_eq!(
        admin.resolve_lost(&assigned.lease.key).await.unwrap().phase,
        TaskPhase::Lost
    );
    let preview = admin
        .plan_artifact_gc(&ArtifactGcRequest {
            version: 1,
            retire_before_ms: None,
            max_objects: 4096,
        })
        .await
        .unwrap();
    assert!(!preview.objects.is_empty());
    admin.apply_artifact_gc(&preview.id).await.unwrap();
    assert_eq!(admin.artifact_storage().await.unwrap().stored_objects, 0);
    assert_eq!(
        worker
            .complete(&completion)
            .await
            .unwrap()
            .artifact_retired_at_ms,
        retired.artifact_retired_at_ms
    );
    assert!(admin.begin_artifact_download("storage").await.is_err());
    assert!(admin.artifact_bytes(&reference).await.is_err());
}
