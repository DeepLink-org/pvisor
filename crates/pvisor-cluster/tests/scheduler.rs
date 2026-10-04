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
