//! Synthetic node observations verify fencing/persistence, not physical density.
use pvisor_cluster::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{
    ExecutorKind, IsolationKind,
    memory::{MemoryObservation, NodeMemorySample, ProcessMemory, ResidentMemory, SystemMemory},
};

fn registration() -> WorkerRegistration {
    WorkerRegistration {
        artifact_export: None,
        gateway: None,
        cpu_observation_protocol: None,
        cpu_qos_classes: vec![],
        version: CLUSTER_VERSION,
        id: "node-worker".into(),
        incarnation: "epoch".into(),
        capacity: Resources {
            slots: 1,
            memory_bytes: 64 * 1024 * 1024,
            cpu_millis: 1000,
        },
        execution: vec![ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        }],
        labels: Default::default(),
        cache_keys: vec![],
        vm_control_protocol: None,
        artifact_protocol: None,
        environment_support: None,
        vm_control_actions: vec![],
        execution_restore_protocol: None,
        parked_execution_suspend_protocol: None,
    }
}
fn report(sequence: u64) -> NodeMemoryReportRequest {
    NodeMemoryReportRequest {
        worker_id: "node-worker".into(),
        incarnation: "epoch".into(),
        sequence,
        sample_age_ms: 12,
        sample: NodeMemorySample {
            sampled_at_unix_ms: sequence,
            supervisor: MemoryObservation::Measured {
                usage: ProcessMemory {
                    pid: 123,
                    start_time_ticks: 99,
                    process: ResidentMemory {
                        mapped_bytes: 8192,
                        rss_bytes: 4096,
                        pss_bytes: 4096,
                        private_dirty_bytes: 4096,
                        ..Default::default()
                    },
                },
            },
            system: MemoryObservation::Measured {
                usage: SystemMemory {
                    host_boot_id: "00000000-0000-0000-0000-000000000001".into(),
                    total_bytes: 10000,
                    available_bytes: 4000,
                    free_bytes: 1000,
                    cached_bytes: 2000,
                    buffers_bytes: 100,
                    slab_bytes: 500,
                    swap_total_bytes: 0,
                    swap_free_bytes: 0,
                },
            },
            cgroup: MemoryObservation::Unavailable {
                error: "synthetic inaccessible cgroup".into(),
            },
        },
    }
}

#[test]
fn idle_worker_reports_do_not_renew_write_wal_or_refresh_replayed_measurements() {
    let temp = tempfile::tempdir().unwrap();
    let journal = temp.path().join("journal");
    let mut scheduler = Scheduler::open(&journal, SchedulerConfig::default()).unwrap();
    scheduler.register(registration(), 1).unwrap();
    let bytes = std::fs::read(&journal).unwrap();
    let previous = scheduler.workers().remove(0);
    let first = report(1);
    assert!(
        scheduler
            .report_node_memory(first.clone(), 100)
            .unwrap()
            .accepted
    );
    assert!(
        scheduler
            .report_node_memory(first.clone(), 200)
            .unwrap()
            .accepted
    );
    assert_eq!(
        scheduler.workers()[0]
            .memory_sample
            .as_ref()
            .unwrap()
            .received_at_ms,
        100
    );
    let second = report(2);
    assert!(
        scheduler
            .report_node_memory(second.clone(), 300)
            .unwrap()
            .accepted
    );
    assert!(!scheduler.report_node_memory(first, 400).unwrap().accepted);
    let observed = scheduler.workers().remove(0);
    assert_eq!(observed.memory_sample.unwrap().report, second);
    assert_eq!(observed.seen_at_ms, previous.seen_at_ms);
    assert_eq!(observed.reserved, previous.reserved);
    assert!(observed.admission.is_none());
    assert_eq!(std::fs::read(&journal).unwrap(), bytes);
    // Registration retries retain the original incarnation's bound identity.
    scheduler.register(registration(), 500).unwrap();
    assert_eq!(
        scheduler.workers()[0]
            .memory_sample
            .as_ref()
            .unwrap()
            .received_at_ms,
        300
    );
    drop(scheduler);
    let mut scheduler = Scheduler::open(&journal, SchedulerConfig::default()).unwrap();
    assert!(scheduler.workers()[0].memory_sample.is_none());
    assert!(
        scheduler
            .report_node_memory(report(1), 600)
            .unwrap()
            .accepted
    );
}

#[test]
fn node_errors_keep_identity_and_invalid_or_stale_reports_cannot_replace_data() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler =
        Scheduler::open(&temp.path().join("journal"), SchedulerConfig::default()).unwrap();
    scheduler.register(registration(), 1).unwrap();
    scheduler.report_node_memory(report(1), 100).unwrap();
    let mut failed = report(2);
    failed.sample.supervisor = MemoryObservation::Unavailable {
        error: "synthetic smaps failure".into(),
    };
    failed.sample.system = MemoryObservation::Unavailable {
        error: "synthetic meminfo failure".into(),
    };
    scheduler.report_node_memory(failed.clone(), 200).unwrap();
    scheduler.register(registration(), 201).unwrap();
    for case in 0..9 {
        let mut bad = report(3);
        match case {
            0 => bad.worker_id = "unknown".into(),
            1 => bad.incarnation = "stale".into(),
            2 => bad.sequence = 0,
            3 => bad.sample.sampled_at_unix_ms = 0,
            4 => {
                if let MemoryObservation::Measured { usage } = &mut bad.sample.supervisor {
                    usage.pid += 1;
                }
            }
            5 => {
                if let MemoryObservation::Measured { usage } = &mut bad.sample.supervisor {
                    usage.start_time_ticks += 1;
                }
            }
            6 => {
                if let MemoryObservation::Measured { usage } = &mut bad.sample.system {
                    usage.host_boot_id = "00000000-0000-0000-0000-000000000002".into();
                }
            }
            7 => {
                bad.sample.cgroup = MemoryObservation::Unavailable {
                    error: "x".repeat(1025),
                }
            }
            _ => {
                bad = failed.clone();
                bad.sample_age_ms += 1;
            }
        }
        assert!(scheduler.report_node_memory(bad, 300).is_err(), "{case}");
        assert_eq!(
            scheduler.workers()[0]
                .memory_sample
                .as_ref()
                .unwrap()
                .report,
            failed
        );
    }
    let mut next = registration();
    next.incarnation = "new-epoch".into();
    scheduler.register(next, 400).unwrap();
    assert!(scheduler.workers()[0].memory_sample.is_none());
    assert!(scheduler.report_node_memory(report(3), 401).is_err());
    let mut new = report(1);
    new.incarnation = "new-epoch".into();
    if let MemoryObservation::Measured { usage } = &mut new.sample.supervisor {
        usage.pid = 456;
        usage.start_time_ticks = 200;
    }
    scheduler.report_node_memory(new, 402).unwrap();
}

#[test]
fn node_memory_does_not_reap_extend_or_release_an_expired_execution() {
    let temp = tempfile::tempdir().unwrap();
    let mut scheduler = Scheduler::open(
        &temp.path().join("journal"),
        SchedulerConfig {
            lease_duration_ms: 100,
            ..Default::default()
        },
    )
    .unwrap();
    let worker = registration();
    scheduler.register(worker.clone(), 1).unwrap();
    let mut run = pvisor_core::RunSpec::process("run", "vm", "/bin/true");
    let pvisor_core::RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    scheduler
        .submit(
            TaskSpec {
                retain_artifacts: None,
                gateway: None,
                cpu_qos: None,
                version: CLUSTER_VERSION,
                id: "task".into(),
                tenant: "tenant".into(),
                run,
                execution: worker.execution[0].clone(),
                resources: worker.capacity,
                labels: Default::default(),
                cache_keys: vec![],
                retain_bundle: false,
                environment: None,
                restore: None,
            },
            2,
        )
        .unwrap();
    let assignment = scheduler
        .poll(
            PollRequest {
                worker_id: worker.id,
                incarnation: worker.incarnation,
                active: vec![],
                available: worker.capacity,
                max_assignments: 1,
                admission: None,
            },
            3,
        )
        .unwrap()
        .assignments
        .remove(0);
    let before = scheduler.task("task").unwrap();
    let worker_before = scheduler.workers().remove(0);
    assert!(
        scheduler
            .report_node_memory(report(1), 1000)
            .unwrap()
            .accepted
    );
    let after = scheduler.task("task").unwrap();
    assert_eq!(after.phase, TaskPhase::Leased);
    assert_eq!(
        after.lease.unwrap().expires_at_ms,
        assignment.lease.expires_at_ms
    );
    assert_eq!(after.updated_at_ms, before.updated_at_ms);
    assert_eq!(scheduler.workers()[0].reserved, worker_before.reserved);
    assert_eq!(scheduler.workers()[0].seen_at_ms, worker_before.seen_at_ms);
    assert_eq!(scheduler.reap(1001).unwrap(), 1);
    assert_eq!(scheduler.task("task").unwrap().phase, TaskPhase::Lost);
}
