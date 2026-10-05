use pvisor_cluster::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};
use std::collections::BTreeMap;

fn resources(slots: u32) -> Resources {
    Resources {
        slots,
        memory_bytes: 64 * 1024 * 1024 * slots as u64,
        cpu_millis: 250 * slots as u64,
    }
}
fn class() -> ExecutionClass {
    ExecutionClass {
        executor: ExecutorKind::Process,
        isolation: IsolationKind::HostProcess,
    }
}
fn spec(id: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "test", "/bin/true");
    let RunInvocation::Process(p) = &mut run.invocation;
    p.inherit_env = false;
    run.runtime.max_output_bytes = 1024;
    TaskSpec {
        retain_artifacts: None,
        gateway: None,
        cpu_qos: None,
        restore: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "tenant".into(),
        run,
        execution: class(),
        resources: resources(1),
        labels: BTreeMap::new(),
        cache_keys: Vec::new(),
        retain_bundle: false,
        environment: None,
    }
}
fn worker(id: &str, slots: u32) -> WorkerRegistration {
    WorkerRegistration {
        checkpoint_storage: None,
        artifact_export: None,
        gateway: None,
        cpu_observation_protocol: None,
        cpu_qos_classes: vec![],
        execution_restore_protocol: None,
        parked_execution_suspend_protocol: None,
        version: CLUSTER_VERSION,
        id: id.into(),
        incarnation: "epoch-1".into(),
        capacity: resources(slots),
        execution: vec![class()],
        vm_control_protocol: None,
        vm_control_actions: Vec::new(),
        artifact_protocol: None,
        environment_support: None,
        labels: BTreeMap::new(),
        cache_keys: Vec::new(),
    }
}
fn poll(id: &str, slots: u32, active: Vec<LeaseKey>) -> PollRequest {
    PollRequest {
        worker_id: id.into(),
        incarnation: "epoch-1".into(),
        active,
        available: resources(slots),
        max_assignments: 64,
        admission: None,
    }
}
fn config() -> SchedulerConfig {
    SchedulerConfig {
        lease_duration_ms: 1000,
        ..Default::default()
    }
}
fn finish(key: LeaseKey) -> Completion {
    Completion {
        key,
        result: None,
        error: Some("test error".into()),
        artifacts: None,
        artifact_error: None,
    }
}

#[test]
fn compact_graph_topology_keeps_full_spec_order_exact_retry_and_replay() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph-wal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut graph = graph_spec(
        "rich-graph",
        &[
            ("z-root", &[]),
            ("a-child", &["z-root"]),
            ("m-child", &["z-root"]),
        ],
    );
    for (index, node) in graph.nodes.iter_mut().enumerate() {
        node.task.run.input =
            serde_json::json!({"prompt":"x".repeat(32_768), "order":[index, 2, 1]});
        node.task
            .run
            .metadata
            .insert("custom".into(), serde_json::json!({"nested":{"key":index}}));
        node.task
            .labels
            .insert("node-label".into(), index.to_string());
        node.task.cache_keys.push(format!("cache-{index}"));
    }
    let original = serde_json::to_value(&graph).unwrap();
    assert_eq!(
        serde_json::to_value(s.submit_graph(graph.clone(), 10).unwrap().spec).unwrap(),
        original
    );
    assert_eq!(s.task_records().len(), 3);
    assert_eq!(
        s.task_records()
            .map(|task| task.spec.id.as_str())
            .collect::<Vec<_>>(),
        ["a-child", "m-child", "z-root"]
    );
    s.cancel_graph("rich-graph", 11).unwrap();
    let unchanged_wal = std::fs::read(&path).unwrap();
    assert_eq!(
        serde_json::to_value(s.submit_graph(graph.clone(), 12).unwrap().spec).unwrap(),
        original
    );
    let mut mutations = Vec::new();
    let mut changed = graph.clone();
    changed.nodes.swap(1, 2);
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes[1].depends_on = vec!["m-child".into()];
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes[0].task.run.input["order"] = serde_json::json!([1, 2, 3]);
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes[0].task.run.metadata.insert(
        "custom".into(),
        serde_json::json!({"nested":{"key":"different"}}),
    );
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes[0].task.labels.clear();
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes[0].task.cache_keys.clear();
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.tenant = "different".into();
    mutations.push(changed);
    let mut changed = graph.clone();
    changed.nodes.pop();
    mutations.push(changed);
    for changed in mutations {
        assert!(s.submit_graph(changed, 13).is_err());
    }
    assert_eq!(std::fs::read(&path).unwrap(), unchanged_wal);
    drop(s);
    let mut reopened = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.graph("rich-graph").unwrap().spec).unwrap(),
        original
    );
    assert_eq!(
        serde_json::to_value(reopened.submit_graph(graph, 14).unwrap().spec).unwrap(),
        original
    );
    assert_eq!(std::fs::read(&path).unwrap(), unchanged_wal);
}

#[test]
fn cancelled_history_never_consumes_ready_window_and_counts_replay_every_transition() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let config = SchedulerConfig {
        queue_lookahead: 2,
        lease_duration_ms: 1000,
        ..Default::default()
    };
    let mut s = Scheduler::open(&path, config.clone()).unwrap();
    let mut ids = Vec::new();
    for graph in 0..8 {
        let nodes = (0..256)
            .map(|node| {
                let id = format!("history-{graph}-{node}");
                ids.push(id.clone());
                TaskGraphNode {
                    task: spec(&id),
                    depends_on: vec![],
                }
            })
            .collect();
        let id = format!("history-graph-{graph}");
        s.submit_graph(
            TaskGraphSpec {
                version: CLUSTER_VERSION,
                id: id.clone(),
                tenant: "tenant".into(),
                nodes,
            },
            0,
        )
        .unwrap();
        s.cancel_graph(&id, 1).unwrap();
    }
    for id in ["ready-a", "ready-b", "ready-c"] {
        s.submit(spec(id), 2).unwrap();
        ids.push(id.into());
    }
    let verify = |s: &Scheduler| {
        let mut expected = BTreeMap::new();
        for id in &ids {
            *expected
                .entry(format!("{:?}", s.task(id).unwrap().phase).to_lowercase())
                .or_insert(0) += 1;
        }
        assert_eq!(s.counts(), expected);
    };
    verify(&s);
    s.register(worker("node", 2), 3).unwrap();
    let assignments = s.poll(poll("node", 2, vec![]), 4).unwrap().assignments;
    assert_eq!(
        assignments
            .iter()
            .map(|a| a.spec.id.as_str())
            .collect::<Vec<_>>(),
        ["ready-a", "ready-b"]
    );
    let a = assignments[0].lease.key.clone();
    let b = assignments[1].lease.key.clone();
    s.decline(
        AdmissionRejection {
            key: a,
            reason: "test temporary admission rejection".into(),
        },
        5,
    )
    .unwrap();
    verify(&s);
    drop(s);
    let mut s = Scheduler::open(&path, config.clone()).unwrap();
    verify(&s);
    let assignments = s
        .poll(poll("node", 1, vec![b.clone()]), 6)
        .unwrap()
        .assignments;
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].spec.id, "ready-c");
    let c = assignments[0].lease.key.clone();
    verify(&s);
    s.complete(finish(c), 7).unwrap();
    verify(&s);
    s.cancel("ready-a", 8).unwrap();
    s.cancel("ready-b", 9).unwrap();
    verify(&s);
    s.complete(finish(b), 10).unwrap();
    verify(&s);
    let expected = BTreeMap::from([("cancelled".into(), 2050), ("failed".into(), 1)]);
    assert_eq!(s.counts(), expected);
    drop(s);
    assert_eq!(Scheduler::open(&path, config).unwrap().counts(), expected);
}

#[test]
fn restart_delivery_renews_only_known_terminal_attempts_and_never_redelivers_unknown_work() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("node", 2), 0).unwrap();
    s.submit(spec("known"), 0).unwrap();
    s.submit(spec("unknown"), 0).unwrap();
    let assignments = s.poll(poll("node", 2, vec![]), 1).unwrap().assignments;
    let known = assignments
        .iter()
        .find(|a| a.spec.id == "known")
        .unwrap()
        .lease
        .key
        .clone();
    let unknown = assignments
        .iter()
        .find(|a| a.spec.id == "unknown")
        .unwrap()
        .lease
        .key
        .clone();
    s.submit(spec("queued"), 2).unwrap();
    let request = RecoveryRequest {
        worker_id: "node".into(),
        incarnation: "epoch-1".into(),
        completed: vec![known.clone()],
    };
    let first = s.recover(request.clone(), 900).unwrap();
    assert_eq!(first.renewed, vec![known.clone()]);
    assert!(first.stop.is_empty());
    assert_eq!(s.task("unknown").unwrap().phase, TaskPhase::Leased);
    assert_eq!(
        s.task("unknown")
            .unwrap()
            .lease
            .as_ref()
            .unwrap()
            .expires_at_ms,
        1001
    );
    assert_eq!(s.task("queued").unwrap().phase, TaskPhase::Queued);
    assert_eq!(s.workers()[0].reserved, resources(2));
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut invalid = request.clone();
    invalid.completed.push(known.clone());
    assert!(s.recover(invalid, 950).is_err());
    let mut invalid = request.clone();
    invalid.completed[0].worker_id = "another".into();
    assert!(s.recover(invalid, 950).is_err());
    let next = s.recover(request.clone(), 1100).unwrap();
    assert_eq!(next.renewed, vec![known.clone()]);
    assert_eq!(s.task("unknown").unwrap().phase, TaskPhase::Lost);
    assert_eq!(
        s.task("unknown").unwrap().lease.as_ref().unwrap().key,
        unknown
    );
    assert_eq!(s.task("queued").unwrap().phase, TaskPhase::Queued);
    assert_eq!(s.workers()[0].reserved, resources(1));
    let mut fresh = worker("node", 2);
    fresh.incarnation = "epoch-2".into();
    assert!(s.register(fresh.clone(), 1101).is_err());
    s.complete(finish(known), 1102).unwrap();
    s.register(fresh, 1103).unwrap();
    assert!(s.recover(request, 1104).is_err());
    assert_eq!(s.task("queued").unwrap().phase, TaskPhase::Queued);
}

fn environment_template() -> EnvironmentTemplate {
    let layer = |key: &str, revision: &str| EnvironmentLayer {
        handle: format!(
            "pvisor-v1:{}:linux-amd64:{}",
            key.repeat(64),
            revision.repeat(64)
        ),
        manifest_digest: format!("sha256:{}", revision.repeat(64)),
    };
    EnvironmentTemplate {
        version: CLUSTER_VERSION,
        architecture: "amd64".into(),
        base: layer("a", "1"),
        workspace: Some(layer("b", "2")),
        toolkits: vec![layer("c", "3")],
    }
}

#[test]
fn immutable_environment_layers_are_independent_fenced_by_capability_and_replayed_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let original = s.publish_environment(environment_template()).unwrap();
    assert_eq!(
        s.publish_environment(environment_template()).unwrap(),
        original
    );
    let mut update = environment_template();
    update.toolkits[0].handle = update.toolkits[0]
        .handle
        .replace(&"3".repeat(64), &"4".repeat(64));
    update.toolkits[0].manifest_digest = format!("sha256:{}", "4".repeat(64));
    let updated = s.publish_environment(update).unwrap();
    assert_ne!(original.digest, updated.digest);
    assert_eq!(original.template.base, updated.template.base);
    assert_eq!(original.template.workspace, updated.template.workspace);
    assert_eq!(s.environment(&original.digest).unwrap(), original);
    let mut task = spec("env");
    task.environment = Some(original.digest.clone());
    assert!(
        s.submit(task.clone(), 0).is_err(),
        "host execution must not ignore environment layers"
    );
    task.execution = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    let mut unknown = task.clone();
    unknown.environment = Some("0".repeat(64));
    assert!(s.submit(unknown, 0).is_err());
    let mut forged = task.clone();
    forged.run.metadata.insert(
        "pvisor.vm.workspace_overlay".into(),
        serde_json::json!({"lowers":["/"]}),
    );
    assert!(s.submit(forged, 0).is_err());
    s.submit(task.clone(), 0).unwrap();
    let mut legacy = worker("w", 1);
    legacy.execution = vec![task.execution.clone()];
    s.register(legacy.clone(), 0).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![]), 1)
            .unwrap()
            .assignments
            .is_empty()
    );
    legacy.environment_support = Some(EnvironmentSupport {
        version: CLUSTER_VERSION,
        architecture: "arm64".into(),
    });
    s.register(legacy.clone(), 2).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![]), 3)
            .unwrap()
            .assignments
            .is_empty()
    );
    legacy.environment_support.as_mut().unwrap().architecture = "amd64".into();
    s.register(legacy, 4).unwrap();
    let assignment = s
        .poll(poll("w", 1, vec![]), 5)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.environment, Some(original.clone()));
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.environment(&updated.digest).unwrap(), updated);
    let redelivered = s
        .poll(poll("w", 0, vec![]), 6)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(redelivered.lease.key, assignment.lease.key);
    assert_eq!(redelivered.environment, assignment.environment);
    assert_eq!(s.workers()[0].reserved, resources(1));
    task.environment = Some(updated.digest);
    assert!(
        s.submit(task, 7).is_err(),
        "existing task stays pinned to original version"
    );
}

#[test]
fn environment_rejects_mutable_handles_and_affinity_uses_independent_layers() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    for handle in [
        "ubuntu:latest".to_owned(),
        "/srv/layers".into(),
        format!(
            "pvisor-v1:{}:linux-arm64-v8:{}",
            "a".repeat(64),
            "b".repeat(64)
        ),
    ] {
        let mut invalid = environment_template();
        invalid.base.handle = handle;
        assert!(s.publish_environment(invalid).is_err());
    }
    let cold = s.publish_environment(environment_template()).unwrap();
    let mut changed = environment_template();
    changed.workspace.as_mut().unwrap().handle = changed
        .workspace
        .as_ref()
        .unwrap()
        .handle
        .replace(&"2".repeat(64), &"5".repeat(64));
    let warm = s.publish_environment(changed).unwrap();
    let mut registration = worker("w", 1);
    registration.execution = vec![ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    }];
    registration.environment_support = Some(EnvironmentSupport {
        version: CLUSTER_VERSION,
        architecture: "amd64".into(),
    });
    registration.cache_keys = vec![warm.template.workspace.as_ref().unwrap().handle.clone()];
    for (id, environment) in [("cold", cold), ("warm", warm)] {
        let mut task = spec(id);
        task.execution = registration.execution[0].clone();
        task.environment = Some(environment.digest);
        s.submit(task, 0).unwrap();
    }
    s.register(registration, 0).unwrap();
    assert_eq!(
        s.poll(poll("w", 1, vec![]), 1).unwrap().assignments[0]
            .spec
            .id,
        "warm"
    );
}

fn native_result(id: &str) -> pvisor_core::RunResult {
    serde_json::from_value(serde_json::json!({
        "run_id": id, "attempt_id": "native-attempt", "state": "completed",
        "started_at_unix_ms": 2, "finished_at_unix_ms": 3, "exit_code": 0
    }))
    .unwrap()
}

fn archive(s: &Scheduler, key: &LeaseKey, result: &pvisor_core::RunResult) -> ArtifactManifest {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": 4, "run": result
    }))
    .unwrap();
    let chunk = s.artifact_store().put(&bytes).unwrap();
    ArtifactManifest {
        version: CLUSTER_VERSION,
        key: key.clone(),
        files: vec![ArtifactFile {
            name: "run-bundle.json".into(),
            bytes: bytes.len() as u64,
            digest: chunk.digest.clone(),
            chunks: vec![chunk],
        }],
    }
}

fn manifest_ref(s: &Scheduler, manifest: &ArtifactManifest) -> BlobRef {
    s.artifact_store()
        .put(&serde_json::to_vec(manifest).unwrap())
        .unwrap()
}

#[test]
fn required_archive_checks_capability_integrity_identity_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    let mut task = spec("archived");
    task.retain_bundle = true;
    s.submit(task, 0).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![]), 1)
            .unwrap()
            .assignments
            .is_empty()
    );
    let mut supported = worker("new", 1);
    supported.artifact_protocol = Some(CLUSTER_VERSION);
    s.register(supported, 1).unwrap();
    let key = s
        .poll(poll("new", 1, vec![]), 2)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let result = native_result("archived");
    let mut completion = Completion {
        key: key.clone(),
        result: Some(result.clone()),
        error: None,
        artifacts: None,
        artifact_error: None,
    };
    assert!(s.complete(completion.clone(), 3).is_err());
    let manifest = archive(&s, &key, &result);
    let mut wrong = manifest.clone();
    wrong.key.generation += 1;
    completion.artifacts = Some(manifest_ref(&s, &wrong));
    assert!(s.complete(completion.clone(), 4).is_err());
    wrong = manifest.clone();
    wrong.files[0].chunks[0].digest = "0".repeat(64);
    completion.artifacts = Some(manifest_ref(&s, &wrong));
    assert!(s.complete(completion.clone(), 5).is_err());
    wrong = manifest.clone();
    wrong.files[0].digest = "0".repeat(64);
    completion.artifacts = Some(manifest_ref(&s, &wrong));
    assert!(s.complete(completion.clone(), 6).is_err());
    wrong = manifest.clone();
    wrong.files[0].name = "../run-bundle.json".into();
    completion.artifacts = Some(manifest_ref(&s, &wrong));
    assert!(s.complete(completion.clone(), 7).is_err());
    let mut different = result.clone();
    different.attempt_id = "other-attempt".into();
    let wrong = archive(&s, &key, &different);
    completion.artifacts = Some(manifest_ref(&s, &wrong));
    assert!(s.complete(completion.clone(), 8).is_err());
    assert_eq!(s.task("archived").unwrap().phase, TaskPhase::Leased);
    assert_eq!(s.workers()[0].reserved, resources(1));
    completion.artifacts = Some(manifest_ref(&s, &manifest));
    assert_eq!(
        s.complete(completion.clone(), 9).unwrap().phase,
        TaskPhase::Succeeded
    );
    assert!(s.authorize_artifact_upload(&key, 10).is_err());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let record = s.complete(completion.clone(), 2000).unwrap();
    assert_eq!(record.phase, TaskPhase::Succeeded);
    assert_eq!(record.artifacts, completion.artifacts);
    assert_eq!(
        s.artifact_store()
            .read_manifest(record.artifacts.as_ref().unwrap())
            .unwrap(),
        manifest
    );
    completion.artifact_error = Some("conflicting delivery".into());
    assert!(s.complete(completion, 2001).is_err());
}

#[test]
fn archive_failure_retains_native_success_without_reexecuting_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut registration = worker("w", 1);
    registration.artifact_protocol = Some(CLUSTER_VERSION);
    s.register(registration, 0).unwrap();
    let mut task = spec("unarchived");
    task.retain_bundle = true;
    s.submit(task, 0).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let completion = Completion {
        key,
        result: Some(native_result("unarchived")),
        error: None,
        artifacts: None,
        artifact_error: Some("store unavailable".into()),
    };
    let record = s.complete(completion.clone(), 2).unwrap();
    assert_eq!(record.phase, TaskPhase::Failed);
    assert_eq!(
        record.result.unwrap().state,
        pvisor_core::RunState::Completed
    );
    assert_eq!(record.artifact_error.as_deref(), Some("store unavailable"));
    assert!(record.error.is_none());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.complete(completion, 3).unwrap().phase, TaskPhase::Failed);
    assert!(
        s.poll(poll("w", 1, vec![]), 4)
            .unwrap()
            .assignments
            .is_empty()
    );
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn final_admission_rejection_requeues_only_unstarted_work_and_fences_old_generation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    s.register(worker("other", 1), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let rejection = AdmissionRejection {
        key: key.clone(),
        reason: "pressure changed before start".into(),
    };
    let task = s.decline(rejection.clone(), 2).unwrap();
    assert_eq!(task.phase, TaskPhase::Queued);
    assert_eq!(task.admission_rejections, 1);
    assert!(task.lease.is_none());
    assert_eq!(s.workers()[1].reserved, Resources::default());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        s.decline(rejection.clone(), 3)
            .unwrap()
            .admission_rejections,
        1
    );
    let next = s
        .poll(poll("other", 1, vec![]), 4)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    assert_eq!(next.generation, key.generation + 1);
    assert!(s.complete(finish(key), 5).is_err());
    assert_eq!(s.decline(rejection, 6).unwrap().lease.unwrap().key, next);
    s.poll(poll("other", 0, vec![next.clone()]), 7).unwrap();
    assert!(
        s.decline(
            AdmissionRejection {
                key: next.clone(),
                reason: "already accepted".into()
            },
            8
        )
        .is_err()
    );
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
    s.complete(finish(next), 9).unwrap();
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn node_probe_failure_blocks_new_work_but_keeps_renewal_cancellation_and_report_evidence() {
    use pvisor_cluster::admission::AdmissionPolicy;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("w", 2), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    s.submit(spec("two"), 0).unwrap();
    let mut first = poll("w", 2, vec![]);
    first.max_assignments = 1;
    let key = s.poll(first, 1).unwrap().assignments.remove(0).lease.key;
    let policy = AdmissionPolicy {
        mode: AdmissionMode::LinuxPressure,
        ..Default::default()
    };
    let mut request = poll("w", 1, vec![key.clone()]);
    let report = policy
        .report(
            resources(2),
            resources(1),
            0,
            Err("probe unavailable".into()),
        )
        .unwrap();
    request.available = report.available;
    request.admission = Some(report.clone());
    let response = s.poll(request.clone(), 900).unwrap();
    assert!(response.assignments.is_empty());
    assert_eq!(response.renewed, vec![key.clone()]);
    assert_eq!(
        s.task("one").unwrap().lease.as_ref().unwrap().expires_at_ms,
        1900
    );
    assert_eq!(s.workers()[0].reserved, resources(1));
    assert_eq!(s.workers()[0].admission.as_ref(), Some(&report));
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.workers()[0].admission_received_at_ms, Some(900));
    s.cancel("one", 901).unwrap();
    assert_eq!(s.poll(request, 902).unwrap().stop, vec![key.clone()]);
    s.complete(finish(key), 903).unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Cancelled);
    assert_eq!(s.task("two").unwrap().phase, TaskPhase::Queued);
    // An older worker can still renew without claiming measured telemetry.
    assert_eq!(
        s.poll(poll("w", 2, vec![]), 904).unwrap().assignments.len(),
        1
    );
    assert!(s.workers()[0].admission.is_none());
}

#[test]
fn controller_normalizes_stale_measurements_and_rejects_inconsistent_reports() {
    use pvisor_cluster::admission::AdmissionPolicy;
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    s.register(worker("w", 2), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let policy = AdmissionPolicy {
        mode: AdmissionMode::LinuxPressure,
        memory_reserve_bytes: 0,
        ..Default::default()
    };
    let measured = NodeMeasurements {
        local_cpu_quota_millis: None,
        system_memory_available_bytes: resources(2).memory_bytes,
        cgroup_memory_headroom_bytes: None,
        cpu_limit_millis: resources(2).cpu_millis,
        cpu_some_avg10_bps: 0,
        memory_full_avg10_bps: 0,
    };
    let report = policy
        .report(resources(2), Resources::default(), 1000, Ok(measured))
        .unwrap();
    assert_eq!(report.available, resources(2)); // worker's configured age limit is longer
    let mut request = poll("w", 2, vec![]);
    request.admission = Some(report);
    assert!(s.poll(request.clone(), 1).unwrap().assignments.is_empty());
    let recorded = s.workers()[0].admission.clone().unwrap();
    assert_eq!(recorded.available, Resources::default());
    assert!(recorded.blocked.contains(&AdmissionBlock::StaleSample));
    request.admission.as_mut().unwrap().available.slots = 0;
    assert!(s.poll(request, 2).is_err());
    assert_eq!(s.workers()[0].seen_at_ms, 1);
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Queued);
}

#[test]
fn requeue_never_leases_the_same_task_twice_in_one_batch_live_or_after_replay() {
    for restart in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let mut s = Scheduler::open(&path, config()).unwrap();
        s.register(worker("w", 4), 0).unwrap();
        s.submit(spec("one"), 0).unwrap();
        let key = s
            .poll(poll("w", 4, vec![]), 1)
            .unwrap()
            .assignments
            .remove(0)
            .lease
            .key;
        s.decline(
            AdmissionRejection {
                key,
                reason: "not started".into(),
            },
            2,
        )
        .unwrap();
        if restart {
            drop(s);
            s = Scheduler::open(&path, config()).unwrap();
        }
        let batch = s.poll(poll("w", 4, vec![]), 3).unwrap();
        assert_eq!(batch.assignments.len(), 1);
        assert_eq!(s.workers()[0].reserved, resources(1));
        assert_eq!(batch.assignments[0].lease.key.generation, 2);
    }
}

#[test]
fn cancellation_winning_unstarted_rejection_is_confirmed_through_completion() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.cancel("one", 2).unwrap();
    assert!(
        s.decline(
            AdmissionRejection {
                key: key.clone(),
                reason: "not started".into()
            },
            3
        )
        .is_err()
    );
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Cancelling);
    assert_eq!(s.workers()[0].reserved, resources(1));
    s.complete(finish(key), 4).unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Cancelled);
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn crash_recovery_preserves_reservations_and_idempotency() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("w", 2), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    s.submit(spec("two"), 0).unwrap();
    s.submit(spec("three"), 0).unwrap();
    let assigned = s.poll(poll("w", 2, vec![]), 10).unwrap().assignments;
    assert_eq!(assigned.len(), 2);
    assert_eq!(s.workers()[0].reserved.slots, 2);
    assert!(Scheduler::open(&path, config()).is_err());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.workers()[0].reserved.slots, 2);
    assert_eq!(s.submit(spec("one"), 20).unwrap().phase, TaskPhase::Leased);
    let mut conflict = spec("one");
    conflict.tenant = "other".into();
    assert!(s.submit(conflict, 20).is_err());
    let active: Vec<_> = assigned.iter().map(|a| a.lease.key.clone()).collect();
    assert!(
        s.poll(poll("w", 2, active.clone()), 30)
            .unwrap()
            .assignments
            .is_empty()
    );
    s.complete(finish(active[0].clone()), 40).unwrap();
    assert_eq!(
        s.complete(finish(active[0].clone()), 41).unwrap().phase,
        TaskPhase::Failed
    );
    let next = s.poll(poll("w", 1, vec![active[1].clone()]), 50).unwrap();
    assert_eq!(next.assignments.len(), 1);
    assert_eq!(next.assignments[0].spec.id, "three");
}

#[test]
fn lost_response_redelivery_renews_same_key_and_does_not_double_reserve() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    s.submit(spec("t"), 0).unwrap();
    let first = s
        .poll(poll("w", 1, vec![]), 0)
        .unwrap()
        .assignments
        .remove(0);
    let next = s
        .poll(poll("w", 1, vec![]), 990)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(first.lease.key, next.lease.key);
    assert_eq!(next.lease.expires_at_ms, 1990);
    assert_eq!(s.workers()[0].reserved.slots, 1);
    let acknowledged = s.poll(poll("w", 0, vec![next.lease.key]), 1000).unwrap();
    assert!(acknowledged.assignments.is_empty());
    assert_eq!(s.task("t").unwrap().phase, TaskPhase::Running);
}

#[test]
fn expiry_fences_old_incarnation_and_never_replays_unknown_effects() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    s.submit(spec("t"), 0).unwrap();
    let assignment = s
        .poll(poll("w", 1, vec![]), 0)
        .unwrap()
        .assignments
        .remove(0);
    let mut replacement = worker("w", 1);
    replacement.incarnation = "epoch-2".into();
    assert!(s.register(replacement.clone(), 999).is_err());
    assert_eq!(s.reap(1000).unwrap(), 1);
    assert_eq!(s.reap(1001).unwrap(), 0);
    assert_eq!(s.task("t").unwrap().phase, TaskPhase::Lost);
    assert_eq!(s.workers()[0].reserved.slots, 0);
    assert!(
        s.complete(finish(assignment.lease.key.clone()), 1001)
            .is_err()
    );
    s.register(replacement, 1001).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![assignment.lease.key]), 1002)
            .is_err()
    );
    drop(s);
    let s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.task("t").unwrap().phase, TaskPhase::Lost);
}

#[test]
fn cancellation_waits_for_acknowledgement_and_expiry_stays_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    s.submit(spec("q"), 0).unwrap();
    assert_eq!(s.cancel("q", 0).unwrap().phase, TaskPhase::Cancelled);
    s.submit(spec("t"), 0).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 0)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    assert_eq!(s.cancel("t", 1).unwrap().phase, TaskPhase::Cancelling);
    assert_eq!(s.workers()[0].reserved.slots, 1);
    let response = s.poll(poll("w", 0, vec![key.clone()]), 2).unwrap();
    assert_eq!(response.stop, vec![key.clone()]);
    assert_eq!(
        s.complete(finish(key), 3).unwrap().phase,
        TaskPhase::Cancelled
    );
    s.submit(spec("lost"), 3).unwrap();
    s.poll(poll("w", 1, vec![]), 3).unwrap();
    s.cancel("lost", 4).unwrap();
    s.reap(1003).unwrap();
    assert_eq!(s.task("lost").unwrap().phase, TaskPhase::Lost);
}

#[test]
fn local_admission_labels_classes_cache_affinity_and_tenant_quota() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.tenant_quotas.insert("tenant".into(), resources(1));
    let mut s = Scheduler::open(&temp.path().join("journal"), cfg).unwrap();
    let mut w = worker("w", 8);
    w.cache_keys.push("layer".into());
    s.register(w, 0).unwrap();
    let mut wrong = spec("wrong");
    wrong.labels.insert("arch".into(), "other".into());
    s.submit(wrong, 0).unwrap();
    let mut vm = spec("vm");
    vm.execution.executor = ExecutorKind::VirtualMachine;
    s.submit(vm, 0).unwrap();
    s.submit(spec("cold"), 0).unwrap();
    let mut warm = spec("warm");
    warm.cache_keys.push("layer".into());
    s.submit(warm, 0).unwrap();
    assert!(
        s.poll(poll("w", 0, vec![]), 0)
            .unwrap()
            .assignments
            .is_empty()
    );
    let assignment = s.poll(poll("w", 8, vec![]), 1).unwrap().assignments;
    assert_eq!(assignment.len(), 1);
    assert_eq!(assignment[0].spec.id, "warm");
    s.drain("w", true).unwrap();
    s.complete(finish(assignment[0].lease.key.clone()), 2)
        .unwrap();
    assert!(
        s.poll(poll("w", 8, vec![]), 3)
            .unwrap()
            .assignments
            .is_empty()
    );
    s.drain("w", false).unwrap();
    assert_eq!(
        s.poll(poll("w", 8, vec![]), 4).unwrap().assignments[0]
            .spec
            .id,
        "cold"
    );
}

#[test]
fn bounded_queue_rotation_finds_work_after_incompatible_window() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.queue_lookahead = 1;
    let mut s = Scheduler::open(&temp.path().join("journal"), cfg).unwrap();
    s.register(worker("w", 1), 0).unwrap();
    let mut big = spec("big");
    big.resources.memory_bytes *= 10;
    s.submit(big, 0).unwrap();
    s.submit(spec("small"), 0).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![]), 0)
            .unwrap()
            .assignments
            .is_empty()
    );
    assert_eq!(
        s.poll(poll("w", 1, vec![]), 1).unwrap().assignments[0]
            .spec
            .id,
        "small"
    );
}

#[test]
fn journal_repairs_only_partial_tail_and_rejects_complete_corruption() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.submit(spec("t"), 0).unwrap();
    drop(s);
    let valid_len = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"interrupted transaction")
        .unwrap();
    let s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), valid_len);
    drop(s);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"bad checksum\n")
        .unwrap();
    assert!(Scheduler::open(&path, config()).is_err());
}

#[test]
fn malicious_or_overflowing_requests_are_rejected_before_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut task = spec("../../host");
    assert!(s.submit(task.clone(), 0).is_err());
    task = spec("inherited");
    let RunInvocation::Process(p) = &mut task.run.invocation;
    p.inherit_env = true;
    assert!(s.submit(task, 0).is_err());
    task = spec("huge");
    task.run.runtime.max_output_bytes = usize::MAX;
    assert!(s.submit(task, 0).is_err());
    task = spec("forged-restore");
    task.run.metadata.insert(
        "pvisor.orchestration.execution_restore".into(),
        serde_json::json!({"store": "/host/forged"}),
    );
    assert!(s.submit(task, 0).is_err());
    assert!(s.task("forged-restore").is_err());
    s.register(worker("w", 1), 0).unwrap();
    s.submit(spec("t"), 0).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 0)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    assert!(
        s.poll(poll("w", 1, vec![key.clone(), key.clone()]), 1)
            .is_err()
    );
    let mut invalid = key;
    invalid.incarnation = "forged".into();
    assert!(s.poll(poll("w", 1, vec![invalid]), 1).is_err());
    assert_eq!(s.workers()[0].reserved.slots, 1);
}

#[test]
fn cpu_qos_contract_rejects_hidden_requests_unknown_classes_and_non_vm_execution() {
    use pvisor_core::CpuQosClass::{BestEffort, LatencySensitive};
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut task = spec("qos");
    let legacy = serde_json::to_value(&task).unwrap();
    assert!(legacy.get("cpu_qos").is_none());
    assert_eq!(
        serde_json::from_value::<TaskSpec>(legacy).unwrap().cpu_qos,
        None
    );
    task.cpu_qos = Some(BestEffort);
    assert!(scheduler.submit(task.clone(), 0).is_err());
    task.execution = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    task.cpu_qos = None;
    task.run.runtime.cpu_qos = Some(BestEffort);
    assert!(scheduler.submit(task.clone(), 0).is_err());
    task.cpu_qos = Some(LatencySensitive);
    assert!(scheduler.submit(task.clone(), 0).is_err());
    task.run.runtime.cpu_qos = Some(LatencySensitive);
    scheduler.submit(task.clone(), 0).unwrap();
    scheduler.submit(task.clone(), 0).unwrap();
    let mut conflicting = task.clone();
    conflicting.cpu_qos = Some(BestEffort);
    conflicting.run.runtime.cpu_qos = None;
    assert!(scheduler.submit(conflicting, 0).is_err());
    let mut json = serde_json::to_value(&task).unwrap();
    json["cpu_qos"] = serde_json::json!("urgent");
    assert!(serde_json::from_value::<TaskSpec>(json).is_err());
}

#[test]
fn cpu_qos_dispatch_requires_capability_and_preserves_full_budgets_and_replay() {
    use pvisor_core::CpuQosClass::{BestEffort, LatencySensitive};
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut cfg = config();
    cfg.tenant_quotas.insert("tenant".into(), resources(1));
    let mut scheduler = Scheduler::open(&journal, cfg.clone()).unwrap();
    let vm = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    let mut registration = worker("node", 4);
    registration.execution = vec![vm.clone()];
    scheduler.register(registration.clone(), 0).unwrap();
    for (id, class) in [("ls", LatencySensitive), ("be", BestEffort)] {
        let mut task = spec(id);
        task.execution = vm.clone();
        task.cpu_qos = Some(class);
        scheduler.submit(task, 0).unwrap();
    }
    assert!(
        scheduler
            .poll(poll("node", 4, vec![]), 1)
            .unwrap()
            .assignments
            .is_empty()
    );
    registration.cpu_qos_classes = vec![BestEffort];
    scheduler.register(registration.clone(), 2).unwrap();
    let assignment = scheduler
        .poll(poll("node", 4, vec![]), 3)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.spec.id, "be");
    assert_eq!(scheduler.workers()[0].reserved, resources(1));
    registration.cpu_qos_classes.push(LatencySensitive);
    scheduler.register(registration, 4).unwrap();
    let active = vec![assignment.lease.key.clone()];
    assert!(
        scheduler
            .poll(poll("node", 4, active), 5)
            .unwrap()
            .assignments
            .is_empty(),
        "QoS cannot discount tenant reservations"
    );
    scheduler.complete(finish(assignment.lease.key), 6).unwrap();
    let mut insufficient = poll("node", 1, vec![]);
    insufficient.available.cpu_millis -= 1;
    assert!(
        scheduler
            .poll(insufficient, 7)
            .unwrap()
            .assignments
            .is_empty()
    );
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, cfg).unwrap();
    let assignment = scheduler
        .poll(poll("node", 1, vec![]), 8)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.spec.cpu_qos, Some(LatencySensitive));
    assert_eq!(scheduler.workers()[0].reserved, resources(1));
}

#[test]
fn cpu_qos_registration_rejects_duplicates_and_process_only_workers() {
    use pvisor_core::CpuQosClass::{BestEffort, LatencySensitive};
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut registration = worker("node", 2);
    let json = serde_json::to_value(&registration).unwrap();
    assert!(json.get("cpu_qos_classes").is_none());
    assert!(
        serde_json::from_value::<WorkerRegistration>(json)
            .unwrap()
            .cpu_qos_classes
            .is_empty()
    );
    registration.cpu_qos_classes = vec![BestEffort];
    assert!(scheduler.register(registration.clone(), 0).is_err());
    registration.execution = vec![ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    }];
    registration.cpu_qos_classes = vec![LatencySensitive, LatencySensitive];
    assert!(scheduler.register(registration.clone(), 0).is_err());
    registration.cpu_qos_classes = vec![BestEffort, LatencySensitive, BestEffort];
    assert!(scheduler.register(registration.clone(), 0).is_err());
    registration.cpu_qos_classes = vec![BestEffort, LatencySensitive];
    scheduler.register(registration, 0).unwrap();
}

#[tokio::test]
async fn client_refuses_hidden_cpu_qos_before_contacting_an_older_controller() {
    let client = pvisor_cluster::client::Client::new(
        "http://127.0.0.1:1",
        "local-test-credential-1234567890".into(),
    )
    .unwrap();
    let mut task = spec("hidden");
    task.execution = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    task.run.runtime.cpu_qos = Some(pvisor_core::CpuQosClass::BestEffort);
    let error = client.submit(&task).await.unwrap_err();
    assert!(
        error.to_string().contains("explicit TaskSpec cpu_qos"),
        "{error}"
    );
}

#[test]
fn cpu_observation_capability_is_explicit_optional_and_requires_native_vm_support() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut registration = worker("cpu-capability", 1);
    let legacy = serde_json::to_value(&registration).unwrap();
    assert!(legacy.get("cpu_observation_protocol").is_none());
    assert_eq!(
        serde_json::from_value::<WorkerRegistration>(legacy)
            .unwrap()
            .cpu_observation_protocol,
        None
    );
    registration.cpu_observation_protocol =
        Some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION);
    assert!(scheduler.register(registration.clone(), 0).is_err());
    registration.execution = vec![ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    }];
    registration.cpu_observation_protocol =
        Some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION + 1);
    assert!(scheduler.register(registration.clone(), 0).is_err());
    registration.cpu_observation_protocol =
        Some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION);
    scheduler.register(registration.clone(), 0).unwrap();
    // Legacy live-only Workers remain valid, while future wire contracts fail
    // during registration before a task can be assigned.
    registration.cpu_observation_protocol = Some(1);
    scheduler.register(registration, 1).unwrap();
}

fn graph_spec(id: &str, nodes: &[(&str, &[&str])]) -> TaskGraphSpec {
    TaskGraphSpec {
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "tenant".into(),
        nodes: nodes
            .iter()
            .map(|(id, deps)| TaskGraphNode {
                task: spec(id),
                depends_on: deps.iter().map(|d| (*d).into()).collect(),
            })
            .collect(),
    }
}
fn graph_success(key: LeaseKey) -> Completion {
    Completion {
        result: Some(native_result(&key.task_id)),
        error: None,
        artifacts: None,
        artifact_error: None,
        key,
    }
}

#[test]
fn graph_diamond_waits_for_all_successes_and_replays_without_duplicate_execution() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let graph = graph_spec(
        "diamond",
        &[
            ("join", &["left", "right"]),
            ("left", &[]),
            ("right", &[]),
            ("tail", &["join"]),
        ],
    );
    s.submit_graph(graph.clone(), 0).unwrap();
    assert_eq!(
        s.task("join").unwrap().phase,
        TaskPhase::WaitingDependencies
    );
    s.register(worker("node", 2), 0).unwrap();
    let roots = s.poll(poll("node", 2, vec![]), 1).unwrap().assignments;
    assert_eq!(roots.len(), 2);
    assert!(
        roots
            .iter()
            .all(|a| a.spec.id == "left" || a.spec.id == "right")
    );
    s.complete(graph_success(roots[0].lease.key.clone()), 2)
        .unwrap();
    assert_eq!(
        s.task("join").unwrap().phase,
        TaskPhase::WaitingDependencies
    );
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        s.submit_graph(graph.clone(), 3).unwrap().phase,
        TaskGraphPhase::Running
    );
    s.complete(graph_success(roots[1].lease.key.clone()), 4)
        .unwrap();
    assert_eq!(s.task("join").unwrap().phase, TaskPhase::Queued);
    let join = s.poll(poll("node", 2, vec![]), 5).unwrap().assignments;
    assert_eq!(join.len(), 1);
    assert_eq!(join[0].spec.id, "join");
    let completion = graph_success(join[0].lease.key.clone());
    s.complete(completion.clone(), 6).unwrap();
    s.complete(completion, 7).unwrap();
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let tail = s.poll(poll("node", 2, vec![]), 8).unwrap().assignments;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].spec.id, "tail");
    s.complete(graph_success(tail[0].lease.key.clone()), 9)
        .unwrap();
    let record = s.graph("diamond").unwrap();
    assert_eq!(record.phase, TaskGraphPhase::Succeeded);
    assert_eq!(record.created_at_ms, 0);
    assert_eq!(record.updated_at_ms, 9);
    assert!(
        s.poll(poll("node", 2, vec![]), 10)
            .unwrap()
            .assignments
            .is_empty()
    );
    assert_eq!(s.workers()[0].reserved, Resources::default());
    assert!(record.nodes.iter().all(|n| n.phase == TaskPhase::Succeeded));
    assert!(
        record
            .spec
            .nodes
            .iter()
            .all(|n| s.task(&n.task.id).unwrap().generation == 1)
    );
    assert_eq!(
        s.cancel_graph("diamond", 11)
            .unwrap()
            .cancel_requested_at_ms,
        None
    );
}

#[test]
fn graph_failed_and_unknown_outcomes_block_descendants_but_preserve_independent_work() {
    for lost in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let mut s = Scheduler::open(&path, config()).unwrap();
        s.submit_graph(
            graph_spec(
                "failure",
                &[
                    ("root", &[]),
                    ("child", &["root"]),
                    ("tail", &["child"]),
                    ("independent", &[]),
                ],
            ),
            0,
        )
        .unwrap();
        s.register(worker("node", 1), 0).unwrap();
        // The queue follows node order, independently of lexical task IDs.
        let root = s
            .poll(poll("node", 1, vec![]), 1)
            .unwrap()
            .assignments
            .remove(0);
        assert_eq!(root.spec.id, "root");
        if lost {
            assert_eq!(s.reap(1001).unwrap(), 1);
        } else {
            s.complete(finish(root.lease.key.clone()), 2).unwrap();
        }
        let completion = graph_success(root.lease.key.clone());
        assert!(s.complete(completion, 1002).is_err());
        for id in ["child", "tail"] {
            let child = s.task(id).unwrap();
            assert_eq!(child.phase, TaskPhase::Failed);
            assert_eq!(child.generation, 0);
            assert!(child.lease.is_none() && child.reserved.is_none() && child.result.is_none());
            assert!(child.error.unwrap().contains("never executed"));
        }
        drop(s);
        let mut s = Scheduler::open(&path, config()).unwrap();
        let work = s.poll(poll("node", 1, vec![]), 1003).unwrap().assignments;
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].spec.id, "independent");
        s.complete(graph_success(work[0].lease.key.clone()), 1004)
            .unwrap();
        assert_eq!(s.graph("failure").unwrap().phase, TaskGraphPhase::Failed);
        assert_eq!(s.workers()[0].reserved, Resources::default());
    }
}

#[test]
fn graph_cancel_is_atomic_idempotent_and_keeps_live_reservations_until_acknowledged() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    // Both descendant-first and parent-first ordering must be safe on replay.
    s.submit_graph(
        graph_spec(
            "cancel",
            &[
                ("tail", &["child"]),
                ("root", &[]),
                ("child", &["root"]),
                ("other", &[]),
            ],
        ),
        0,
    )
    .unwrap();
    s.register(worker("node", 1), 0).unwrap();
    let root = s
        .poll(poll("node", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0);
    let record = s.cancel_graph("cancel", 2).unwrap();
    assert_eq!(record.phase, TaskGraphPhase::Cancelling);
    assert_eq!(record.cancel_requested_at_ms, Some(2));
    assert_eq!(s.task("root").unwrap().phase, TaskPhase::Cancelling);
    assert_eq!(s.workers()[0].reserved, resources(1));
    for id in ["tail", "child", "other"] {
        assert_eq!(s.task(id).unwrap().phase, TaskPhase::Cancelled);
        assert_eq!(s.task(id).unwrap().generation, 0);
    }
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        s.cancel_graph("cancel", 3).unwrap().cancel_requested_at_ms,
        Some(2)
    );
    assert_eq!(s.workers()[0].reserved, resources(1));
    let polled = s
        .poll(poll("node", 1, vec![root.lease.key.clone()]), 4)
        .unwrap();
    assert!(polled.assignments.is_empty());
    assert_eq!(polled.stop, vec![root.lease.key.clone()]);
    s.complete(graph_success(root.lease.key), 5).unwrap();
    assert_eq!(s.graph("cancel").unwrap().phase, TaskGraphPhase::Cancelled);
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn invalid_graphs_never_partially_publish_tasks_or_journal_records() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.submit(spec("existing"), 0).unwrap();
    let good = graph_spec("valid", &[("a", &[]), ("b", &["a"])]);
    let mut invalid = Vec::new();
    let mut bad = good.clone();
    bad.nodes[0].depends_on = vec!["b".into()];
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].depends_on.push("a".into());
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].depends_on = vec!["existing".into()];
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].depends_on = vec!["b".into()];
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.tenant = "other".into();
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.run.run_id = bad.nodes[0].task.run.run_id.clone();
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task = spec("existing");
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.resources.slots = 2;
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.environment = Some("missing".into());
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.id = "a".into();
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes.clear();
    invalid.push(bad);
    let mut bad = good.clone();
    bad.version += 1;
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[1].task.run.run_id = spec("existing").run.run_id;
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes[0]
        .task
        .run
        .metadata
        .insert("large".into(), "x".repeat(2 * 1024 * 1024).into());
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes = (0..257)
        .map(|i| TaskGraphNode {
            task: spec(&format!("wide-{i}")),
            depends_on: vec![],
        })
        .collect();
    invalid.push(bad);
    let mut bad = good.clone();
    bad.nodes = (0..92)
        .map(|i| TaskGraphNode {
            task: spec(&format!("dense-{i}")),
            depends_on: (0..i).map(|p| format!("dense-{p}")).collect(),
        })
        .collect();
    invalid.push(bad);
    let before = std::fs::metadata(&path).unwrap().len();
    for bad in invalid {
        assert!(s.submit_graph(bad, 1).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        assert!(s.graph("valid").is_err());
        assert!(s.task("a").is_err() && s.task("b").is_err());
        assert_eq!(s.counts().values().sum::<usize>(), 1);
    }
    s.submit_graph(good.clone(), 2).unwrap();
    let committed = std::fs::metadata(&path).unwrap().len();
    let mut collision = spec("standalone-collision");
    collision.run.run_id = spec("a").run.run_id;
    assert!(s.submit(collision, 3).is_err());
    assert_eq!(s.submit_graph(good.clone(), 3).unwrap().created_at_ms, 2);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), committed);
    let mut conflict = good;
    conflict.nodes[1].depends_on.clear();
    assert!(s.submit_graph(conflict, 4).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), committed);
}

#[test]
fn incomplete_graph_commit_recovery_publishes_all_nodes_or_none() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.submit(spec("existing"), 0).unwrap();
    let before = std::fs::metadata(&path).unwrap().len();
    let graph = graph_spec("atomic", &[("root", &[]), ("child", &["root"])]);
    s.submit_graph(graph.clone(), 1).unwrap();
    let end = std::fs::metadata(&path).unwrap().len();
    drop(s);
    // Simulate a crash leaving a partial final transaction, never a committed
    // acknowledgement. Recovering the previous frame cannot adopt half a graph.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(before + (end - before) / 2)
        .unwrap();
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
    assert!(s.graph("atomic").is_err());
    assert!(s.task("root").is_err() && s.task("child").is_err());
    s.submit_graph(graph, 2).unwrap();
    drop(s);
    let s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.task("root").unwrap().phase, TaskPhase::Queued);
    assert_eq!(
        s.task("child").unwrap().phase,
        TaskPhase::WaitingDependencies
    );
    assert_eq!(s.graph("atomic").unwrap().created_at_ms, 2);
}

#[test]
fn graph_retention_budget_counts_all_nodes_and_long_failure_chains_do_not_recurse() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut limited = config();
    limited.max_tasks = 2;
    let mut s = Scheduler::open(&path, limited).unwrap();
    s.submit(spec("existing"), 0).unwrap();
    assert!(
        s.submit_graph(graph_spec("too-many", &[("a", &[]), ("b", &["a"])]), 1)
            .is_err()
    );
    assert!(s.task("a").is_err());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let nodes = (0..256)
        .map(|i| TaskGraphNode {
            task: spec(&format!("n{i}")),
            depends_on: if i == 0 {
                vec![]
            } else {
                vec![format!("n{}", i - 1)]
            },
        })
        .collect();
    s.submit_graph(
        TaskGraphSpec {
            version: CLUSTER_VERSION,
            id: "chain".into(),
            tenant: "tenant".into(),
            nodes,
        },
        2,
    )
    .unwrap();
    s.cancel("n0", 3).unwrap();
    let graph = s.graph("chain").unwrap();
    assert_eq!(graph.phase, TaskGraphPhase::Failed);
    assert_eq!(graph.nodes.len(), 256);
    assert!(graph.nodes.iter().all(|n| n.phase.terminal()));
    assert!(
        graph
            .nodes
            .iter()
            .all(|n| s.task(&n.task_id).unwrap().generation == 0)
    );
}

fn gateway_requirement(level: pvisor_core::gateway::CaptureLevel) -> GatewayRequirement {
    GatewayRequirement {
        version: CLUSTER_VERSION,
        level,
        models: vec!["test-model".into()],
    }
}

#[test]
fn gateway_requirements_match_real_model_and_capture_capabilities_without_legacy_fallback() {
    use pvisor_core::gateway::CaptureLevel;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut task = spec("agent-loop");
    let legacy = serde_json::to_value(&task).unwrap();
    assert!(legacy.get("gateway").is_none());
    assert!(
        serde_json::from_value::<TaskSpec>(legacy)
            .unwrap()
            .gateway
            .is_none()
    );
    task.gateway = Some(gateway_requirement(CaptureLevel::Dialogue));
    assert!(s.submit(task.clone(), 0).is_err());
    task.run.capabilities.models = vec!["test-*".into()];
    s.submit(task.clone(), 0).unwrap();
    let mut registered = worker("node", 1);
    assert!(
        serde_json::to_value(&registered)
            .unwrap()
            .get("gateway")
            .is_none()
    );
    s.register(registered.clone(), 0).unwrap();
    assert!(
        s.poll(poll("node", 1, vec![]), 1)
            .unwrap()
            .assignments
            .is_empty()
    );
    assert_eq!(s.workers()[0].reserved, Resources::default());
    registered.gateway = Some(GatewaySupport {
        version: CLUSTER_VERSION,
        level: CaptureLevel::Full,
        model_patterns: vec!["test-*".into()],
    });
    s.register(registered.clone(), 2).unwrap();
    // More intrusive capture is not a substitute for the requested level.
    assert!(
        s.poll(poll("node", 1, vec![]), 3)
            .unwrap()
            .assignments
            .is_empty()
    );
    registered.gateway.as_mut().unwrap().level = CaptureLevel::Dialogue;
    registered.gateway.as_mut().unwrap().model_patterns = vec!["other-model".into()];
    s.register(registered.clone(), 4).unwrap();
    assert!(
        s.poll(poll("node", 1, vec![]), 5)
            .unwrap()
            .assignments
            .is_empty()
    );
    registered.gateway.as_mut().unwrap().model_patterns = vec!["test-*".into()];
    s.register(registered, 6).unwrap();
    let assignment = s
        .poll(poll("node", 1, vec![]), 7)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.spec.gateway, task.gateway);
    s.complete(graph_success(assignment.lease.key), 8).unwrap();
    drop(s);
    let s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.task("agent-loop").unwrap().spec.gateway, task.gateway);
    assert_eq!(
        s.workers()[0]
            .registration
            .gateway
            .as_ref()
            .unwrap()
            .model_patterns,
        vec!["test-*"]
    );
}

#[test]
fn gateway_invalid_versions_patterns_and_untrusted_route_fields_never_commit() {
    use pvisor_core::gateway::CaptureLevel;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut task = spec("invalid-gateway");
    task.run.capabilities.models = vec!["*".into()];
    task.gateway = Some(gateway_requirement(CaptureLevel::Summary));
    let before = std::fs::metadata(&path).unwrap().len();
    for models in [
        vec![],
        vec!["*".into()],
        vec!["test-model".into(), "test-model".into()],
        vec!["bad\nmodel".into()],
        vec!["x".repeat(257)],
    ] {
        let mut invalid = task.clone();
        invalid.gateway.as_mut().unwrap().models = models;
        assert!(s.submit(invalid, 0).is_err());
    }
    let mut invalid = task.clone();
    invalid.gateway.as_mut().unwrap().version += 1;
    assert!(s.submit(invalid, 0).is_err());
    for patterns in [
        vec![],
        vec!["test*middle".into()],
        vec!["*test*".into()],
        vec!["*".into(), "*".into()],
    ] {
        let mut node = worker("invalid", 1);
        node.gateway = Some(GatewaySupport {
            version: CLUSTER_VERSION,
            level: CaptureLevel::Summary,
            model_patterns: patterns,
        });
        assert!(s.register(node, 0).is_err());
    }
    let mut node = worker("invalid", 1);
    node.gateway = Some(GatewaySupport {
        version: CLUSTER_VERSION + 1,
        level: CaptureLevel::Summary,
        model_patterns: vec!["*".into()],
    });
    assert!(s.register(node, 0).is_err());
    let mut wire = serde_json::to_value(&task).unwrap();
    wire["gateway"]["api_key"] = "not-a-task-field".into();
    assert!(serde_json::from_value::<TaskSpec>(wire).is_err());
    task.run
        .metadata
        .insert("pvisor.orchestration.gateway".into(), true.into());
    assert!(s.submit(task, 0).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
    assert!(s.counts().is_empty() && s.workers().is_empty());
}

#[test]
fn requested_trace_requires_extended_capability_and_a_verified_manifest_with_all_requested_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let mut task = spec("extended-export");
    task.retain_artifacts = Some(ArtifactRetention {
        execution_checkpoint: None,
        version: ARTIFACT_EXPORT_VERSION,
        trace: true,
        workspace_upper: false,
    });
    assert!(
        !serde_json::to_value(spec("legacy"))
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("retain_artifacts")
    );
    assert!(!task.retain_bundle && task.requires_artifacts());
    s.submit(task.clone(), 0).unwrap();
    let mut registration = worker("w", 1);
    registration.artifact_protocol = Some(CLUSTER_VERSION);
    s.register(registration.clone(), 0).unwrap();
    assert!(
        s.poll(poll("w", 1, vec![]), 1)
            .unwrap()
            .assignments
            .is_empty()
    );
    assert_eq!(s.workers()[0].reserved, Resources::default());
    registration.artifact_export = Some(ArtifactExportSupport {
        execution_checkpoint: false,
        version: ARTIFACT_EXPORT_VERSION,
        trace: true,
        workspace_upper: false,
    });
    s.register(registration, 2).unwrap();
    let key = s
        .poll(poll("w", 1, vec![]), 3)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let result = native_result("extended-export");
    let mut manifest = archive(&s, &key, &result);
    let mut completion = Completion {
        key: key.clone(),
        result: Some(result),
        error: None,
        artifacts: Some(manifest_ref(&s, &manifest)),
        artifact_error: None,
    };
    let before = std::fs::metadata(&path).unwrap().len();
    let prior_phase = s.task("extended-export").unwrap().phase;
    assert!(s.complete(completion.clone(), 4).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
    assert_eq!(s.task("extended-export").unwrap().phase, prior_phase);
    // Protocol fixture only; actual native traces are covered by Worker tests.
    let chunk = s.artifact_store().put(b"terminal-trace-fixture").unwrap();
    manifest.files.push(ArtifactFile {
        name: "trace".into(),
        bytes: chunk.bytes,
        digest: chunk.digest.clone(),
        chunks: vec![chunk],
    });
    completion.artifacts = Some(manifest_ref(&s, &manifest));
    s.complete(completion, 5).unwrap();
    assert_eq!(
        s.task("extended-export").unwrap().phase,
        TaskPhase::Succeeded
    );
    drop(s);
    let s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        s.task("extended-export").unwrap().spec.retain_artifacts,
        task.retain_artifacts
    );
    assert_eq!(
        s.artifact_store()
            .read_manifest(
                s.task("extended-export")
                    .unwrap()
                    .artifacts
                    .as_ref()
                    .unwrap()
            )
            .unwrap(),
        manifest
    );
}

#[test]
fn invalid_export_requirements_and_capabilities_never_commit_or_read_host_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let before = std::fs::metadata(&path).unwrap().len();
    for retention in [
        ArtifactRetention {
            execution_checkpoint: None,
            version: ARTIFACT_EXPORT_VERSION + 1,
            trace: true,
            workspace_upper: false,
        },
        ArtifactRetention {
            execution_checkpoint: None,
            version: ARTIFACT_EXPORT_VERSION,
            trace: false,
            workspace_upper: false,
        },
        ArtifactRetention {
            execution_checkpoint: None,
            version: ARTIFACT_EXPORT_VERSION,
            trace: false,
            workspace_upper: true,
        },
    ] {
        let mut task = spec("invalid");
        task.retain_artifacts = Some(retention);
        assert!(s.submit(task, 0).is_err());
    }
    let mut task = spec("invalid-provenance");
    task.run.metadata.insert(
        "pvisor.orchestration.artifact_retention".into(),
        true.into(),
    );
    assert!(s.submit(task, 0).is_err());
    let mut node = worker("unsupported", 1);
    node.artifact_export = Some(ArtifactExportSupport {
        execution_checkpoint: false,
        version: ARTIFACT_EXPORT_VERSION,
        trace: true,
        workspace_upper: false,
    });
    assert!(s.register(node, 0).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
    assert!(s.counts().is_empty() && s.workers().is_empty());
}

#[test]
fn native_delivery_reuses_capacity_preserves_fences_and_keeps_dag_waiting_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut settings = config();
    settings.tenant_quotas.insert(
        "tenant".into(),
        resources(1)
            .checked_add(artifact_delivery_reservation(resources(1)).unwrap())
            .unwrap(),
    );
    let mut scheduler = Scheduler::open(&path, settings.clone()).unwrap();
    let mut node = worker("node", 1);
    node.capacity.memory_bytes *= 3;
    node.capacity.cpu_millis *= 3;
    node.artifact_protocol = Some(CLUSTER_VERSION);
    scheduler.register(node.clone(), 0).unwrap();
    let mut graph = graph_spec(
        "delivery-dag",
        &[("source", &[]), ("dependent", &["source"])],
    );
    graph.nodes[0].task.retain_bundle = true;
    scheduler.submit_graph(graph, 0).unwrap();
    let source = scheduler
        .poll(poll("node", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0);
    let request = NativeDone {
        version: ARTIFACT_DELIVERY_VERSION,
        key: source.lease.key.clone(),
        result: native_result("source"),
    };
    let before = std::fs::metadata(&path).unwrap().len();
    let mut bad = request.clone();
    bad.version += 1;
    assert!(scheduler.native_done(bad, 2).is_err());
    bad = request.clone();
    bad.key.generation += 1;
    assert!(scheduler.native_done(bad, 2).is_err());
    bad = request.clone();
    bad.result.run_id = "other".into();
    assert!(scheduler.native_done(bad, 2).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
    let receipt = scheduler.native_done(request.clone(), 2).unwrap();
    assert_eq!(
        receipt.reserved,
        artifact_delivery_reservation(source.spec.resources).unwrap()
    );
    assert_eq!(scheduler.workers()[0].reserved, receipt.reserved);
    assert_eq!(
        scheduler.task("source").unwrap().phase,
        TaskPhase::RetainingArtifacts
    );
    assert_eq!(
        scheduler.task("dependent").unwrap().phase,
        TaskPhase::WaitingDependencies
    );
    let committed = std::fs::metadata(&path).unwrap().len();
    assert_eq!(scheduler.native_done(request.clone(), 3).unwrap(), receipt);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), committed);
    drop(scheduler);
    let mut scheduler = Scheduler::open(&path, settings).unwrap();
    assert_eq!(scheduler.workers()[0].reserved, receipt.reserved);
    scheduler.submit(spec("independent"), 4).unwrap();
    let independent = scheduler
        .poll(poll("node", 1, vec![request.key.clone()]), 5)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(independent.spec.id, "independent");
    let both = scheduler
        .poll(
            poll(
                "node",
                0,
                vec![request.key.clone(), independent.lease.key.clone()],
            ),
            900,
        )
        .unwrap();
    assert_eq!(both.renewed.len(), 2); // More leases than execution slots is bounded and legal.
    assert!(both.assignments.is_empty());
    scheduler
        .complete(graph_success(independent.lease.key), 901)
        .unwrap();
    let mut finish = Completion {
        key: request.key.clone(),
        result: Some(request.result.clone()),
        error: None,
        artifacts: None,
        artifact_error: Some("export failed".into()),
    };
    finish.result.as_mut().unwrap().exit_code = Some(7);
    assert!(scheduler.complete(finish, 902).is_err());
    assert_eq!(
        scheduler.task("source").unwrap().phase,
        TaskPhase::RetainingArtifacts
    );
    let manifest = archive(&scheduler, &request.key, &request.result);
    scheduler
        .complete(
            Completion {
                key: request.key,
                result: Some(request.result),
                error: None,
                artifacts: Some(manifest_ref(&scheduler, &manifest)),
                artifact_error: None,
            },
            903,
        )
        .unwrap();
    assert_eq!(
        scheduler.task("dependent").unwrap().phase,
        TaskPhase::Queued
    );
    assert_eq!(scheduler.workers()[0].reserved, Resources::default());
}

#[test]
fn expired_native_delivery_preserves_known_result_without_reexecution_or_late_overwrite() {
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
        let mut node = worker("node", 1);
        node.artifact_protocol = Some(CLUSTER_VERSION);
        scheduler.register(node, 0).unwrap();
        let mut task = spec("source");
        task.retain_bundle = true;
        scheduler.submit(task, 0).unwrap();
        let key = scheduler
            .poll(poll("node", 1, vec![]), 1)
            .unwrap()
            .assignments
            .remove(0)
            .lease
            .key;
        let mut result = native_result("source");
        result.output.stdout = Some("x".repeat(1024));
        scheduler
            .native_done(
                NativeDone {
                    version: ARTIFACT_DELIVERY_VERSION,
                    key: key.clone(),
                    result: result.clone(),
                },
                2,
            )
            .unwrap();
        if cancel {
            scheduler.cancel("source", 3).unwrap();
        }
        let journal = temp.path().join("journal");
        let before_expiry = std::fs::metadata(&journal).unwrap().len();
        scheduler.reap(1001).unwrap();
        assert!(std::fs::metadata(&journal).unwrap().len() - before_expiry < 512);

        let record = scheduler.task("source").unwrap();
        assert_eq!(
            record.phase,
            if cancel {
                TaskPhase::Cancelled
            } else {
                TaskPhase::Failed
            }
        );
        assert_eq!(
            serde_json::to_value(&record.result).unwrap(),
            serde_json::to_value(&result).unwrap()
        );
        drop(scheduler);
        let mut scheduler = Scheduler::open(&journal, config()).unwrap();
        assert_eq!(
            serde_json::to_value(&scheduler.task("source").unwrap().result).unwrap(),
            serde_json::to_value(&result).unwrap()
        );
        assert!(record.error.is_none());
        assert!(
            record
                .artifact_error
                .unwrap()
                .contains("delivery lease expired")
        );
        assert_eq!(scheduler.workers()[0].reserved, Resources::default());
        assert!(
            scheduler
                .poll(poll("node", 1, vec![]), 1002)
                .unwrap()
                .assignments
                .is_empty()
        );
        assert!(
            scheduler
                .complete(
                    Completion {
                        key,
                        result: Some(result),
                        error: None,
                        artifacts: None,
                        artifact_error: Some("late result".into())
                    },
                    1002
                )
                .is_err()
        );
    }
}

#[test]
fn artifact_delivery_count_and_budget_bounds_keep_excess_attempt_fully_reserved() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let mut node = worker("node", 1);
    node.capacity.memory_bytes *= 70;
    node.capacity.cpu_millis *= 70;
    node.artifact_protocol = Some(CLUSTER_VERSION);
    scheduler.register(node, 0).unwrap();
    let mut active = vec![];
    for index in 0..=MAX_ARTIFACT_DELIVERIES {
        let id = format!("delivery-{index}");
        let mut task = spec(&id);
        task.retain_bundle = true;
        scheduler.submit(task, 1).unwrap();
        let assignment = scheduler
            .poll(poll("node", 1, active.clone()), 2)
            .unwrap()
            .assignments
            .remove(0);
        let request = NativeDone {
            version: ARTIFACT_DELIVERY_VERSION,
            result: native_result(&id),
            key: assignment.lease.key.clone(),
        };
        let before = std::fs::metadata(&path).unwrap().len();
        if index == MAX_ARTIFACT_DELIVERIES {
            assert!(scheduler.native_done(request, 3).is_err());
            assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
            assert_eq!(
                scheduler.task(&id).unwrap().current_reservation(),
                resources(1)
            );
        } else {
            scheduler.native_done(request, 3).unwrap();
        }
        active.push(assignment.lease.key);
    }
    assert_eq!(scheduler.workers()[0].reserved.slots, 1);
    assert_eq!(
        scheduler
            .poll(poll("node", 0, active.clone()), 4)
            .unwrap()
            .renewed
            .len(),
        65
    );
    active.push(LeaseKey {
        task_id: "unrecognized".into(),
        worker_id: "node".into(),
        incarnation: "epoch-1".into(),
        generation: 1,
    });
    assert!(scheduler.poll(poll("node", 0, active), 4).is_err());
}

#[test]
fn native_handoff_waits_for_observed_resume_and_preserves_its_success() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let class = ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    };
    let mut node = worker("node", 1);
    node.execution = vec![class.clone()];
    node.artifact_protocol = Some(CLUSTER_VERSION);
    node.vm_control_protocol = Some(CLUSTER_VERSION);
    node.vm_control_actions = vec![ControlAction::Pause, ControlAction::Resume];
    scheduler.register(node, 0).unwrap();
    let mut task = spec("source");
    task.execution = class;
    task.retain_bundle = true;
    scheduler.submit(task, 0).unwrap();
    let key = scheduler
        .poll(poll("node", 1, vec![]), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    scheduler
        .poll(poll("node", 0, vec![key.clone()]), 2)
        .unwrap();
    for (index, action, state) in [
        (0, ControlAction::Pause, pvisor_core::VmState::Paused),
        (1, ControlAction::Resume, pvisor_core::VmState::Running),
    ] {
        scheduler
            .request_control(
                "source",
                ControlRequest {
                    request_id: format!("control-{index}"),
                    action,
                },
                3 + index * 3,
            )
            .unwrap();
        let command = scheduler
            .poll(poll("node", 1, vec![key.clone()]), 4 + index * 3)
            .unwrap()
            .controls
            .remove(0);
        let request = NativeDone {
            version: ARTIFACT_DELIVERY_VERSION,
            key: key.clone(),
            result: native_result("source"),
        };
        let before = std::fs::metadata(&path).unwrap().len();
        assert!(
            scheduler
                .native_done(request.clone(), 5 + index * 3)
                .is_err()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
        scheduler
            .acknowledge_control(
                ControlAcknowledgement {
                    command: command.clone(),
                    outcome: ControlOutcome::Succeeded {
                        state,
                        memory: None,
                    },
                },
                5 + index * 3,
            )
            .unwrap();
        if action == ControlAction::Resume {
            scheduler.native_done(request, 9).unwrap();
            assert_eq!(
                scheduler
                    .acknowledge_control(
                        ControlAcknowledgement {
                            command,
                            outcome: ControlOutcome::Succeeded {
                                state,
                                memory: None
                            }
                        },
                        10
                    )
                    .unwrap()
                    .phase,
                ControlPhase::Succeeded
            );
        }
    }
    drop(scheduler);
    let scheduler = Scheduler::open(&path, config()).unwrap();
    let task = scheduler.task("source").unwrap();
    assert_eq!(task.phase, TaskPhase::RetainingArtifacts);
    assert!(
        task.controls
            .iter()
            .all(|control| control.phase == ControlPhase::Succeeded)
    );
}

#[test]
fn full_artifact_storage_defers_required_tasks_without_stopping_other_work_or_lease_renewal() {
    for object_bound in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
        let store = scheduler.artifact_store();
        store
            .set_storage_limits(ArtifactStorageLimits {
                version: CLUSTER_VERSION,
                max_bytes: (!object_bound).then_some(3),
                max_objects: object_bound.then_some(1),
            })
            .unwrap();
        store.put(b"all").unwrap();
        let mut node = worker("node", 2);
        node.artifact_protocol = Some(CLUSTER_VERSION);
        scheduler.register(node, 0).unwrap();
        let mut required = spec("required");
        required.retain_bundle = true;
        scheduler.submit(required, 0).unwrap();
        scheduler.submit(spec("ordinary"), 0).unwrap();
        let assignments = scheduler
            .poll(poll("node", 2, vec![]), 1)
            .unwrap()
            .assignments;
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].spec.id, "ordinary");
        let key = assignments[0].lease.key.clone();
        let renewed = scheduler
            .poll(poll("node", 1, vec![key.clone()]), 900)
            .unwrap();
        assert_eq!(renewed.renewed, vec![key.clone()]);
        assert!(renewed.assignments.is_empty());
        let waiting = scheduler.task("required").unwrap();
        assert_eq!(waiting.phase, TaskPhase::Queued);
        assert!(waiting.lease.is_none());
        assert_eq!(waiting.current_reservation(), Resources::default());
        // Raising the quota online reopens admission without changing Run identity.
        store
            .update_storage_limits(ArtifactStorageLimits {
                version: CLUSTER_VERSION,
                max_bytes: Some(6),
                max_objects: Some(2),
            })
            .unwrap();
        let assignments = scheduler
            .poll(poll("node", 1, vec![key]), 901)
            .unwrap()
            .assignments;
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].spec.id, "required");
        assert_eq!(assignments[0].lease.key.generation, 1);
    }
}
