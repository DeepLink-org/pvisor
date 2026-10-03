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
    }
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
