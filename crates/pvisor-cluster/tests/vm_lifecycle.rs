//! Protocol/fault tests use synthetic VM observations. They do not substitute
//! for a hardware-backed worker lifecycle or density experiment.
use pvisor_cluster::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec, VmMemory, VmState};
use std::collections::BTreeMap;

fn full() -> Resources {
    Resources {
        slots: 1,
        memory_bytes: 64 * 1024 * 1024,
        cpu_millis: 250,
    }
}
fn execution() -> ExecutionClass {
    ExecutionClass {
        executor: ExecutorKind::VirtualMachine,
        isolation: IsolationKind::VirtualMachine,
    }
}
fn spec(id: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "vm", "/bin/true");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
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
        execution: execution(),
        resources: full(),
        labels: BTreeMap::new(),
        cache_keys: vec![],
        retain_bundle: false,
        environment: None,
    }
}
fn worker() -> WorkerRegistration {
    WorkerRegistration {
        checkpoint_storage: None,
        artifact_export: None,
        gateway: None,
        cpu_observation_protocol: None,
        cpu_qos_classes: vec![],
        execution_restore_protocol: None,
        parked_execution_suspend_protocol: None,
        version: CLUSTER_VERSION,
        id: "worker".into(),
        incarnation: "epoch".into(),
        capacity: Resources {
            slots: 2,
            memory_bytes: 2 * full().memory_bytes,
            cpu_millis: full().cpu_millis,
        },
        execution: vec![execution()],
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
    }
}
fn config() -> SchedulerConfig {
    SchedulerConfig {
        lease_duration_ms: 1000,
        ..Default::default()
    }
}
fn request(id: &str, action: ControlAction) -> ControlRequest {
    ControlRequest {
        request_id: id.into(),
        action,
    }
}
fn poll(keys: Vec<LeaseKey>, available: Resources) -> PollRequest {
    PollRequest {
        worker_id: "worker".into(),
        incarnation: "epoch".into(),
        active: keys,
        available,
        max_assignments: 64,
        admission: None,
    }
}
fn free(s: &Scheduler) -> Resources {
    let worker = s
        .workers()
        .into_iter()
        .find(|worker| worker.registration.id == "worker")
        .unwrap();
    worker
        .registration
        .capacity
        .checked_sub(worker.reserved)
        .unwrap()
}
fn start(s: &mut Scheduler) -> LeaseKey {
    s.register(worker(), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let key = s
        .poll(poll(vec![], free(s)), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.poll(poll(vec![key.clone()], free(s)), 2).unwrap();
    key
}

fn fork_request() -> ExecutionForkRequest {
    ExecutionForkRequest {
        version: CLUSTER_VERSION,
        request_id: "fork-pair".into(),
        checkpoint_request_id: "save-fork".into(),
        branches: vec![
            ExecutionForkBranch {
                task_id: "left".into(),
                run_id: "run-left".into(),
            },
            ExecutionForkBranch {
                task_id: "right".into(),
                run_id: "run-right".into(),
            },
        ],
    }
}

fn fork_source(s: &mut Scheduler, seal: bool) -> LeaseKey {
    fork_source_input(
        s,
        seal,
        serde_json::json!({"prompt": "preserve source input"}),
    )
}

fn fork_source_input(s: &mut Scheduler, seal: bool, input: serde_json::Value) -> LeaseKey {
    let key = running_fork_source(s, input);
    s.request_control("one", request("save-fork", ControlAction::Checkpoint), 3)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(s)), 4)
        .unwrap()
        .controls
        .remove(0);
    if seal {
        s.acknowledge_control(
            ControlAcknowledgement {
                command,
                outcome: ControlOutcome::Checkpointed {
                    checkpoint: pvisor_core::operation::ExecutionCheckpoint {
                        snapshot_id: "d".repeat(64),
                        store: "/worker/fork-snapshots".into(),
                        source_run_id: "one".into(),
                        source_attempt_id: "fork-source-attempt".into(),
                        created_at_unix_ms: 4,
                        ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
                    },
                },
            },
            5,
        )
        .unwrap();
    }
    key
}

fn running_fork_source(s: &mut Scheduler, input: serde_json::Value) -> LeaseKey {
    running_fork_source_qos(s, input, None)
}

fn running_fork_source_qos(
    s: &mut Scheduler,
    input: serde_json::Value,
    qos: Option<pvisor_core::CpuQosClass>,
) -> LeaseKey {
    let mut registration = worker();
    if let Some(class) = qos {
        registration.cpu_qos_classes.push(class);
    }
    registration.execution_restore_protocol = Some(CLUSTER_VERSION);
    registration
        .vm_control_actions
        .extend([ControlAction::Checkpoint, ControlAction::Suspend]);
    s.register(registration, 0).unwrap();
    let mut source = spec("one");
    source.cpu_qos = qos;
    source.run.input = input;
    source
        .run
        .metadata
        .insert("user-field".into(), serde_json::json!([1, 2, 3]));
    let RunInvocation::Process(process) = &mut source.run.invocation;
    process.env.insert("TOOL_MODE".into(), "source-mode".into());
    s.submit(source, 0).unwrap();
    let key = s
        .poll(poll(vec![], free(s)), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.poll(poll(vec![key.clone()], free(s)), 2).unwrap();
    key
}

fn capture_ack(s: &mut Scheduler, key: &LeaseKey) -> ControlAcknowledgement {
    let command = s
        .poll(poll(vec![key.clone()], free(s)), 4)
        .unwrap()
        .controls
        .remove(0);
    ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Checkpointed {
            checkpoint: pvisor_core::operation::ExecutionCheckpoint {
                snapshot_id: "e".repeat(64),
                store: "/worker/live-snapshots".into(),
                source_run_id: "one".into(),
                source_attempt_id: "live-source-attempt".into(),
                created_at_unix_ms: 4,
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
            },
        },
    }
}

#[test]
fn live_capture_waits_without_admission_and_ack_releases_branches_atomically_after_replay() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    let source_key = running_fork_source(&mut s, serde_json::json!({"task":"preserved"}));
    let request = fork_request();
    let before = std::fs::metadata(&journal).unwrap().len();
    let pending = s.request_live_fork("one", request.clone(), 3).unwrap();
    assert_eq!(pending.phase, LiveForkPhase::Capturing);
    assert_eq!(pending.source_key, source_key);
    let committed = std::fs::metadata(&journal).unwrap().len();
    assert_eq!(
        std::fs::read(&journal).unwrap()[before as usize..]
            .iter()
            .filter(|b| **b == b'\n')
            .count(),
        1
    );
    assert_eq!(
        s.request_live_fork("one", request.clone(), 3).unwrap(),
        pending
    );
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), committed);
    for branch in &request.branches {
        let task = s.task(&branch.task_id).unwrap();
        assert_eq!(task.phase, TaskPhase::WaitingCheckpoint);
        assert!(task.lease.is_none());
        assert_eq!(task.generation, 0);
        assert_eq!(task.current_reservation(), Resources::default());
    }
    assert_eq!(s.workers()[0].reserved, full());
    let mut unsealed = s.task("one").unwrap().spec;
    unsealed.id = "unsealed-child".into();
    unsealed.run.run_id = "unsealed-child".into();
    unsealed.run.parent_run_id = Some("one".into());
    unsealed.restore = Some(ExecutionRestore {
        task_id: "one".into(),
        request_id: "save-fork".into(),
    });
    assert!(s.submit(unsealed, 3).is_err());
    assert!(s.task("unsealed-child").is_err());
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(s.live_fork("one", &request.request_id).unwrap(), pending);
    let ack = capture_ack(&mut s, &source_key);
    for mismatch in ["lease", "request", "run"] {
        let mut forged = ack.clone();
        match mismatch {
            "lease" => forged.command.key.generation += 1,
            "request" => forged.command.request.request_id = "unrelated-capture".into(),
            "run" => {
                let ControlOutcome::Checkpointed { checkpoint } = &mut forged.outcome else {
                    unreachable!()
                };
                checkpoint.source_run_id = "another-run".into();
            }
            _ => unreachable!(),
        }
        assert!(s.acknowledge_control(forged, 4).is_err(), "{mismatch}");
        assert_eq!(s.live_fork("one", &request.request_id).unwrap(), pending);
        assert_eq!(s.task("right").unwrap().phase, TaskPhase::WaitingCheckpoint);
    }
    assert!(
        s.poll(poll(vec![source_key.clone()], free(&s)), 4)
            .unwrap()
            .assignments
            .is_empty()
    );
    // A cancelled waiting branch must never be revived by successful capture.
    s.cancel("left", 4).unwrap();
    let before_ack = std::fs::metadata(&journal).unwrap().len();
    s.acknowledge_control(ack.clone(), 5).unwrap();
    let ready = s.live_fork("one", &request.request_id).unwrap();
    assert_eq!(ready.phase, LiveForkPhase::Ready);
    assert_eq!(
        ready.fork.as_ref().unwrap(),
        &s.execution_fork("one", &request.request_id).unwrap()
    );
    assert_eq!(s.task("left").unwrap().phase, TaskPhase::Cancelled);
    assert_eq!(s.task("right").unwrap().phase, TaskPhase::Queued);
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
    assert_eq!(s.workers()[0].reserved, full());
    assert!(
        s.poll(poll(vec![source_key.clone()], free(&s)), 6)
            .unwrap()
            .assignments
            .is_empty()
    );
    let complete_wal = std::fs::read(&journal).unwrap();
    drop(s);
    // A torn acknowledgement exposes the durable pending plan, never a queue
    // entry or receipt based on an incompletely committed snapshot observation.
    std::fs::write(&journal, &complete_wal[..before_ack as usize + 9]).unwrap();
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(s.live_fork("one", &request.request_id).unwrap(), pending);
    assert_eq!(s.task("right").unwrap().phase, TaskPhase::WaitingCheckpoint);
    s.acknowledge_control(ack, 7).unwrap();
    let ready = s.live_fork("one", &request.request_id).unwrap();
    s.cancel("one", 8).unwrap();
    assert_eq!(s.task("right").unwrap().phase, TaskPhase::Queued);
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(
        s.request_live_fork("one", request.clone(), 9).unwrap(),
        ready
    );
    assert_eq!(
        s.fork_execution("one", request, 9).unwrap(),
        ready.fork.unwrap()
    );
    assert_eq!(s.task("left").unwrap().phase, TaskPhase::Cancelled);
}

#[test]
fn live_capture_failure_cancellation_expiry_and_early_completion_never_admit_branches() {
    for failure in ["capture", "cancel", "expiry", "complete"] {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let mut s = Scheduler::open(&journal, config()).unwrap();
        let key = running_fork_source(&mut s, serde_json::Value::Null);
        let request = fork_request();
        s.request_live_fork("one", request.clone(), 3).unwrap();
        let ack = capture_ack(&mut s, &key);
        match failure {
            "capture" => {
                s.acknowledge_control(
                    ControlAcknowledgement {
                        command: ack.command.clone(),
                        outcome: ControlOutcome::Failed {
                            error: "no storage space".into(),
                        },
                    },
                    5,
                )
                .unwrap();
                assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
                assert_eq!(s.workers()[0].reserved, full());
            }
            "cancel" => {
                s.cancel("one", 5).unwrap();
                assert_eq!(s.workers()[0].reserved, full());
            }
            "expiry" => {
                s.reap(1005).unwrap();
            }
            "complete" => {
                s.complete(
                    Completion {
                        key,
                        result: None,
                        error: Some("source stopped".into()),
                        artifacts: None,
                        artifact_error: None,
                    },
                    5,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let failed = s.live_fork("one", &request.request_id).unwrap();
        assert_eq!(failed.phase, LiveForkPhase::Failed, "{failure}");
        assert!(failed.error.is_some());
        assert!(failed.fork.is_none());
        assert!(s.acknowledge_control(ack, 1006).is_err());
        for branch in &request.branches {
            let task = s.task(&branch.task_id).unwrap();
            assert_eq!(task.phase, TaskPhase::Failed, "{failure}");
            assert_eq!(task.generation, 0);
            assert!(task.lease.is_none());
        }
        drop(s);
        let mut s = Scheduler::open(&journal, config()).unwrap();
        assert_eq!(
            s.request_live_fork("one", request.clone(), 1007).unwrap(),
            failed
        );
        let mut conflicting = request.clone();
        conflicting.branches[0].task_id = "new-child".into();
        assert!(
            s.request_live_fork("one", conflicting.clone(), 1008)
                .is_err()
        );
        assert!(s.fork_execution("one", conflicting, 1008).is_err());
        assert!(s.task("new-child").is_err());
    }
}

#[test]
fn live_capture_preflight_and_partial_request_do_not_publish_control_or_any_child() {
    for invalid in [
        "version",
        "duplicate",
        "run",
        "checkpoint",
        "capability",
        "retention",
        "bytes",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let mut settings = config();
        if invalid == "retention" {
            settings.max_tasks = 2;
        }
        let mut s = Scheduler::open(&journal, settings).unwrap();
        let input = if invalid == "bytes" {
            serde_json::json!("x".repeat(2 * 1024 * 1024))
        } else {
            serde_json::Value::Null
        };
        running_fork_source(&mut s, input);
        let mut request = fork_request();
        match invalid {
            "version" => request.version += 1,
            "duplicate" => request.branches[1] = request.branches[0].clone(),
            "run" => request.branches[0].run_id = "one".into(),
            "checkpoint" => {
                s.request_control(
                    "one",
                    ControlRequest {
                        request_id: "save-fork".into(),
                        action: ControlAction::Checkpoint,
                    },
                    3,
                )
                .unwrap();
            }
            "capability" => {
                let mut w = s.workers()[0].registration.clone();
                w.execution_restore_protocol = None;
                s.register(w, 3).unwrap();
            }
            _ => {}
        }
        let before = std::fs::metadata(&journal).unwrap().len();
        assert!(s.request_live_fork("one", request, 3).is_err(), "{invalid}");
        assert_eq!(
            std::fs::metadata(&journal).unwrap().len(),
            before,
            "{invalid}"
        );
        assert!(s.task("left").is_err());
        assert!(s.task("right").is_err());
        assert!(s.live_fork("one", "fork-pair").is_err());
    }
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    running_fork_source(&mut s, serde_json::Value::Null);
    let before = std::fs::metadata(&journal).unwrap().len();
    s.request_live_fork("one", fork_request(), 3).unwrap();
    let wal = std::fs::read(&journal).unwrap();
    drop(s);
    std::fs::write(&journal, &wal[..wal.len() - 5]).unwrap();
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), before);
    assert!(s.task("left").is_err());
    assert!(s.task("right").is_err());
    assert!(s.task("one").unwrap().controls.is_empty());
    let key = s.task("one").unwrap().lease.unwrap().key;
    assert!(s.task("one").unwrap().reconciliation_pending);
    s.poll(poll(vec![key], Resources::default()), 4).unwrap();
    s.request_live_fork("one", fork_request(), 4).unwrap();
}

#[test]
fn fork_commits_every_branch_once_preserves_source_and_respects_full_admission() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    let source_key = fork_source(&mut s, true);
    let parent = s.task("one").unwrap().spec;
    let request = fork_request();
    let before = std::fs::metadata(&journal).unwrap().len();
    let record = s.fork_execution("one", request.clone(), 6).unwrap();
    assert_eq!(record.source_key, source_key);
    let committed = std::fs::metadata(&journal).unwrap().len();
    assert!(committed > before);
    let wal = std::fs::read(&journal).unwrap();
    let frame = &wal[before as usize..];
    assert_eq!(frame.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert!(frame.len() <= MAX_EXECUTION_FORK_BYTES + 66);
    assert_eq!(s.fork_execution("one", request.clone(), 7).unwrap(), record);
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), committed);
    for branch in &request.branches {
        let task = s.task(&branch.task_id).unwrap();
        assert_eq!(task.phase, TaskPhase::Queued);
        assert_eq!(task.current_reservation(), Resources::default());
        let mut expected = parent.clone();
        expected.id = branch.task_id.clone();
        expected.run.run_id = branch.run_id.clone();
        expected.run.parent_run_id = Some(parent.run.run_id.clone());
        expected.restore = Some(ExecutionRestore {
            task_id: "one".into(),
            request_id: "save-fork".into(),
        });
        assert_eq!(
            serde_json::to_value(task.spec).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
    assert_eq!(s.workers()[0].reserved, full());
    assert!(
        s.poll(poll(vec![source_key.clone()], free(&s)), 8)
            .unwrap()
            .assignments
            .is_empty()
    );
    s.request_control(
        "one",
        crate::request("park-for-branches", ControlAction::Pause),
        9,
    )
    .unwrap();
    let pause = s
        .poll(poll(vec![source_key.clone()], free(&s)), 10)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(pause), 11).unwrap();
    let assigned = s
        .poll(poll(vec![source_key], free(&s)), 12)
        .unwrap()
        .assignments;
    assert_eq!(
        assigned.len(),
        1,
        "one free slot and full CPU permits only one branch"
    );
    assert_eq!(assigned[0].checkpoint.as_ref(), Some(&record.checkpoint));
    assert_eq!(assigned[0].spec.id, "left");
    assert_eq!(s.task("left").unwrap().current_reservation(), full());
    assert_eq!(s.task("right").unwrap().phase, TaskPhase::Queued);
    s.cancel("right", 13).unwrap();
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(
        s.execution_fork("one", &request.request_id).unwrap(),
        record
    );
    assert_eq!(
        s.fork_execution("one", request.clone(), 14).unwrap(),
        record
    );
    assert_eq!(s.task("right").unwrap().phase, TaskPhase::Cancelled);
    assert_eq!(s.task("left").unwrap().generation, 1);
    let mut conflict = request;
    conflict.branches[0].run_id = "changed".into();
    assert!(s.fork_execution("one", conflict, 15).is_err());
}

#[test]
fn fork_validation_never_leaves_partial_children_or_a_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    fork_source(&mut s, true);
    s.submit(spec("occupied"), 6).unwrap();
    for invalid in [
        "version",
        "checkpoint",
        "task",
        "run",
        "duplicate-task",
        "duplicate-run",
        "empty",
        "too-many",
        "request",
        "source-run",
    ] {
        let mut request = fork_request();
        match invalid {
            "version" => request.version += 1,
            "checkpoint" => request.checkpoint_request_id = "unobserved".into(),
            "task" => request.branches[1].task_id = "occupied".into(),
            "run" => request.branches[1].run_id = "occupied".into(),
            "duplicate-task" => request.branches[1].task_id = request.branches[0].task_id.clone(),
            "duplicate-run" => request.branches[1].run_id = request.branches[0].run_id.clone(),
            "empty" => request.branches.clear(),
            "too-many" => {
                request.branches = (0..65)
                    .map(|n| ExecutionForkBranch {
                        task_id: format!("child-{n}"),
                        run_id: format!("child-run-{n}").into(),
                    })
                    .collect()
            }
            "request" => request.request_id = "../outside".into(),
            "source-run" => request.branches[1].run_id = "one".into(),
            _ => unreachable!(),
        }
        let before = std::fs::metadata(&journal).unwrap().len();
        assert!(s.fork_execution("one", request, 7).is_err(), "{invalid}");
        assert_eq!(
            std::fs::metadata(&journal).unwrap().len(),
            before,
            "{invalid}"
        );
        assert!(s.task("left").is_err());
        assert!(s.task("right").is_err());
        assert!(s.execution_fork("one", "fork-pair").is_err());
    }
    // No hidden policy/invocation/host-path override is accepted on the wire.
    let mut wire = serde_json::to_value(fork_request()).unwrap();
    wire["branches"][0]["command"] = serde_json::json!("/bin/false");
    assert!(serde_json::from_value::<ExecutionForkRequest>(wire).is_err());
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    let mut request = fork_request();
    request.branches[1].run_id = "occupied".into();
    assert!(
        s.fork_execution("one", request, 8).is_err(),
        "Run identity index must survive replay"
    );
}

#[test]
fn fork_requires_observed_capture_and_native_suspend_termination() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    fork_source(&mut s, false);
    assert!(s.fork_execution("one", fork_request(), 5).is_err());
    assert!(s.task("left").is_err());
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut s);
    let mut registration = worker();
    registration.vm_control_actions.push(ControlAction::Suspend);
    s.register(registration, 3).unwrap();
    s.request_control("one", request("suspend", ControlAction::Suspend), 4)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 5)
        .unwrap()
        .controls
        .remove(0);
    let receipt = pvisor_core::operation::ExecutionSuspension::from_result(
        suspended_completion(key.clone()).result.as_ref().unwrap(),
    )
    .unwrap();
    s.acknowledge_control(
        ControlAcknowledgement {
            command,
            outcome: ControlOutcome::Checkpointed {
                checkpoint: receipt.checkpoint.clone(),
            },
        },
        6,
    )
    .unwrap();
    let mut fork = fork_request();
    fork.checkpoint_request_id = "suspend".into();
    assert!(s.fork_execution("one", fork.clone(), 7).is_err());
    assert!(s.task("left").is_err());
    s.complete(suspended_completion(key), 8).unwrap();
    assert_eq!(
        s.fork_execution("one", fork, 9).unwrap().checkpoint,
        receipt.checkpoint
    );
}

#[test]
fn fork_retention_limit_and_truncated_commit_leave_every_branch_uncreated() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let bounded = SchedulerConfig {
        max_tasks: 2,
        ..config()
    };
    let mut s = Scheduler::open(&journal, bounded.clone()).unwrap();
    fork_source(&mut s, true);
    assert!(s.fork_execution("one", fork_request(), 6).is_err());
    assert!(s.task("left").is_err());
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    let before = std::fs::metadata(&journal).unwrap().len();
    s.fork_execution("one", fork_request(), 7).unwrap();
    let after = std::fs::metadata(&journal).unwrap().len();
    drop(s);
    // A crash during the one frame cannot expose the first branch alone.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(before + (after - before) / 2)
        .unwrap();
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), before);
    assert!(s.task("left").is_err());
    assert!(s.task("right").is_err());
    assert!(s.execution_fork("one", "fork-pair").is_err());
    s.fork_execution("one", fork_request(), 8).unwrap();
    assert!(s.task("left").is_ok() && s.task("right").is_ok());
}

#[test]
fn fork_bounds_branch_count_and_serialized_work_without_bypassing_tenant_quota() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut bounded = config();
    let quota = full().checked_add(full()).unwrap();
    bounded.tenant_quotas.insert("tenant".into(), quota);
    let mut s = Scheduler::open(&journal, bounded).unwrap();
    let source = fork_source(&mut s, true);
    let mut registration = s.workers()[0].registration.clone();
    registration.capacity = quota.checked_add(full()).unwrap();
    s.register(registration, 6).unwrap();
    let mut request = fork_request();
    request.branches = (0..MAX_EXECUTION_FORK_BRANCHES)
        .map(|n| ExecutionForkBranch {
            task_id: format!("branch-{n}"),
            run_id: format!("branch-run-{n}").into(),
        })
        .collect();
    s.fork_execution("one", request.clone(), 7).unwrap();
    let assigned = s
        .poll(poll(vec![source.clone()], free(&s)), 8)
        .unwrap()
        .assignments;
    assert_eq!(
        assigned.len(),
        1,
        "tenant quota applies to each branch's full charge"
    );
    assert_eq!(s.workers()[0].reserved, quota);
    assert_eq!(
        s.task(&request.branches[1].task_id).unwrap().phase,
        TaskPhase::Queued
    );
    assert!(
        s.poll(
            poll(vec![source, assigned[0].lease.key.clone()], free(&s)),
            9
        )
        .unwrap()
        .assignments
        .is_empty()
    );

    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    fork_source_input(
        &mut s,
        true,
        serde_json::json!("x".repeat(MAX_EXECUTION_FORK_BYTES / 2)),
    );
    let before = std::fs::metadata(&journal).unwrap().len();
    let error = s.fork_execution("one", fork_request(), 6).unwrap_err();
    assert!(error.to_string().contains("4 MiB"), "{error}");
    assert_eq!(std::fs::metadata(&journal).unwrap().len(), before);
    assert!(s.task("left").is_err() && s.task("right").is_err());
}

fn memory_report(key: &LeaseKey, sequence: u64) -> MemoryReportRequest {
    use pvisor_core::memory::{NativeVmMemory, ResidentMemory, RunMemorySample};
    let ram = ResidentMemory {
        mapped_bytes: 16384,
        rss_bytes: 8192,
        pss_bytes: 4096,
        shared_clean_bytes: 8192,
        ..Default::default()
    };
    let non_ram = ResidentMemory {
        mapped_bytes: 4096,
        rss_bytes: 4096,
        pss_bytes: 4096,
        private_dirty_bytes: 4096,
        ..Default::default()
    };
    MemoryReportRequest {
        worker_id: key.worker_id.clone(),
        incarnation: key.incarnation.clone(),
        samples: vec![AttemptMemorySample {
            key: key.clone(),
            sequence,
            sample_age_ms: 1,
            sample: RunMemorySample {
                run_id: "one".into(),
                attempt_id: "synthetic-native-one".into(),
                sampled_at_unix_ms: sequence + 1,
                usage: Some(NativeVmMemory {
                    pid: 123,
                    start_time_ticks: 99,
                    process: ram.checked_add(&non_ram).unwrap(),
                    guest_ram: ram,
                    non_ram,
                }),
                error: None,
            },
        }],
    }
}

#[test]
fn physical_observations_do_not_write_wal_renew_leases_or_release_capacity_and_restart_clears_them()
{
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let key = start(&mut scheduler);
    let before = scheduler.task("one").unwrap();
    let wal = std::fs::read(&path).unwrap();
    let request = memory_report(&key, 1);
    let receipt = scheduler.report_memory(request.clone(), 10).unwrap();
    assert_eq!(receipt.accepted, vec![key.clone()]);
    assert!(receipt.ignored.is_empty());
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .memory_sample
            .as_ref()
            .unwrap()
            .received_at_ms,
        10
    );
    scheduler.report_memory(request.clone(), 20).unwrap();
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .memory_sample
            .as_ref()
            .unwrap()
            .received_at_ms,
        10,
        "replay must not refresh a stale sample"
    );
    let newer = memory_report(&key, 2);
    scheduler.report_memory(newer.clone(), 30).unwrap();
    let old = scheduler.report_memory(request, 40).unwrap();
    assert_eq!(old.ignored, vec![key.clone()]);
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .memory_sample
            .as_ref()
            .unwrap()
            .report,
        newer.samples[0]
    );
    let after = scheduler.task("one").unwrap();
    assert_eq!(after.phase, before.phase);
    assert_eq!(after.updated_at_ms, before.updated_at_ms);
    assert_eq!(
        after.lease.as_ref().unwrap().expires_at_ms,
        before.lease.unwrap().expires_at_ms
    );
    assert_eq!(after.current_reservation(), full());
    assert_eq!(scheduler.workers()[0].reserved, full());
    assert_eq!(std::fs::read(&path).unwrap(), wal);
    drop(scheduler);
    let mut reopened = Scheduler::open(&path, config()).unwrap();
    assert!(reopened.task("one").unwrap().memory_sample.is_none());
    assert_eq!(reopened.task("one").unwrap().current_reservation(), full());
    reopened.report_memory(newer, 50).unwrap();
    let failed = Completion {
        key: key.clone(),
        result: None,
        error: Some("synthetic termination".into()),
        artifacts: None,
        artifact_error: None,
    };
    reopened.complete(failed, 60).unwrap();
    assert!(reopened.task("one").unwrap().memory_sample.is_none());
    assert_eq!(
        reopened
            .report_memory(memory_report(&key, 3), 70)
            .unwrap()
            .ignored,
        vec![key]
    );
    assert!(reopened.task("one").unwrap().memory_sample.is_none());
}

#[test]
fn memory_report_failures_keep_binding_and_malformed_or_conflicting_batches_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut scheduler);
    scheduler.report_memory(memory_report(&key, 1), 10).unwrap();
    let mut failed = memory_report(&key, 2);
    failed.samples[0].sample.usage = None;
    failed.samples[0].sample.error = Some("process memory is temporarily unavailable".into());
    scheduler.report_memory(failed.clone(), 20).unwrap();
    for case in 0..7 {
        let mut forged = memory_report(&key, 3);
        match case {
            0 => forged.samples[0].sample.usage.as_mut().unwrap().pid += 1,
            1 => {
                forged.samples[0]
                    .sample
                    .usage
                    .as_mut()
                    .unwrap()
                    .start_time_ticks += 1
            }
            2 => forged.samples[0].sample.attempt_id = "foreign-attempt".into(),
            3 => forged.samples[0].sample.run_id = "foreign-run".into(),
            4 => forged.samples[0].sequence = 0,
            5 => forged.incarnation = "foreign-epoch".into(),
            _ => forged.samples[0].key.worker_id = "foreign-worker".into(),
        }
        assert!(scheduler.report_memory(forged, 30).is_err(), "case {case}");
    }
    let mut conflict = failed.clone();
    conflict.samples[0].sample.error = Some("different bytes for same sequence".into());
    assert!(scheduler.report_memory(conflict, 30).is_err());
    let mut batch = memory_report(&key, 3);
    let mut bad = batch.samples[0].clone();
    bad.key.task_id = "unknown".into();
    bad.sample.usage = None; // Unknown leases still require a valid wire contract.
    batch.samples.push(bad);
    assert!(scheduler.report_memory(batch, 30).is_err());
    assert_eq!(
        scheduler.task("one").unwrap().memory_sample.unwrap().report,
        failed.samples[0]
    );
    let mut duplicates = memory_report(&key, 3);
    duplicates.samples.push(duplicates.samples[0].clone());
    assert!(scheduler.report_memory(duplicates, 30).is_err());
    let mut oversized = memory_report(&key, 3);
    oversized.samples = vec![oversized.samples[0].clone(); 65];
    assert!(scheduler.report_memory(oversized, 30).is_err());
    assert_eq!(scheduler.task("one").unwrap().current_reservation(), full());
}

#[test]
fn memory_observation_cannot_revive_expired_or_unknown_execution() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut scheduler);
    let expires = scheduler.task("one").unwrap().lease.unwrap().expires_at_ms;
    let mut request = memory_report(&key, 1);
    let mut unknown = request.samples[0].clone();
    unknown.key.task_id = "unknown".into();
    request.samples.push(unknown.clone());
    let response = scheduler.report_memory(request, expires).unwrap();
    assert!(response.accepted.is_empty());
    assert_eq!(response.ignored, vec![key.clone(), unknown.key]);
    assert_eq!(
        scheduler.task("one").unwrap().lease.unwrap().expires_at_ms,
        expires
    );
    assert!(scheduler.task("one").unwrap().memory_sample.is_none());
    scheduler.reap(expires).unwrap();
    assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Lost);
    assert_eq!(
        scheduler
            .report_memory(memory_report(&key, 2), expires + 1)
            .unwrap()
            .ignored,
        vec![key]
    );
    assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Lost);
}
fn success(command: ControlCommand) -> ControlAcknowledgement {
    let (state, memory) = match command.request.action {
        ControlAction::Checkpoint | ControlAction::Suspend => {
            panic!("this helper only acknowledges VM state transitions")
        }
        ControlAction::Pause => (VmState::Paused, None),
        ControlAction::Resume => (VmState::Running, None),
        ControlAction::Offload => (
            VmState::Offloaded,
            Some(VmMemory {
                backing_file: "/private/ram".into(),
                backed_bytes: full().memory_bytes,
                resident_before_bytes: Some(full().memory_bytes),
                resident_after_bytes: Some(0),
            }),
        ),
    };
    ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Succeeded { state, memory },
    }
}

fn suspended_completion(key: LeaseKey) -> Completion {
    use pvisor_core::operation::{ExecutionCheckpoint, ExecutionSuspension, SnapshotRamStorage};
    let receipt = ExecutionSuspension {
        request_id: "suspend".into(),
        checkpoint: ExecutionCheckpoint {
            snapshot_id: "c".repeat(64),
            store: "/worker/one/execution-snapshots".into(),
            source_run_id: "one".into(),
            source_attempt_id: "native-attempt".into(),
            created_at_unix_ms: 5,
            ram_storage: SnapshotRamStorage::Compressed,
        },
    };
    Completion {
        key,
        result: Some(pvisor_core::RunResult {
            run_id: "one".into(),
            attempt_id: "native-attempt".into(),
            state: pvisor_core::RunState::Hibernated,
            started_at_unix_ms: 2,
            finished_at_unix_ms: 6,
            exit_code: None,
            failure: None,
            output: Default::default(),
            value: Some(serde_json::to_value(receipt).unwrap()),
            metrics: Default::default(),
            artifacts: vec![],
            event_stream_ref: None,
            warnings: vec![],
            executor_observations: Default::default(),
        }),
        error: None,
        artifacts: None,
        artifact_error: None,
    }
}

#[test]
fn parked_suspension_capability_is_optional_on_old_wire_and_requires_native_vm_support() {
    let legacy = serde_json::to_value(worker()).unwrap();
    assert!(legacy.get("parked_execution_suspend_protocol").is_none());
    let decoded: WorkerRegistration = serde_json::from_value(legacy).unwrap();
    assert!(decoded.parked_execution_suspend_protocol.is_none());
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut registration = worker();
    registration.parked_execution_suspend_protocol = Some(CLUSTER_VERSION);
    assert!(scheduler.register(registration.clone(), 1).is_err());
    registration.vm_control_actions.push(ControlAction::Suspend);
    registration.parked_execution_suspend_protocol = Some(CLUSTER_VERSION + 1);
    assert!(scheduler.register(registration.clone(), 1).is_err());
    registration.parked_execution_suspend_protocol = Some(CLUSTER_VERSION);
    registration.vm_control_protocol = None;
    assert!(scheduler.register(registration.clone(), 1).is_err());
    registration.vm_control_protocol = Some(CLUSTER_VERSION);
    registration.execution[0].executor = ExecutorKind::Process;
    registration.execution[0].isolation = IsolationKind::HostProcess;
    assert!(scheduler.register(registration.clone(), 1).is_err());
    assert!(scheduler.workers().is_empty());
    registration.execution = vec![execution()];
    scheduler.register(registration, 1).unwrap();
}

#[test]
fn paused_and_offloaded_suspension_never_recharges_cpu_and_waits_for_native_exit() {
    for action in [ControlAction::Pause, ControlAction::Offload] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        let mut scheduler = Scheduler::open(&path, config()).unwrap();
        let source = start(&mut scheduler);
        let mut registration = worker();
        registration
            .vm_control_actions
            .extend([ControlAction::Checkpoint, ControlAction::Suspend]);
        registration.execution_restore_protocol = Some(CLUSTER_VERSION);
        scheduler.register(registration.clone(), 3).unwrap();
        scheduler
            .request_control("one", request("park", action), 4)
            .unwrap();
        let parked = scheduler
            .poll(poll(vec![source.clone()], free(&scheduler)), 5)
            .unwrap()
            .controls
            .remove(0);
        scheduler.acknowledge_control(success(parked), 6).unwrap();
        let held = Resources {
            cpu_millis: 0,
            ..full()
        };
        assert_eq!(scheduler.workers()[0].reserved, held);
        assert!(
            scheduler
                .request_control("one", request("legacy-suspend", ControlAction::Suspend), 7)
                .is_err()
        );
        assert_eq!(scheduler.task("one").unwrap().controls.len(), 1);
        registration.parked_execution_suspend_protocol = Some(CLUSTER_VERSION);
        scheduler.register(registration, 7).unwrap();
        // Continuing checkpoints still require running CPUs. Suspend directly
        // consumes the current parked state, even with all CPU in another VM.
        assert!(
            scheduler
                .request_control("one", request("checkpoint", ControlAction::Checkpoint), 7)
                .is_err()
        );
        scheduler.submit(spec("competitor"), 8).unwrap();
        let competitor = scheduler
            .poll(poll(vec![source.clone()], free(&scheduler)), 9)
            .unwrap()
            .assignments
            .remove(0)
            .lease
            .key;
        let keys = vec![source.clone(), competitor.clone()];
        scheduler
            .request_control("one", request("suspend", ControlAction::Suspend), 10)
            .unwrap();
        let command = scheduler
            .poll(poll(keys.clone(), free(&scheduler)), 11)
            .unwrap()
            .controls
            .remove(0);
        assert_eq!(command.request.action, ControlAction::Suspend);
        assert_eq!(scheduler.task("one").unwrap().current_reservation(), held);
        let expected = held.checked_add(full()).unwrap();
        assert_eq!(scheduler.workers()[0].reserved, expected);
        let completion = suspended_completion(source.clone());
        let receipt = pvisor_core::operation::ExecutionSuspension::from_result(
            completion.result.as_ref().unwrap(),
        )
        .unwrap();
        let acknowledgement = ControlAcknowledgement {
            command,
            outcome: ControlOutcome::Checkpointed {
                checkpoint: receipt.checkpoint,
            },
        };
        scheduler
            .acknowledge_control(acknowledgement.clone(), 12)
            .unwrap();
        assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Suspending);
        assert_eq!(scheduler.workers()[0].reserved, expected);
        drop(scheduler);
        let mut scheduler = Scheduler::open(&path, config()).unwrap();
        assert_eq!(scheduler.workers()[0].reserved, expected);
        scheduler.acknowledge_control(acknowledgement, 13).unwrap();
        scheduler.complete(completion, 14).unwrap();
        assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Suspended);
        assert_eq!(scheduler.workers()[0].reserved, full());
        assert_eq!(
            scheduler.task("competitor").unwrap().current_reservation(),
            full()
        );
        scheduler
            .poll(poll(vec![competitor], free(&scheduler)), 15)
            .unwrap();
        assert_eq!(
            scheduler.task("competitor").unwrap().phase,
            TaskPhase::Running
        );
    }
}

#[test]
fn suspension_holds_all_capacity_until_native_completion_and_allows_new_attempt_restore() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let source = start(&mut scheduler);
    let mut registration = worker();
    registration
        .vm_control_actions
        .extend([ControlAction::Checkpoint, ControlAction::Suspend]);
    registration.execution_restore_protocol = Some(CLUSTER_VERSION);
    scheduler.register(registration, 3).unwrap();
    scheduler
        .request_control("one", request("suspend", ControlAction::Suspend), 4)
        .unwrap();
    let command = scheduler
        .poll(poll(vec![source.clone()], free(&scheduler)), 5)
        .unwrap()
        .controls
        .remove(0);
    let completion = suspended_completion(source.clone());
    let receipt = pvisor_core::operation::ExecutionSuspension::from_result(
        completion.result.as_ref().unwrap(),
    )
    .unwrap();
    let acknowledgement = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Checkpointed {
            checkpoint: receipt.checkpoint.clone(),
        },
    };
    scheduler
        .acknowledge_control(acknowledgement.clone(), 6)
        .unwrap();
    assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Suspending);
    assert_eq!(scheduler.workers()[0].reserved, full());
    scheduler.submit(spec("competing"), 6).unwrap();
    assert!(
        scheduler
            .poll(poll(vec![source.clone()], free(&scheduler)), 7)
            .unwrap()
            .assignments
            .is_empty()
    );
    let mut restored = spec("restored");
    restored.run.parent_run_id = Some("one".into());
    restored.restore = Some(ExecutionRestore {
        task_id: "one".into(),
        request_id: "suspend".into(),
    });
    assert!(
        scheduler.submit(restored.clone(), 7).is_err(),
        "sealed state alone cannot prove termination"
    );
    assert!(
        scheduler
            .request_control("one", request("premature-resume", ControlAction::Resume), 7)
            .is_err()
    );
    let stopped = scheduler.complete(completion.clone(), 8).unwrap();
    assert_eq!(stopped.phase, TaskPhase::Suspended);
    assert!(stopped.phase.terminal());
    assert_eq!(scheduler.workers()[0].reserved, Resources::default());
    assert_eq!(
        scheduler.complete(completion, 9).unwrap().phase,
        TaskPhase::Suspended
    );
    assert_eq!(
        scheduler
            .acknowledge_control(acknowledgement, 9)
            .unwrap()
            .phase,
        ControlPhase::Succeeded
    );
    scheduler.submit(restored, 10).unwrap();
    drop(scheduler);
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    assert_eq!(scheduler.workers()[0].reserved, Resources::default());
    scheduler.cancel("competing", 11).unwrap();
    let assignment = scheduler
        .poll(poll(vec![], free(&scheduler)), 12)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.spec.id, "restored");
    assert_eq!(assignment.checkpoint, Some(receipt.checkpoint));
    assert_ne!(assignment.lease.key, source);
    assert_eq!(scheduler.workers()[0].reserved, full());
}

#[test]
fn native_suspend_completion_atomically_recovers_a_lost_control_ack_and_rejects_forgery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let key = start(&mut scheduler);
    let mut registration = worker();
    registration.vm_control_actions.push(ControlAction::Suspend);
    scheduler.register(registration, 3).unwrap();
    scheduler
        .request_control("one", request("suspend", ControlAction::Suspend), 4)
        .unwrap();
    let good = suspended_completion(key.clone());
    assert!(
        scheduler.complete(good.clone(), 4).is_err(),
        "pending command has no native authority"
    );
    scheduler
        .poll(poll(vec![key], free(&scheduler)), 5)
        .unwrap();
    for corruption in [
        "missing", "attempt", "request", "encoding", "exit", "failure",
    ] {
        let mut bad = good.clone();
        let result = bad.result.as_mut().unwrap();
        match corruption {
            "missing" => result.value = None,
            "attempt" => {
                result.value.as_mut().unwrap()["checkpoint"]["source_attempt_id"] = "foreign".into()
            }
            "request" => result.value.as_mut().unwrap()["request_id"] = "unknown".into(),
            "encoding" => {
                result.value.as_mut().unwrap()["checkpoint"]["ram_storage"] = "raw".into()
            }
            "exit" => result.exit_code = Some(0),
            "failure" => {
                result.failure = Some(pvisor_core::RunFailure {
                    kind: pvisor_core::RunFailureKind::Infrastructure,
                    message: "failed".into(),
                    retryable: false,
                })
            }
            _ => unreachable!(),
        }
        assert!(scheduler.complete(bad, 6).is_err(), "{corruption}");
        assert_eq!(scheduler.workers()[0].reserved, full());
        assert_eq!(
            scheduler.task("one").unwrap().controls[0].phase,
            ControlPhase::Issued
        );
    }
    scheduler.complete(good, 7).unwrap();
    drop(scheduler);
    let mut scheduler = Scheduler::open(&path, config()).unwrap();
    let task = scheduler.task("one").unwrap();
    assert_eq!(task.phase, TaskPhase::Suspended);
    assert_eq!(task.controls[0].phase, ControlPhase::Succeeded);
    assert!(matches!(
        task.controls[0].outcome,
        Some(ControlOutcome::Checkpointed { .. })
    ));
    assert_eq!(scheduler.workers()[0].reserved, Resources::default());
    assert_eq!(
        scheduler.reap(2000).unwrap(),
        0,
        "finished source cannot become Lost"
    );
}

#[test]
fn cancellation_and_expiry_fence_suspend_completion_without_resurrecting_control() {
    for expired in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
        let key = start(&mut scheduler);
        let mut registration = worker();
        registration.vm_control_actions.push(ControlAction::Suspend);
        scheduler.register(registration, 3).unwrap();
        scheduler
            .request_control("one", request("suspend", ControlAction::Suspend), 4)
            .unwrap();
        scheduler
            .poll(poll(vec![key.clone()], free(&scheduler)), 5)
            .unwrap();
        if expired {
            scheduler.reap(2000).unwrap();
            assert!(scheduler.complete(suspended_completion(key), 2001).is_err());
            assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Lost);
        } else {
            scheduler.cancel("one", 6).unwrap();
            assert_eq!(scheduler.workers()[0].reserved, full());
            scheduler.complete(suspended_completion(key), 7).unwrap();
            assert_eq!(scheduler.task("one").unwrap().phase, TaskPhase::Cancelled);
        }
        assert_eq!(
            scheduler.task("one").unwrap().controls[0].phase,
            ControlPhase::Aborted
        );
        assert_eq!(scheduler.workers()[0].reserved, Resources::default());
    }
}

#[test]
fn checkpoint_acknowledgement_keeps_resources_and_is_fenced_durable_and_idempotent() {
    use pvisor_core::operation::{ExecutionCheckpoint, SnapshotRamStorage};
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut s = Scheduler::open(&journal, config()).unwrap();
    let key = start(&mut s);
    assert!(
        s.request_control("one", request("unsupported", ControlAction::Checkpoint), 3)
            .is_err()
    );
    let mut registration = worker();
    registration
        .vm_control_actions
        .push(ControlAction::Checkpoint);
    s.register(registration, 3).unwrap();
    s.request_control("one", request("checkpoint", ControlAction::Checkpoint), 4)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 5)
        .unwrap()
        .controls
        .remove(0);
    assert_eq!(s.workers()[0].reserved, full());
    let acknowledgement = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Checkpointed {
            checkpoint: ExecutionCheckpoint {
                snapshot_id: "a".repeat(64),
                store: "/worker/owned-snapshots".into(),
                source_run_id: "one".into(),
                source_attempt_id: "native-attempt".into(),
                created_at_unix_ms: 5,
                ram_storage: SnapshotRamStorage::Compressed,
            },
        },
    };
    let mut invalid = acknowledgement.clone();
    let ControlOutcome::Checkpointed { checkpoint } = &mut invalid.outcome else {
        panic!()
    };
    checkpoint.source_run_id = "other".into();
    assert!(s.acknowledge_control(invalid, 6).is_err());
    let record = s.acknowledge_control(acknowledgement.clone(), 6).unwrap();
    assert_eq!(s.workers()[0].reserved, full());
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
    assert_eq!(
        record,
        s.acknowledge_control(acknowledgement.clone(), 7).unwrap()
    );
    drop(s);
    let mut s = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(s.workers()[0].reserved, full());
    assert_eq!(s.acknowledge_control(acknowledgement, 8).unwrap(), record);
    assert_eq!(
        s.request_control("one", request("checkpoint", ControlAction::Checkpoint), 8)
            .unwrap(),
        record
    );
    assert!(s.task("one").unwrap().reconciliation_pending);
    s.poll(poll(vec![key.clone()], free(&s)), 9).unwrap();
    s.request_control("one", request("paused", ControlAction::Pause), 9)
        .unwrap();
    let pause = s
        .poll(poll(vec![key.clone()], free(&s)), 10)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(pause), 11).unwrap();
    assert!(
        s.request_control("one", request("new-capture", ControlAction::Checkpoint), 12)
            .is_err()
    );
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Paused);
}

#[test]
fn execution_restore_uses_sealed_history_node_affinity_full_admission_and_new_identity() {
    use pvisor_core::operation::{ExecutionCheckpoint, SnapshotRamStorage};
    let temporary = tempfile::tempdir().unwrap();
    let journal = temporary.path().join("journal");
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let source = start(&mut scheduler);
    let mut registration = worker();
    registration
        .vm_control_actions
        .push(ControlAction::Checkpoint);
    scheduler.register(registration.clone(), 3).unwrap();
    scheduler
        .request_control("one", request("save", ControlAction::Checkpoint), 4)
        .unwrap();
    let mut restored = spec("restored");
    restored.run.parent_run_id = Some("one".into());
    restored.restore = Some(ExecutionRestore {
        task_id: "one".into(),
        request_id: "save".into(),
    });
    assert!(
        scheduler.submit(restored.clone(), 4).is_err(),
        "pending capture cannot authorize restore"
    );
    let command = scheduler
        .poll(poll(vec![source.clone()], free(&scheduler)), 5)
        .unwrap()
        .controls
        .remove(0);
    let checkpoint = ExecutionCheckpoint {
        snapshot_id: "b".repeat(64),
        store: "/worker/source/execution-snapshots".into(),
        source_run_id: "one".into(),
        source_attempt_id: "source-attempt".into(),
        created_at_unix_ms: 5,
        ram_storage: SnapshotRamStorage::Compressed,
    };
    scheduler
        .acknowledge_control(
            ControlAcknowledgement {
                command,
                outcome: ControlOutcome::Checkpointed {
                    checkpoint: checkpoint.clone(),
                },
            },
            6,
        )
        .unwrap();
    for change in [
        "tenant",
        "identity",
        "parent",
        "command",
        "environment",
        "memory",
        "cpu_qos",
    ] {
        let mut invalid = restored.clone();
        match change {
            "tenant" => invalid.tenant = "foreign".into(),
            "identity" => invalid.run.run_id = "one".into(),
            "parent" => invalid.run.parent_run_id = None,
            "command" => {
                let RunInvocation::Process(process) = &mut invalid.run.invocation;
                process.program = "/bin/false".into();
            }
            "environment" => {
                let RunInvocation::Process(process) = &mut invalid.run.invocation;
                process.env.insert("CHANGED".into(), "value".into());
            }
            "memory" => invalid.resources.memory_bytes += 1024 * 1024,
            "cpu_qos" => invalid.cpu_qos = Some(pvisor_core::CpuQosClass::LatencySensitive),
            _ => unreachable!(),
        }
        assert!(scheduler.submit(invalid, 7).is_err(), "{change}");
    }
    scheduler.submit(restored.clone(), 7).unwrap();
    assert!(
        scheduler
            .poll(poll(vec![source.clone()], free(&scheduler)), 8)
            .unwrap()
            .assignments
            .is_empty(),
        "source CPU reservation must block a second execution"
    );
    scheduler
        .request_control("one", request("pause", ControlAction::Pause), 9)
        .unwrap();
    let pause = scheduler
        .poll(poll(vec![source.clone()], free(&scheduler)), 10)
        .unwrap()
        .controls
        .remove(0);
    scheduler.acknowledge_control(success(pause), 11).unwrap();
    assert!(
        scheduler
            .poll(poll(vec![source.clone()], free(&scheduler)), 12)
            .unwrap()
            .assignments
            .is_empty(),
        "an older Worker cannot receive a restore assignment"
    );
    let mut other = registration.clone();
    other.id = "other".into();
    other.execution_restore_protocol = Some(CLUSTER_VERSION);
    scheduler.register(other.clone(), 13).unwrap();
    let mut other_poll = poll(vec![], other.capacity);
    other_poll.worker_id = other.id;
    assert!(
        scheduler
            .poll(other_poll, 14)
            .unwrap()
            .assignments
            .is_empty(),
        "node-local snapshot must stay on its owning node"
    );
    registration.execution_restore_protocol = Some(CLUSTER_VERSION);
    scheduler.register(registration, 15).unwrap();
    let assignment = scheduler
        .poll(poll(vec![source.clone()], free(&scheduler)), 16)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.checkpoint, Some(checkpoint.clone()));
    assert_eq!(
        scheduler.task("restored").unwrap().current_reservation(),
        full()
    );
    let restore_key = assignment.lease.key;
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let replay = scheduler
        .poll(poll(vec![source.clone()], free(&scheduler)), 17)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(replay.checkpoint, Some(checkpoint));
    assert_eq!(replay.lease.key, restore_key);
    scheduler
        .poll(poll(vec![source, restore_key], free(&scheduler)), 18)
        .unwrap();
    assert!(
        scheduler
            .request_control("restored", request("offload", ControlAction::Offload), 19)
            .is_err()
    );
    assert!(
        scheduler
            .request_control(
                "restored",
                request("pause-restored", ControlAction::Pause),
                19
            )
            .is_ok()
    );
    assert_eq!(
        scheduler.submit(restored, 20).unwrap().phase,
        TaskPhase::Running
    );
}

#[test]
fn only_acknowledged_pause_releases_cpu_and_restart_preserves_that_charge() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let key = start(&mut s);
    let requested = s
        .request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    assert_eq!(requested.phase, ControlPhase::Pending);
    assert_eq!(s.workers()[0].reserved, full());
    assert_eq!(
        s.request_control("one", request("pause", ControlAction::Pause), 4)
            .unwrap(),
        requested
    );
    assert!(
        s.request_control("one", request("pause", ControlAction::Resume), 4)
            .is_err()
    );
    assert!(
        s.request_control("one", request("other", ControlAction::Offload), 4)
            .is_err()
    );
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 5)
        .unwrap()
        .controls
        .remove(0);
    assert_eq!(s.workers()[0].reserved, full());
    let ack = success(command.clone());
    let record = s.acknowledge_control(ack.clone(), 6).unwrap();
    assert_eq!(record.phase, ControlPhase::Succeeded);
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Paused);
    let paused = Resources {
        cpu_millis: 0,
        ..full()
    };
    assert_eq!(s.workers()[0].reserved, paused);
    assert_eq!(s.acknowledge_control(ack.clone(), 7).unwrap(), record);
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.workers()[0].reserved, paused);
    assert_eq!(s.task("one").unwrap().controls[0], record);
    assert!(
        s.poll(poll(vec![key.clone()], free(&s)), 8)
            .unwrap()
            .controls
            .is_empty()
    );
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Paused);
    s.complete(
        Completion {
            key,
            result: None,
            error: Some("stopped".into()),
            artifacts: None,
            artifact_error: None,
        },
        9,
    )
    .unwrap();
    assert_eq!(s.workers()[0].reserved, Resources::default());
    assert_eq!(s.acknowledge_control(ack, 10).unwrap(), record);
}

#[test]
fn resume_waits_for_cpu_and_reserves_it_before_issuing_even_if_response_is_lost() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let key = start(&mut s);
    s.request_control("one", request("offload", ControlAction::Offload), 3)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 4)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(command), 5).unwrap();
    // A zero mincore sample still retains the whole physical-memory budget.
    assert_eq!(
        s.workers()[0].reserved,
        Resources {
            cpu_millis: 0,
            ..full()
        }
    );
    s.submit(spec("two"), 6).unwrap();
    let other = s
        .poll(poll(vec![key.clone()], free(&s)), 7)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.request_control("one", request("resume", ControlAction::Resume), 8)
        .unwrap();
    assert!(
        s.poll(poll(vec![key.clone(), other.clone()], free(&s)), 9)
            .unwrap()
            .controls
            .is_empty()
    );
    assert_eq!(
        s.task("one").unwrap().controls[1].phase,
        ControlPhase::Pending
    );
    s.complete(
        Completion {
            key: other,
            result: None,
            error: Some("done".into()),
            artifacts: None,
            artifact_error: None,
        },
        10,
    )
    .unwrap();
    // Node final admission can still deny resume despite controller capacity.
    assert!(
        s.poll(poll(vec![key.clone()], Resources::default()), 11)
            .unwrap()
            .controls
            .is_empty()
    );
    let issued = s
        .poll(poll(vec![key.clone()], free(&s)), 12)
        .unwrap()
        .controls
        .remove(0);
    assert_eq!(s.workers()[0].reserved, full());
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Offloaded);
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(s.workers()[0].reserved, full());
    let repeated = s
        .poll(poll(vec![key], Resources::default()), 13)
        .unwrap()
        .controls
        .remove(0);
    assert_eq!(issued, repeated);
    assert_eq!(s.workers()[0].reserved, full());
    s.acknowledge_control(success(repeated), 14).unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
}

#[test]
fn resume_and_new_assignments_share_local_budget_including_lost_response_redelivery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    let key = start(&mut s);
    s.request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let paused = s
        .poll(poll(vec![key.clone()], free(&s)), 4)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(paused), 5).unwrap();
    let mut registration = worker();
    registration.capacity = Resources {
        slots: 3,
        memory_bytes: 3 * full().memory_bytes,
        cpu_millis: 3 * full().cpu_millis,
    };
    s.register(registration, 6).unwrap();
    s.submit(spec("two"), 7).unwrap();
    s.submit(spec("three"), 7).unwrap();
    s.request_control("one", request("resume", ControlAction::Resume), 8)
        .unwrap();
    // Controller has ample capacity, but node pressure leaves only one CPU
    // reservation available. Resume and assignment must not both spend it.
    let available = Resources {
        slots: 2,
        memory_bytes: 2 * full().memory_bytes,
        cpu_millis: full().cpu_millis,
    };
    let issued = s.poll(poll(vec![key.clone()], available), 9).unwrap();
    assert_eq!(issued.controls.len(), 1);
    assert!(issued.assignments.is_empty());
    assert_eq!(
        s.task("one").unwrap().controls[1].admission.cpu_millis,
        full().cpu_millis
    );
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let repeated = s.poll(poll(vec![key.clone()], available), 10).unwrap();
    assert_eq!(repeated.controls, issued.controls);
    assert!(repeated.assignments.is_empty());
    s.acknowledge_control(success(repeated.controls[0].clone()), 11)
        .unwrap();
    let response = s.poll(poll(vec![key], available), 12).unwrap();
    assert!(response.controls.is_empty());
    assert_eq!(response.assignments.len(), 1);
}

#[test]
fn paused_cpu_can_be_reused_but_resume_must_reenter_tenant_quota_even_while_draining() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    config.tenant_quotas.insert(
        "tenant".into(),
        Resources {
            slots: 2,
            memory_bytes: 2 * full().memory_bytes,
            cpu_millis: full().cpu_millis,
        },
    );
    let mut s = Scheduler::open(&temp.path().join("journal"), config).unwrap();
    let key = start(&mut s);
    s.request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let paused = s
        .poll(poll(vec![key.clone()], free(&s)), 4)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(paused), 5).unwrap();
    let mut registration = worker();
    registration.capacity.cpu_millis *= 2;
    s.register(registration, 6).unwrap();
    s.submit(spec("two"), 7).unwrap();
    let other = s
        .poll(poll(vec![key.clone()], free(&s)), 8)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.request_control("one", request("resume", ControlAction::Resume), 9)
        .unwrap();
    assert_eq!(free(&s).cpu_millis, full().cpu_millis);
    assert!(
        s.poll(poll(vec![key.clone(), other.clone()], free(&s)), 10)
            .unwrap()
            .controls
            .is_empty()
    );
    s.drain("worker", true).unwrap();
    s.complete(
        Completion {
            key: other,
            result: None,
            error: Some("done".into()),
            artifacts: None,
            artifact_error: None,
        },
        11,
    )
    .unwrap();
    // Drain denies new tasks, but permits control of existing attempts.
    let resumed = s.poll(poll(vec![key], free(&s)), 12).unwrap();
    assert!(resumed.assignments.is_empty());
    assert_eq!(resumed.controls.len(), 1);
    assert_eq!(s.workers()[0].reserved, full());
    s.acknowledge_control(success(resumed.controls[0].clone()), 13)
        .unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
}

#[test]
fn stale_forged_or_mismatched_observations_never_change_reservations() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut s);
    let record = s
        .request_control("one", request("offload", ControlAction::Offload), 3)
        .unwrap();
    assert!(s.acknowledge_control(success(record.command), 4).is_err()); // never issued
    let command = s
        .poll(poll(vec![key], free(&s)), 5)
        .unwrap()
        .controls
        .remove(0);
    let mut wrong = success(command.clone());
    wrong.command.key.incarnation = "other".into();
    assert!(s.acknowledge_control(wrong, 6).is_err());
    let mut wrong = success(command.clone());
    wrong.command.revision += 1;
    assert!(s.acknowledge_control(wrong, 6).is_err());
    let mut wrong = success(command.clone());
    wrong.outcome = ControlOutcome::Succeeded {
        state: VmState::Paused,
        memory: None,
    };
    assert!(s.acknowledge_control(wrong, 6).is_err());
    let mut wrong = success(command.clone());
    if let ControlOutcome::Succeeded {
        memory: Some(ref mut memory),
        ..
    } = wrong.outcome
    {
        memory.resident_after_bytes = Some(u64::MAX);
    }
    assert!(s.acknowledge_control(wrong, 6).is_err());
    assert_eq!(s.workers()[0].reserved, full());
    let acknowledged = s.acknowledge_control(success(command.clone()), 7).unwrap();
    let wrong = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Failed {
            error: "conflicting".into(),
        },
    };
    assert!(s.acknowledge_control(wrong, 8).is_err());
    assert_eq!(s.task("one").unwrap().controls[0], acknowledged);
}

#[test]
fn cancellation_and_expiry_abort_controls_without_accepting_late_success() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut s);
    s.request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 4)
        .unwrap()
        .controls
        .remove(0);
    s.cancel("one", 5).unwrap();
    assert_eq!(
        s.task("one").unwrap().controls[0].phase,
        ControlPhase::Aborted
    );
    assert!(s.acknowledge_control(success(command.clone()), 6).is_err());
    assert_eq!(s.workers()[0].reserved, full());
    assert!(
        s.poll(poll(vec![key], free(&s)), 7)
            .unwrap()
            .controls
            .is_empty()
    );
    s.reap(1007).unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Lost);
    assert!(s.acknowledge_control(success(command), 1008).is_err());
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn failed_resume_keeps_reserved_cpu_until_native_run_termination_is_acknowledged() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut s);
    s.request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 4)
        .unwrap()
        .controls
        .remove(0);
    s.acknowledge_control(success(command), 5).unwrap();
    s.request_control("one", request("resume", ControlAction::Resume), 6)
        .unwrap();
    let command = s
        .poll(poll(vec![key.clone()], free(&s)), 7)
        .unwrap()
        .controls
        .remove(0);
    let ack = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Failed {
            error: "transition outcome unknown".into(),
        },
    };
    assert_eq!(
        s.acknowledge_control(ack, 8).unwrap().phase,
        ControlPhase::Failed
    );
    assert_eq!(s.workers()[0].reserved, full());
    s.complete(
        Completion {
            key,
            result: None,
            error: Some("native teardown done".into()),
            artifacts: None,
            artifact_error: None,
        },
        9,
    )
    .unwrap();
    assert_eq!(s.workers()[0].reserved, Resources::default());
}

#[test]
fn older_worker_without_vm_control_support_cannot_accept_control_requests() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut registration = worker();
    registration.vm_control_protocol = None;
    s.register(registration, 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    s.poll(poll(vec![], free(&s)), 1).unwrap();
    assert!(
        s.request_control("one", request("pause", ControlAction::Pause), 2)
            .is_err()
    );
    assert!(s.task("one").unwrap().controls.is_empty());
}

#[test]
fn shared_pool_worker_rejects_offload_but_accepts_pause() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let mut registration = worker();
    registration.vm_control_actions = vec![ControlAction::Pause, ControlAction::Resume];
    s.register(registration, 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let key = s
        .poll(poll(vec![], free(&s)), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    assert!(
        s.request_control("one", request("offload", ControlAction::Offload), 2)
            .is_err()
    );
    assert!(s.task("one").unwrap().controls.is_empty());
    assert_eq!(s.workers()[0].reserved, full());
    let pause = s
        .request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let response = s.poll(poll(vec![key], free(&s)), 4).unwrap();
    assert!(response.stop.is_empty());
    assert_eq!(response.controls, vec![pause.command]);
}

#[test]
fn admission_rejection_aborts_attempt_scoped_controls_before_requeue() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    s.register(worker(), 0).unwrap();
    s.submit(spec("one"), 0).unwrap();
    let key = s
        .poll(poll(vec![], free(&s)), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    let control = s
        .request_control("one", request("pause", ControlAction::Pause), 2)
        .unwrap();
    s.decline(
        AdmissionRejection {
            key,
            reason: "not started".into(),
        },
        3,
    )
    .unwrap();
    assert_eq!(
        s.task("one").unwrap().controls[0].phase,
        ControlPhase::Aborted
    );
    assert!(s.acknowledge_control(success(control.command), 4).is_err());
    let next = s
        .poll(poll(vec![], free(&s)), 5)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    assert!(
        s.poll(poll(vec![next.clone()], free(&s)), 6)
            .unwrap()
            .controls
            .is_empty()
    );
    let pause = s
        .request_control("one", request("new-pause", ControlAction::Pause), 7)
        .unwrap();
    assert_eq!(pause.command.key, next);
    assert_eq!(pause.command.revision, 2);
}

#[test]
fn node_pressure_allows_pause_and_renewal_but_defers_resume_without_losing_state() {
    use pvisor_cluster::admission::AdmissionPolicy;
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut s);
    let policy = AdmissionPolicy {
        mode: AdmissionMode::LinuxPressure,
        ..Default::default()
    };
    let failed = policy
        .report(
            worker().capacity,
            full(),
            0,
            Err("node probe unavailable".into()),
        )
        .unwrap();
    let pressured_poll = || {
        let mut request = poll(vec![key.clone()], failed.available);
        request.admission = Some(failed.clone());
        request
    };
    s.request_control("one", request("pause", ControlAction::Pause), 3)
        .unwrap();
    let paused = s.poll(pressured_poll(), 4).unwrap().controls.remove(0);
    s.acknowledge_control(success(paused), 5).unwrap();
    s.request_control("one", request("resume", ControlAction::Resume), 6)
        .unwrap();
    let deferred = s.poll(pressured_poll(), 7).unwrap();
    assert_eq!(deferred.renewed, vec![key.clone()]);
    assert!(deferred.controls.is_empty());
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Paused);
    assert_eq!(
        s.task("one").unwrap().controls[1].phase,
        ControlPhase::Pending
    );
    let resumed = s
        .poll(poll(vec![key.clone()], free(&s)), 8)
        .unwrap()
        .controls
        .remove(0);
    // A pressure change after issue does not undo a durable CPU charge; the
    // same command can be redelivered and gated again at the native worker.
    assert_eq!(
        s.poll(pressured_poll(), 9).unwrap().controls,
        vec![resumed.clone()]
    );
    assert_eq!(s.workers()[0].reserved, full());
    s.acknowledge_control(success(resumed), 10).unwrap();
    assert_eq!(s.task("one").unwrap().phase, TaskPhase::Running);
}

#[test]
fn live_fork_and_wal_preserve_cpu_qos_and_reject_restore_class_changes() {
    use pvisor_core::CpuQosClass::LatencySensitive;
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let source = running_fork_source_qos(
        &mut scheduler,
        serde_json::json!({}),
        Some(LatencySensitive),
    );
    let request = fork_request();
    scheduler
        .request_live_fork("one", request.clone(), 3)
        .unwrap();
    let ack = capture_ack(&mut scheduler, &source);
    scheduler.acknowledge_control(ack, 5).unwrap();
    for id in ["left", "right"] {
        let task = scheduler.task(id).unwrap();
        assert_eq!(task.spec.cpu_qos, Some(LatencySensitive));
        assert_eq!(task.spec.resources, full());
        assert_eq!(task.phase, TaskPhase::Queued);
    }
    let mut changed = scheduler.task("left").unwrap().spec.clone();
    changed.id = "changed".into();
    changed.run.run_id = "changed".into();
    changed.cpu_qos = Some(pvisor_core::CpuQosClass::BestEffort);
    assert!(scheduler.submit(changed, 6).is_err());
    scheduler
        .complete(
            Completion {
                key: source,
                result: None,
                error: Some("source ended".into()),
                artifacts: None,
                artifact_error: None,
            },
            7,
        )
        .unwrap();
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let assignment = scheduler
        .poll(poll(vec![], free(&scheduler)), 8)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(assignment.spec.cpu_qos, Some(LatencySensitive));
    assert_eq!(scheduler.workers()[0].reserved, full());
    assert_eq!(scheduler.task("right").unwrap().phase, TaskPhase::Queued);
}

fn cpu_report(key: &LeaseKey, sequence: u64) -> CpuReportRequest {
    CpuReportRequest {
        worker_id: key.worker_id.clone(),
        incarnation: key.incarnation.clone(),
        samples: vec![AttemptCpuSample {
            key: key.clone(),
            sequence,
            sample_age_ms: 1,
            sample: pvisor_core::cpu::RunCpuSample {
                run_id: "one".into(),
                attempt_id: "synthetic-native-one".into(),
                sampled_at_unix_ms: sequence + 1,
                error: None,
                usage: Some(pvisor_core::cpu::ProcessCpuUsage {
                    pid: 123,
                    start_time_ticks: 99,
                    clock_ticks_per_second: 100,
                    sampled_monotonic_ns: sequence * 1_000_000_000,
                    threads: 2,
                    user_time_ticks: sequence * 70,
                    system_time_ticks: sequence * 30,
                    guest_time_ticks: sequence * 60,
                }),
            },
        }],
    }
}
fn enable_cpu(scheduler: &mut Scheduler) {
    let mut registration = worker();
    registration.cpu_observation_protocol =
        Some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION);
    scheduler.register(registration, 3).unwrap();
}

fn cpu_completion(key: &LeaseKey) -> Completion {
    let mut completion = suspended_completion(key.clone());
    let result = completion.result.as_mut().unwrap();
    result.attempt_id = "synthetic-native-one".into();
    result.state = pvisor_core::RunState::Completed;
    result.exit_code = Some(0);
    result.value = None;
    result.executor_observations.cpu_usage = Some(pvisor_core::cpu::TerminalCpuUsage::Measured {
        usage: cpu_report(key, 3).samples[0].sample.usage.clone().unwrap(),
    });
    completion
}

#[test]
fn final_cpu_counters_are_fenced_persisted_and_idempotent_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let key = start(&mut scheduler);
    enable_cpu(&mut scheduler);
    scheduler.report_cpu(cpu_report(&key, 1), 10).unwrap();
    scheduler.report_memory(memory_report(&key, 1), 11).unwrap();
    let valid = cpu_completion(&key);
    let wal = std::fs::read(&journal).unwrap();
    for case in 0..10 {
        let mut invalid = valid.clone();
        let result = invalid.result.as_mut().unwrap();
        if case == 0 {
            result.attempt_id = "wrong-attempt".into();
        } else {
            let Some(pvisor_core::cpu::TerminalCpuUsage::Measured { usage }) =
                &mut result.executor_observations.cpu_usage
            else {
                unreachable!()
            };
            match case {
                1 => usage.pid += 1,
                2 => usage.start_time_ticks += 1,
                3 => usage.clock_ticks_per_second += 1,
                4 => usage.sampled_monotonic_ns = 1_000_000_000,
                5 => usage.user_time_ticks = 69,
                6 => usage.system_time_ticks = 29,
                7 => usage.guest_time_ticks = 59,
                8 => usage.threads = 0,
                9 => {
                    result.executor_observations.cpu_usage =
                        Some(pvisor_core::cpu::TerminalCpuUsage::Unavailable { error: "".into() })
                }
                _ => unreachable!(),
            }
        }
        assert!(scheduler.complete(invalid, 20).is_err(), "case {case}");
        assert_eq!(std::fs::read(&journal).unwrap(), wal);
        assert_eq!(scheduler.workers()[0].reserved, full());
    }
    let terminal = scheduler.complete(valid.clone(), 20).unwrap();
    assert_eq!(terminal.phase, TaskPhase::Succeeded);
    assert!(terminal.cpu_sample.is_none());
    assert_eq!(scheduler.workers()[0].reserved, Resources::default());
    let evidence = terminal.result.unwrap().executor_observations.cpu_usage;
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .result
            .unwrap()
            .executor_observations
            .cpu_usage,
        evidence
    );
    scheduler.complete(valid.clone(), 21).unwrap();
    let mut conflict = valid;
    conflict
        .result
        .as_mut()
        .unwrap()
        .executor_observations
        .cpu_usage = Some(pvisor_core::cpu::TerminalCpuUsage::unavailable(
        "counter missing",
    ));
    assert!(scheduler.complete(conflict, 22).is_err());
}

#[test]
fn final_cpu_unavailability_is_explicit_and_memory_binding_alone_fences_it() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut scheduler);
    scheduler.report_memory(memory_report(&key, 1), 10).unwrap();
    let mut completion = cpu_completion(&key);
    let mut wrong = completion.clone();
    let Some(pvisor_core::cpu::TerminalCpuUsage::Measured { usage }) = &mut wrong
        .result
        .as_mut()
        .unwrap()
        .executor_observations
        .cpu_usage
    else {
        unreachable!()
    };
    usage.pid += 1;
    assert!(scheduler.complete(wrong, 20).is_err());
    completion
        .result
        .as_mut()
        .unwrap()
        .executor_observations
        .cpu_usage = Some(pvisor_core::cpu::TerminalCpuUsage::unavailable(
        "pidfd unsupported",
    ));
    let terminal = scheduler.complete(completion, 20).unwrap();
    assert!(matches!(
        terminal.result.unwrap().executor_observations.cpu_usage,
        Some(pvisor_core::cpu::TerminalCpuUsage::Unavailable { .. })
    ));
}
#[test]
fn cpu_counters_and_rates_preserve_wal_leases_budgets_and_restart_fencing() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    let key = start(&mut scheduler);
    assert!(scheduler.report_cpu(cpu_report(&key, 1), 4).is_err());
    enable_cpu(&mut scheduler);
    let before = scheduler.task("one").unwrap();
    let wal = std::fs::read(&journal).unwrap();
    let first = cpu_report(&key, 1);
    assert_eq!(
        scheduler.report_cpu(first.clone(), 10).unwrap().accepted,
        vec![key.clone()]
    );
    assert!(
        scheduler
            .task("one")
            .unwrap()
            .cpu_sample
            .unwrap()
            .interval
            .is_none()
    );
    scheduler.report_cpu(cpu_report(&key, 2), 20).unwrap();
    let sample = scheduler.task("one").unwrap().cpu_sample.unwrap();
    assert_eq!(
        sample.interval.unwrap(),
        pvisor_core::cpu::CpuIntervalUsage {
            elapsed_monotonic_ns: 1_000_000_000,
            total_cpu_time_ns: 1_000_000_000,
            cpu_millis: 1000,
        }
    );
    assert_eq!(
        scheduler.report_cpu(first, 21).unwrap().ignored,
        vec![key.clone()]
    );
    scheduler.report_cpu(cpu_report(&key, 2), 22).unwrap();
    let after = scheduler.task("one").unwrap();
    assert_eq!(
        after.lease.as_ref().unwrap().expires_at_ms,
        before.lease.unwrap().expires_at_ms
    );
    assert_eq!(after.current_reservation(), full());
    assert_eq!(scheduler.workers()[0].reserved, full());
    assert_eq!(std::fs::read(&journal).unwrap(), wal);
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, config()).unwrap();
    assert!(scheduler.task("one").unwrap().cpu_sample.is_none());
    assert_eq!(
        scheduler
            .report_cpu(cpu_report(&key, 3), 29)
            .unwrap()
            .ignored,
        vec![key.clone()]
    );
    scheduler
        .poll(poll(vec![key.clone()], free(&scheduler)), 30)
        .unwrap();
    scheduler.report_cpu(cpu_report(&key, 3), 30).unwrap();
    assert!(
        scheduler
            .task("one")
            .unwrap()
            .cpu_sample
            .unwrap()
            .interval
            .is_none()
    );
    scheduler
        .complete(
            Completion {
                key: key.clone(),
                result: None,
                error: Some("ended".into()),
                artifacts: None,
                artifact_error: None,
            },
            40,
        )
        .unwrap();
    assert!(scheduler.task("one").unwrap().cpu_sample.is_none());
    assert_eq!(
        scheduler
            .report_cpu(cpu_report(&key, 4), 50)
            .unwrap()
            .ignored,
        vec![key]
    );
}
#[test]
fn cpu_report_validates_batch_bounds_sequences_identity_monotonicity_and_errors_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let key = start(&mut scheduler);
    enable_cpu(&mut scheduler);
    scheduler.report_cpu(cpu_report(&key, 1), 10).unwrap();
    let mut unavailable = cpu_report(&key, 2);
    unavailable.samples[0].sample.usage = None;
    unavailable.samples[0].sample.error = Some("probe temporarily unavailable".into());
    scheduler.report_cpu(unavailable.clone(), 20).unwrap();
    for case in 0..12 {
        let mut invalid = cpu_report(&key, 3);
        let sample = &mut invalid.samples[0];
        match case {
            0 => sample.sample.attempt_id = "different-attempt".into(),
            1 => sample.sample.run_id = "different-run".into(),
            2 => sample.sample.usage.as_mut().unwrap().pid += 1,
            3 => sample.sample.usage.as_mut().unwrap().start_time_ticks += 1,
            4 => sample.sample.usage.as_mut().unwrap().clock_ticks_per_second += 1,
            5 => sample.sample.usage.as_mut().unwrap().sampled_monotonic_ns = 1,
            6 => sample.sample.usage.as_mut().unwrap().user_time_ticks = 0,
            7 => sample.sample.usage.as_mut().unwrap().system_time_ticks = 0,
            8 => sample.sample.usage.as_mut().unwrap().guest_time_ticks = 0,
            9 => sample.sequence = 0,
            10 => sample.sample.error = Some("both outcomes".into()),
            11 => invalid.incarnation = "stale-epoch".into(),
            _ => unreachable!(),
        }
        assert!(scheduler.report_cpu(invalid, 30).is_err(), "case {case}");
        assert_eq!(
            scheduler.task("one").unwrap().cpu_sample.unwrap().report,
            unavailable.samples[0]
        );
    }
    let mut conflict = unavailable;
    conflict.samples[0].sample.error = Some("conflicting sequence".into());
    assert!(scheduler.report_cpu(conflict, 30).is_err());
    let mut invalid = cpu_report(&key, 3);
    let mut duplicate = invalid.samples[0].clone();
    duplicate.sequence = 4;
    invalid.samples.push(duplicate);
    assert!(scheduler.report_cpu(invalid, 30).is_err());
    let mut invalid = cpu_report(&key, 3);
    invalid.samples[0].key.task_id = "ended-foreign".into();
    invalid.samples[0].sample.usage = None;
    invalid.samples[0].sample.error = None;
    invalid
        .samples
        .insert(0, cpu_report(&key, 3).samples.remove(0));
    assert!(scheduler.report_cpu(invalid, 30).is_err());
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .cpu_sample
            .unwrap()
            .report
            .sequence,
        2
    );
    let mut oversized = cpu_report(&key, 3);
    oversized.samples = vec![oversized.samples[0].clone(); 65];
    assert!(scheduler.report_cpu(oversized, 30).is_err());
    scheduler.report_cpu(cpu_report(&key, 3), 40).unwrap();
    assert_eq!(
        scheduler
            .task("one")
            .unwrap()
            .cpu_sample
            .unwrap()
            .interval
            .unwrap()
            .elapsed_monotonic_ns,
        2_000_000_000,
        "unavailable samples must preserve the last successful baseline"
    );
}
#[test]
fn cpu_and_memory_streams_share_attempt_and_native_process_fences_in_both_orders() {
    for cpu_first in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let mut scheduler = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
        let key = start(&mut scheduler);
        enable_cpu(&mut scheduler);
        if cpu_first {
            scheduler.report_cpu(cpu_report(&key, 1), 10).unwrap();
        } else {
            scheduler.report_memory(memory_report(&key, 1), 10).unwrap();
        }
        for case in 0..3 {
            let result = if cpu_first {
                let mut invalid = memory_report(&key, 1);
                match case {
                    0 => invalid.samples[0].sample.attempt_id = "another".into(),
                    1 => invalid.samples[0].sample.usage.as_mut().unwrap().pid += 1,
                    2 => {
                        invalid.samples[0]
                            .sample
                            .usage
                            .as_mut()
                            .unwrap()
                            .start_time_ticks += 1
                    }
                    _ => unreachable!(),
                }
                scheduler.report_memory(invalid, 20).map(|_| ())
            } else {
                let mut invalid = cpu_report(&key, 1);
                match case {
                    0 => invalid.samples[0].sample.attempt_id = "another".into(),
                    1 => invalid.samples[0].sample.usage.as_mut().unwrap().pid += 1,
                    2 => {
                        invalid.samples[0]
                            .sample
                            .usage
                            .as_mut()
                            .unwrap()
                            .start_time_ticks += 1
                    }
                    _ => unreachable!(),
                }
                scheduler.report_cpu(invalid, 20).map(|_| ())
            };
            assert!(result.is_err(), "CPU first {cpu_first}, case {case}");
        }
        scheduler.report_cpu(cpu_report(&key, 1), 30).unwrap();
        scheduler.report_memory(memory_report(&key, 1), 30).unwrap();
        assert_eq!(scheduler.workers()[0].reserved, full());
        let expires = scheduler.task("one").unwrap().lease.unwrap().expires_at_ms;
        assert_eq!(
            scheduler
                .report_cpu(cpu_report(&key, 2), expires)
                .unwrap()
                .ignored,
            vec![key.clone()]
        );
        assert_eq!(
            scheduler
                .task("one")
                .unwrap()
                .cpu_sample
                .unwrap()
                .report
                .sequence,
            1
        );
        scheduler.reap(expires).unwrap();
        assert!(scheduler.task("one").unwrap().cpu_sample.is_none());
    }
}

fn checkpoint_repository(publish: bool) -> CheckpointStorageSupport {
    CheckpointStorageSupport {
        version: 1,
        repository: "checkpoints".into(),
        publish,
        compatibility: pvisor_core::operation::SnapshotCompatibility {
            host_boot: "owned-host-boot".into(),
            build: "a".repeat(64),
            firmware: "b".repeat(64),
            profile: "native-profile".into(),
        },
    }
}
fn checkpoint_worker(publish: bool) -> WorkerRegistration {
    let mut registration = worker();
    registration
        .vm_control_actions
        .extend([ControlAction::Checkpoint, ControlAction::Suspend]);
    registration.execution_restore_protocol = Some(CLUSTER_VERSION);
    registration.artifact_protocol = Some(CLUSTER_VERSION);
    registration.artifact_export = Some(ArtifactExportSupport {
        version: ARTIFACT_EXPORT_VERSION,
        trace: false,
        workspace_upper: false,
        execution_checkpoint: true,
    });
    registration.checkpoint_storage = Some(checkpoint_repository(publish));
    if !publish {
        registration.artifact_export = None;
    }
    registration
}
fn checkpoint_task() -> TaskSpec {
    let mut task = spec("one");
    task.retain_artifacts = Some(ArtifactRetention {
        version: ARTIFACT_EXPORT_VERSION,
        trace: false,
        workspace_upper: false,
        execution_checkpoint: Some(CheckpointRetention {
            version: 1,
            repository: "checkpoints".into(),
        }),
    });
    task
}
fn checkpoint_manifest(s: &Scheduler, completion: &Completion, bytes: &[u8]) -> BlobRef {
    let bundle = serde_json::to_vec(
        &serde_json::json!({"schema_version":4,"run":completion.result.as_ref().unwrap()}),
    )
    .unwrap();
    let files = [
        ("run-bundle.json", bundle.as_slice()),
        ("execution-checkpoint.json", bytes),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        let chunk = s.artifact_store().put(bytes).unwrap();
        ArtifactFile {
            name: name.into(),
            bytes: bytes.len() as u64,
            digest: chunk.digest.clone(),
            chunks: vec![chunk],
        }
    })
    .collect();
    let manifest = ArtifactManifest {
        version: CLUSTER_VERSION,
        key: completion.key.clone(),
        files,
    };
    s.artifact_store()
        .put(&serde_json::to_vec(&manifest).unwrap())
        .unwrap()
}

#[test]
fn checkpoint_export_requires_explicit_matching_writable_repository_and_native_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    let mut s = Scheduler::open(&temp.path().join("journal"), config()).unwrap();
    let task = checkpoint_task();
    task.validate_artifacts().unwrap();
    assert_eq!(
        task.retain_artifacts.as_ref().unwrap().filenames(),
        ["run-bundle.json", "execution-checkpoint.json"]
    );
    let old = serde_json::to_value(worker()).unwrap();
    assert!(old.get("checkpoint_storage").is_none());
    assert!(
        serde_json::from_value::<WorkerRegistration>(old)
            .unwrap()
            .checkpoint_storage
            .is_none()
    );
    for variant in 0..4 {
        let mut node = checkpoint_worker(true);
        match variant {
            0 => node.checkpoint_storage = None,
            1 => node.checkpoint_storage.as_mut().unwrap().publish = false,
            2 => node
                .vm_control_actions
                .retain(|action| *action != ControlAction::Suspend),
            _ => node.execution_restore_protocol = None,
        }
        assert!(
            s.register(node, 0).is_err(),
            "invalid export capability variant {variant}"
        );
    }
    s.submit(task.clone(), 0).unwrap();
    for variant in 0..3 {
        let mut node = checkpoint_worker(false);
        if variant == 1 {
            node.checkpoint_storage = None;
        }
        if variant == 2 {
            node = checkpoint_worker(true);
            node.checkpoint_storage.as_mut().unwrap().repository = "different-repository".into();
        }
        s.register(node, 0).unwrap();
        assert!(
            s.poll(poll(vec![], full()), 1)
                .unwrap()
                .assignments
                .is_empty(),
            "ineligible checkpoint exporter {variant}"
        );
    }
    s.register(checkpoint_worker(true), 1).unwrap();
    assert_eq!(
        s.poll(poll(vec![], full()), 2).unwrap().assignments[0]
            .spec
            .id,
        "one"
    );
    for repository in ["", ".", "..", "s3://private-bucket/key", "../escape"] {
        assert!(
            CheckpointRetention {
                version: 1,
                repository: repository.into()
            }
            .validate()
            .is_err()
        );
    }
    let mut spoof = spec("spoof");
    spoof.run.metadata.insert(
        "pvisor.orchestration.checkpoint_publication".into(),
        serde_json::Value::Null,
    );
    assert!(s.submit(spoof, 3).is_err());
}

#[test]
fn checkpoint_publication_binds_native_receipt_and_wal_replay_places_only_compatible_workers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = Scheduler::open(&path, config()).unwrap();
    s.register(checkpoint_worker(true), 0).unwrap();
    s.submit(checkpoint_task(), 0).unwrap();
    let source = s
        .poll(poll(vec![], full()), 1)
        .unwrap()
        .assignments
        .remove(0)
        .lease
        .key;
    s.poll(poll(vec![source.clone()], Resources::default()), 2)
        .unwrap();
    s.request_control("one", request("earlier", ControlAction::Checkpoint), 2)
        .unwrap();
    let earlier_command = s
        .poll(poll(vec![source.clone()], Resources::default()), 2)
        .unwrap()
        .controls
        .remove(0);
    let mut earlier_checkpoint = pvisor_core::operation::ExecutionSuspension::from_result(
        suspended_completion(source.clone())
            .result
            .as_ref()
            .unwrap(),
    )
    .unwrap()
    .checkpoint;
    earlier_checkpoint.snapshot_id = "0".repeat(64);
    s.acknowledge_control(
        ControlAcknowledgement {
            command: earlier_command,
            outcome: ControlOutcome::Checkpointed {
                checkpoint: earlier_checkpoint.clone(),
            },
        },
        2,
    )
    .unwrap();
    s.request_control("one", request("suspend", ControlAction::Suspend), 2)
        .unwrap();
    s.poll(poll(vec![source.clone()], Resources::default()), 3)
        .unwrap();
    let mut completion = suspended_completion(source.clone());
    let checkpoint = pvisor_core::operation::ExecutionSuspension::from_result(
        completion.result.as_ref().unwrap(),
    )
    .unwrap()
    .checkpoint;
    let publication = CheckpointPublication {
        version: 1,
        repository: "checkpoints".into(),
        checkpoint: checkpoint.clone(),
        transfer: pvisor_core::operation::SnapshotTransfer {
            version: 1,
            snapshot_id: checkpoint.snapshot_id.clone(),
            transfer_id: "d".repeat(64),
        },
        compatibility: checkpoint_repository(true).compatibility,
    };
    for variant in 0..7 {
        let mut forged = publication.clone();
        let bytes = match variant {
            0 => Vec::new(),
            1 => {
                forged.repository = "other".into();
                serde_json::to_vec(&forged).unwrap()
            }
            2 => {
                forged.checkpoint.source_attempt_id = "foreign-attempt".into();
                serde_json::to_vec(&forged).unwrap()
            }
            3 => {
                forged.checkpoint.store = "/foreign/execution-snapshots".into();
                serde_json::to_vec(&forged).unwrap()
            }
            4 => {
                forged.transfer.snapshot_id = "0".repeat(64);
                serde_json::to_vec(&forged).unwrap()
            }
            5 => {
                forged.compatibility.build = "not-a-hash".into();
                serde_json::to_vec(&forged).unwrap()
            }
            _ => {
                forged.transfer.version = 2;
                serde_json::to_vec(&forged).unwrap()
            }
        };
        completion.artifacts = Some(checkpoint_manifest(&s, &completion, &bytes));
        assert!(
            s.complete(completion.clone(), 4).is_err(),
            "forged checkpoint publication {variant}"
        );
        assert!(s.task("one").unwrap().checkpoint_publication.is_none());
        assert!(!s.task("one").unwrap().phase.terminal());
    }
    completion.artifacts = Some(checkpoint_manifest(
        &s,
        &completion,
        &serde_json::to_vec(&publication).unwrap(),
    ));
    let stopped = s.complete(completion.clone(), 5).unwrap();
    assert_eq!(stopped.phase, TaskPhase::Suspended);
    assert_eq!(stopped.checkpoint_publication, Some(publication.clone()));
    assert_eq!(s.workers()[0].reserved, Resources::default());
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    assert_eq!(
        s.complete(completion, 6).unwrap().checkpoint_publication,
        Some(publication.clone())
    );
    let mut earlier_branch = spec("earlier-branch");
    earlier_branch.run.parent_run_id = Some("one".into());
    earlier_branch.restore = Some(ExecutionRestore {
        task_id: "one".into(),
        request_id: "earlier".into(),
    });
    s.submit(earlier_branch, 6).unwrap();
    let local = s
        .poll(poll(vec![], full()), 6)
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(local.checkpoint, Some(earlier_checkpoint));
    assert!(
        local.checkpoint_publication.is_none(),
        "a later suspension publication cannot replace an earlier checkpoint"
    );
    let mut branch = spec("branch");
    branch.run.parent_run_id = Some("one".into());
    branch.restore = Some(ExecutionRestore {
        task_id: "one".into(),
        request_id: "suspend".into(),
    });
    s.submit(branch, 6).unwrap();
    for variant in 0..7 {
        let mut other = checkpoint_worker(false);
        other.id = "other".into();
        match variant {
            0 => other.checkpoint_storage = None,
            1 => other.checkpoint_storage.as_mut().unwrap().repository = "other-repository".into(),
            2 => {
                other
                    .checkpoint_storage
                    .as_mut()
                    .unwrap()
                    .compatibility
                    .host_boot = "another-boot".into()
            }
            3 => {
                other
                    .checkpoint_storage
                    .as_mut()
                    .unwrap()
                    .compatibility
                    .build = "0".repeat(64)
            }
            4 => {
                other
                    .checkpoint_storage
                    .as_mut()
                    .unwrap()
                    .compatibility
                    .firmware = "0".repeat(64)
            }
            5 => {
                other
                    .checkpoint_storage
                    .as_mut()
                    .unwrap()
                    .compatibility
                    .profile = "another-profile".into()
            }
            _ => {
                other.execution_restore_protocol = None;
                other.checkpoint_storage = None;
            }
        }
        s.register(other.clone(), 7).unwrap();
        let request = PollRequest {
            worker_id: other.id,
            incarnation: other.incarnation,
            active: vec![],
            available: full(),
            max_assignments: 1,
            admission: None,
        };
        assert!(
            s.poll(request, 8).unwrap().assignments.is_empty(),
            "incompatible restore target {variant}"
        );
    }
    let mut other = checkpoint_worker(false);
    other.id = "other".into();
    s.register(other.clone(), 9).unwrap();
    let mut request = PollRequest {
        worker_id: other.id,
        incarnation: other.incarnation,
        active: vec![],
        available: full(),
        max_assignments: 1,
        admission: None,
    };
    let assigned = s.poll(request.clone(), 10).unwrap().assignments.remove(0);
    assert_eq!(assigned.checkpoint, Some(checkpoint));
    assert_eq!(assigned.checkpoint_publication, Some(publication.clone()));
    assert_eq!(assigned.lease.key.worker_id, "other");
    drop(s);
    let mut s = Scheduler::open(&path, config()).unwrap();
    let replay = s.poll(request.clone(), 11).unwrap().assignments.remove(0);
    assert_eq!(replay.lease.key, assigned.lease.key);
    assert_eq!(replay.checkpoint_publication, Some(publication));
    request.active.push(assigned.lease.key);
    request.available = Resources::default();
    assert!(s.poll(request, 12).unwrap().assignments.is_empty());
}
