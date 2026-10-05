//! Synthetic observations verify the durable protocol, not native VM effects.
use pvisor_cluster::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec, VmState};
use std::collections::BTreeMap;

fn resources() -> Resources {
    Resources {
        slots: 1,
        memory_bytes: 64 << 20,
        cpu_millis: 250,
    }
}
fn spec(id: &str) -> TaskSpec {
    let mut run = RunSpec::process(id, "agent", "/bin/true");
    let RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    run.capabilities.models = vec!["m".into()];
    run.runtime.max_output_bytes = 1024;
    TaskSpec {
        version: CLUSTER_VERSION,
        id: id.into(),
        tenant: "t".into(),
        run,
        execution: ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        },
        resources: resources(),
        labels: BTreeMap::new(),
        cache_keys: vec![],
        environment: None,
        retain_bundle: false,
        retain_artifacts: None,
        restore: None,
        cpu_qos: None,
        gateway: Some(GatewayRequirement {
            version: CLUSTER_VERSION,
            level: Default::default(),
            models: vec!["m".into()],
        }),
    }
}
fn registration() -> WorkerRegistration {
    serde_json::from_value(serde_json::json!({
        "version":1,"id":"w","incarnation":"i", "capacity":{"slots":2,"memory_bytes":134217728,"cpu_millis":250},
        "execution":[spec("fixture").execution], "labels":{}, "cache_keys":[],
        "vm_control_protocol":1,"vm_control_actions":["pause","resume"], "artifact_protocol":1,
        "gateway":{"version":1,"level":spec("fixture").gateway.unwrap().level,"model_patterns":["m"]}
    })).unwrap()
}
fn free(s: &Scheduler) -> Resources {
    let w = &s.workers()[0];
    w.registration.capacity.checked_sub(w.reserved).unwrap()
}
fn poll(s: &mut Scheduler, keys: &[LeaseKey]) -> PollResponse {
    s.poll(
        PollRequest {
            worker_id: "w".into(),
            incarnation: "i".into(),
            active: keys.to_vec(),
            available: free(s),
            max_assignments: 2,
            admission: None,
        },
        10,
    )
    .unwrap()
}
fn start(s: &mut Scheduler) -> LeaseKey {
    s.register(registration(), 0).unwrap();
    s.submit(spec("a"), 0).unwrap();
    let key = poll(s, &[]).assignments.remove(0).lease.key;
    poll(s, std::slice::from_ref(&key));
    key
}
fn key(lease: &LeaseKey, revision: u64) -> InferenceWaitKey {
    InferenceWaitKey {
        lease: lease.clone(),
        revision,
        call_id: format!("call-{revision}"),
    }
}
fn query(
    s: &mut Scheduler,
    key: &InferenceWaitKey,
    intent: InferenceWaitIntent,
) -> InferenceWaitReceipt {
    s.inference_wait(
        InferenceWaitRequest {
            key: key.clone(),
            intent,
        },
        10,
    )
    .unwrap()
}
fn ack(s: &mut Scheduler, command: ControlCommand) -> ControlAcknowledgement {
    let state = if command.request.action == ControlAction::Pause {
        VmState::Paused
    } else {
        VmState::Running
    };
    let ack = ControlAcknowledgement {
        command,
        outcome: ControlOutcome::Succeeded {
            state,
            memory: None,
        },
    };
    s.acknowledge_control(ack.clone(), 10).unwrap();
    ack
}
fn open(path: &std::path::Path) -> Scheduler {
    Scheduler::open(path, SchedulerConfig::default()).unwrap()
}

#[test]
fn pause_releases_only_cpu_and_ready_waits_for_competitor_and_native_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = open(&tmp.path().join("wal"));
    let lease = start(&mut s);
    let wait = key(&lease, 1);
    let begin = query(&mut s, &wait, InferenceWaitIntent::Begin);
    assert!(!begin.entered);
    s.submit(spec("b"), 0).unwrap();
    let pause = poll(&mut s, std::slice::from_ref(&lease));
    assert!(pause.assignments.is_empty());
    assert_eq!(free(&s).cpu_millis, 0);
    let old_pause = ack(&mut s, pause.controls[0].clone());
    assert!(query(&mut s, &wait, InferenceWaitIntent::Observe).entered);
    assert_eq!(
        s.task("a").unwrap().current_reservation(),
        Resources {
            cpu_millis: 0,
            ..resources()
        }
    );
    let competitor = poll(&mut s, std::slice::from_ref(&lease))
        .assignments
        .remove(0)
        .lease
        .key;
    assert!(!query(&mut s, &wait, InferenceWaitIntent::Ready).delivery_ready);
    assert!(
        poll(&mut s, &[lease.clone(), competitor.clone()])
            .controls
            .is_empty()
    );
    // A lost pause-ack reply is still idempotent after Ready requested resume.
    s.acknowledge_control(old_pause, 10).unwrap();
    s.complete(
        Completion {
            key: competitor,
            result: None,
            error: Some("done".into()),
            artifacts: None,
            artifact_error: None,
        },
        10,
    )
    .unwrap();
    let resume = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    assert_eq!(resume.request.action, ControlAction::Resume);
    assert_eq!(free(&s).cpu_millis, 0);
    assert!(!query(&mut s, &wait, InferenceWaitIntent::Observe).delivery_ready);
    ack(&mut s, resume);
    assert!(query(&mut s, &wait, InferenceWaitIntent::Observe).delivery_ready);
}

#[test]
fn cancellation_before_begin_and_before_issue_never_pauses_and_issued_cancel_resumes_after_replay()
{
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wal");
    let mut s = open(&path);
    let lease = start(&mut s);
    let first = key(&lease, 1);
    assert!(query(&mut s, &first, InferenceWaitIntent::Ready).delivery_ready);
    assert!(query(&mut s, &first, InferenceWaitIntent::Begin).entered);
    assert!(
        poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .is_empty()
    );
    let second = key(&lease, 2);
    query(&mut s, &second, InferenceWaitIntent::Begin);
    query(&mut s, &second, InferenceWaitIntent::Ready);
    assert!(
        poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .is_empty()
    );
    let third = key(&lease, 3);
    query(&mut s, &third, InferenceWaitIntent::Begin);
    let pause = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    query(&mut s, &third, InferenceWaitIntent::Ready);
    ack(&mut s, pause);
    drop(s);
    let mut s = open(&path);
    let resume = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    assert_eq!(resume.request.action, ControlAction::Resume);
    ack(&mut s, resume);
    assert!(query(&mut s, &third, InferenceWaitIntent::Observe).delivery_ready);
}

#[test]
fn manual_pause_revokes_ownership_and_wait_never_automatically_overrides_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wal");
    let mut s = open(&path);
    let lease = start(&mut s);
    let wait = key(&lease, 1);
    query(&mut s, &wait, InferenceWaitIntent::Begin);
    let pause = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    ack(&mut s, pause);
    s.request_control(
        "a",
        ControlRequest {
            request_id: "human-pause".into(),
            action: ControlAction::Pause,
        },
        10,
    )
    .unwrap();
    let manual = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    ack(&mut s, manual);
    let ready = query(&mut s, &wait, InferenceWaitIntent::Ready);
    assert!(ready.record.interrupted);
    assert!(!ready.delivery_ready);
    drop(s);
    let mut s = open(&path);
    assert!(
        poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .is_empty()
    );
    s.request_control(
        "a",
        ControlRequest {
            request_id: "human-resume".into(),
            action: ControlAction::Resume,
        },
        10,
    )
    .unwrap();
    let manual = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    ack(&mut s, manual);
    assert!(query(&mut s, &wait, InferenceWaitIntent::Observe).delivery_ready);
    assert_eq!(s.task("a").unwrap().controls.len(), 2);
}

#[test]
fn restart_requires_fresh_worker_report_before_wait_entry_or_resume_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wal");
    let mut s = open(&path);
    let lease = start(&mut s);
    let wait = key(&lease, 1);
    query(&mut s, &wait, InferenceWaitIntent::Begin);
    let pause = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    ack(&mut s, pause);
    drop(s);

    // Durable pause evidence alone cannot authorize the Gateway after restart.
    let mut s = open(&path);
    assert!(s.task("a").unwrap().reconciliation_pending);
    let before = s.inference_wait_record("a").unwrap().unwrap();
    for intent in [
        InferenceWaitIntent::Begin,
        InferenceWaitIntent::Ready,
        InferenceWaitIntent::Observe,
    ] {
        assert!(
            s.inference_wait(
                InferenceWaitRequest {
                    key: wait.clone(),
                    intent,
                },
                10,
            )
            .is_err()
        );
    }
    assert_eq!(s.inference_wait_record("a").unwrap().unwrap(), before);
    assert!(
        poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .is_empty()
    );
    assert!(query(&mut s, &wait, InferenceWaitIntent::Observe).entered);
    assert!(!query(&mut s, &wait, InferenceWaitIntent::Ready).delivery_ready);
    let revision = s
        .inference_wait_record("a")
        .unwrap()
        .unwrap()
        .resume_revision
        .unwrap();
    drop(s);

    // A durable pending resume survives another restart, but must reacquire
    // admission and receive native acknowledgement before delivering a reply.
    let mut s = open(&path);
    assert!(
        s.inference_wait(
            InferenceWaitRequest {
                key: wait.clone(),
                intent: InferenceWaitIntent::Ready,
            },
            10,
        )
        .is_err()
    );
    assert_eq!(s.task("a").unwrap().current_reservation().cpu_millis, 0);
    let resume = poll(&mut s, std::slice::from_ref(&lease))
        .controls
        .remove(0);
    assert_eq!(resume.revision, revision);
    assert_eq!(resume.request.action, ControlAction::Resume);
    assert_eq!(s.task("a").unwrap().current_reservation(), resources());
    assert!(!query(&mut s, &wait, InferenceWaitIntent::Observe).delivery_ready);
    ack(&mut s, resume);
    assert!(query(&mut s, &wait, InferenceWaitIntent::Observe).delivery_ready);
}

#[test]
fn thousands_of_waits_do_not_exhaust_manual_history_and_reject_old_keys_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wal");
    let mut s = open(&path);
    let lease = start(&mut s);
    for revision in 1..=2100 {
        let wait = key(&lease, revision);
        query(&mut s, &wait, InferenceWaitIntent::Begin);
        let pause = poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .remove(0);
        ack(&mut s, pause);
        query(&mut s, &wait, InferenceWaitIntent::Ready);
        let resume = poll(&mut s, std::slice::from_ref(&lease))
            .controls
            .remove(0);
        ack(&mut s, resume);
    }
    assert!(s.task("a").unwrap().controls.is_empty());
    drop(s);
    let mut s = open(&path);
    assert!(
        s.inference_wait(
            InferenceWaitRequest {
                key: key(&lease, 2100),
                intent: InferenceWaitIntent::Observe
            },
            10
        )
        .is_err()
    );
    poll(&mut s, std::slice::from_ref(&lease));
    assert!(query(&mut s, &key(&lease, 2100), InferenceWaitIntent::Observe).delivery_ready);
    for intent in [
        InferenceWaitIntent::Begin,
        InferenceWaitIntent::Ready,
        InferenceWaitIntent::Observe,
    ] {
        assert!(
            s.inference_wait(
                InferenceWaitRequest {
                    key: key(&lease, 1),
                    intent
                },
                10
            )
            .is_err()
        );
    }
    let mut wrong = key(&lease, 2100);
    wrong.call_id = "other".into();
    assert!(
        s.inference_wait(
            InferenceWaitRequest {
                key: wrong,
                intent: InferenceWaitIntent::Ready
            },
            10
        )
        .is_err()
    );
    let mut expired = key(&lease, 2100);
    expired.lease.incarnation = "old".into();
    assert!(
        s.inference_wait(
            InferenceWaitRequest {
                key: expired,
                intent: InferenceWaitIntent::Observe
            },
            10
        )
        .is_err()
    );
    assert!(
        s.inference_wait(
            InferenceWaitRequest {
                key: key(&lease, 2100),
                intent: InferenceWaitIntent::Ready
            },
            100_000
        )
        .is_err()
    );
}
