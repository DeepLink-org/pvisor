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
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "tenant".into(),
        run,
        execution: execution(),
        resources: full(),
        labels: BTreeMap::new(),
        cache_keys: vec![],
    }
}
fn worker() -> WorkerRegistration {
    WorkerRegistration {
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
    }
}
fn free(s: &Scheduler) -> Resources {
    s.workers()[0]
        .registration
        .capacity
        .checked_sub(s.workers()[0].reserved)
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
fn success(command: ControlCommand) -> ControlAcknowledgement {
    let (state, memory) = match command.request.action {
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
