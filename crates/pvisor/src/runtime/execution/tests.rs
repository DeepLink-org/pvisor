use super::*;
use pvisor_core::{AttemptId, AttemptInfo, ExecutorKind, ExecutorPlan, IsolationKind, RunId};

pub(super) fn execution() -> Execution {
    execution_with(RunState::Created, ExecutorKind::VirtualMachine)
}

fn execution_with(state: RunState, kind: ExecutorKind) -> Execution {
    let status = RunStatus {
        run_id: RunId::from("execution-test"),
        state,
        attempt: AttemptInfo {
            attempt_id: AttemptId::from("attempt-test"),
            number: 1,
            executor: ExecutorPlan {
                name: "test".into(),
                kind,
                isolation: IsolationKind::VirtualMachine,
                capability_plan: Default::default(),
                supports_checkpoint: true,
                supports_migration: false,
            },
            started_at_unix_ms: None,
            finished_at_unix_ms: None,
        },
        updated_at_unix_ms: 1,
        message: None,
    };
    let cancellation = CancellationToken::new();
    let (live, _) = tokio::sync::broadcast::channel(16);
    let events = RunEventPublisher::new(
        status.run_id.clone(),
        status.attempt.attempt_id.clone(),
        "test",
        Arc::new(super::super::event::NoopEventSink::default()),
        live,
    );
    Execution::new(
        status,
        crate::executor::vm::control::VmControl::new(cancellation.clone()),
        crate::PVisor::new()
            .resolve_operation(pvisor_core::RunSpec::process(
                "execution-test",
                "test",
                "/bin/true",
            ))
            .unwrap(),
        events,
        cancellation,
    )
    .unwrap()
}

fn native_controls() -> [OperationKind; 5] {
    use pvisor_core::operation::SnapshotRamStorage;
    [
        OperationKind::RunPause,
        OperationKind::RunResume,
        OperationKind::RunOffload { file: None },
        OperationKind::RunCheckpoint {
            request_id: "checkpoint".into(),
            ram_storage: SnapshotRamStorage::Raw,
        },
        OperationKind::RunSuspend {
            request_id: "suspend".into(),
            ram_storage: SnapshotRamStorage::Raw,
        },
    ]
}

#[tokio::test]
async fn execution_dispatch_status_and_terminate_work_without_vm_authority() {
    use pvisor_core::host_protocol::{AgentCtlHostErrorCode, HostAttemptCommand};
    let controls = execution_with(RunState::Created, ExecutorKind::Process);
    controls.ready().await;
    let reply = controls.dispatch(HostAttemptCommand::Status).await.unwrap();
    assert_eq!(reply.status.run_id, controls.status().run_id);
    assert_eq!(
        reply.status.attempt.attempt_id,
        controls.status().attempt.attempt_id
    );
    assert!(reply.value.is_none());
    for kind in native_controls()
        .into_iter()
        .chain([OperationKind::RunExecute {
            program: "/bin/true".into(),
            args: vec![],
            cwd: None,
        }])
    {
        let error = controls
            .dispatch(HostAttemptCommand::Operation { kind })
            .await
            .unwrap_err();
        assert_eq!(error.code, AgentCtlHostErrorCode::Unsupported);
    }
    assert!(!controls.cancellation.is_cancelled());
    let reply = controls
        .dispatch(HostAttemptCommand::Terminate)
        .await
        .unwrap();
    assert!(controls.cancellation.is_cancelled());
    assert!(reply.value.is_none());
    // A cancellation acknowledgement must not claim terminal publication.
    assert_eq!(reply.status.state, RunState::Running);
    controls.finish(RunState::Completed, None, 10).await;
    assert_eq!(
        controls
            .dispatch(HostAttemptCommand::Status)
            .await
            .unwrap()
            .status
            .state,
        RunState::Completed
    );
}

#[tokio::test]
async fn execution_dispatch_routes_all_native_primitives_through_control_checks() {
    use pvisor_core::host_protocol::{AgentCtlHostErrorCode, HostAttemptCommand};
    use pvisor_core::operation::{OperationKind, SnapshotRamStorage};
    let controls = execution_with(RunState::Starting, ExecutorKind::VirtualMachine);
    for kind in native_controls() {
        let error = controls
            .dispatch(HostAttemptCommand::Operation { kind })
            .await
            .unwrap_err();
        assert_eq!(error.code, AgentCtlHostErrorCode::Unavailable);
        assert!(
            error
                .message
                .contains("attempt is not running or suspended")
        );
    }
    let error = controls
        .dispatch(HostAttemptCommand::Operation {
            kind: OperationKind::RunExecute {
                program: "/bin/true".into(),
                args: vec![],
                cwd: None,
            },
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, AgentCtlHostErrorCode::Unsupported);
    let error = controls
        .dispatch(HostAttemptCommand::Operation {
            kind: OperationKind::RunCheckpoint {
                request_id: String::new(),
                ram_storage: SnapshotRamStorage::Raw,
            },
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, AgentCtlHostErrorCode::InvalidRequest);
    controls.cancel();
    let error = controls
        .dispatch(HostAttemptCommand::Operation {
            kind: OperationKind::RunPause,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, AgentCtlHostErrorCode::Unavailable);
    assert!(error.message.contains("attempt is cancelling"));
}

#[tokio::test(start_paused = true)]
async fn execution_readiness_waits_for_start_and_is_fenced_by_cancellation() {
    let controls = execution_with(RunState::Starting, ExecutorKind::VirtualMachine);
    let pending = controls.clone();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), pending.wait_ready())
            .await
            .is_err()
    );
    controls.ready().await;
    pending.wait_ready().await.unwrap();
    let id = controls
        .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
        .await
        .unwrap();
    controls.mark_dispatched(&id).await.unwrap();
    controls
        .succeeded(
            &id,
            Value::Vm {
                state: VmState::Paused,
                memory: None,
            },
        )
        .await
        .unwrap();
    pending.wait_ready().await.unwrap();
    controls.cancellation.cancel();
    assert!(pending.wait_ready().await.is_err());

    // Cancellation must wake a waiter even before any status update.
    let controls = execution_with(RunState::Starting, ExecutorKind::VirtualMachine);
    let pending = controls.clone();
    let waiter = tokio::spawn(async move { pending.wait_ready().await });
    tokio::task::yield_now().await;
    controls.cancellation.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn execution_readiness_rejects_terminal_and_non_vm_attempts() {
    let controls = execution_with(RunState::Starting, ExecutorKind::VirtualMachine);
    controls.finish(RunState::Completed, None, 10).await;
    assert!(controls.wait_ready().await.is_err());
    let controls = execution_with(RunState::Created, ExecutorKind::Process);
    controls.ready().await;
    assert!(controls.wait_ready().await.is_err());
}

async fn records(execution: &Execution) -> Vec<Record> {
    execution
        .owner
        .lock()
        .await
        .trace
        .journal()
        .records()
        .unwrap()
}

#[tokio::test]
async fn execution_history_replays_the_same_pause_resume_state() {
    let execution = execution();
    execution.transition(RunState::Running, None).await;
    for (kind, state) in [
        (OperationKind::RunPause, VmState::Paused),
        (OperationKind::RunResume, VmState::Running),
    ] {
        let id = execution
            .enqueue(kind, ControlOrigin::Manual)
            .await
            .unwrap();
        assert_eq!(execution.snapshot().progress.queued, 1);
        execution.mark_dispatched(&id).await.unwrap();
        assert_eq!(execution.snapshot().progress.in_flight, 1);
        execution
            .succeeded(
                &id,
                Value::Vm {
                    state,
                    memory: None,
                },
            )
            .await
            .unwrap();
    }
    let replayed = replay(&records(&execution).await).unwrap();
    assert_eq!(
        serde_json::to_value(&replayed.status).unwrap(),
        serde_json::to_value(execution.status()).unwrap()
    );
    assert!(replayed.pending.is_empty());
    assert!(!replayed.outcome_unknown);
    assert_eq!(replayed.applied, execution.snapshot().progress.applied);
}

#[tokio::test]
async fn execution_rejection_does_not_advance_the_cursor_or_launch_work() {
    let execution = execution();
    let before = execution.snapshot().progress.applied;
    assert!(
        execution
            .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
            .await
            .is_err()
    );
    assert_eq!(execution.snapshot().progress.applied, before);
    execution.transition(RunState::Running, None).await;
    let before = execution.snapshot().progress.applied;
    assert!(
        execution
            .enqueue(
                OperationKind::RunExecute {
                    program: "true".into(),
                    args: vec![],
                    cwd: None
                },
                ControlOrigin::Manual
            )
            .await
            .is_err()
    );
    assert_eq!(execution.snapshot().progress.applied, before);
    let before = execution.status();
    assert!(
        execution
            .commit(Input::Lifecycle {
                state: RunState::Running,
                message: Some("x".repeat(pvisor_core::event::MAX_EVENT_BYTES)),
                at: 9
            })
            .await
            .is_err()
    );
    assert_eq!(execution.status().state, before.state);
    assert_eq!(
        execution.snapshot().progress.applied,
        records(&execution)
            .await
            .last()
            .map(|record| record.position.clone())
    );
}

#[tokio::test]
async fn execution_failed_history_write_keeps_observations_and_allows_completion() {
    let execution = execution();
    let before = execution.snapshot().progress.applied;
    // A real journal rejection, without depending on OS disk-failure hooks.
    execution
        .transition(
            RunState::Running,
            Some("x".repeat(pvisor_core::event::MAX_EVENT_BYTES)),
        )
        .await;
    assert_eq!(execution.status().state, RunState::Running);
    assert!(execution.snapshot().progress.history_gap);
    assert_eq!(execution.snapshot().progress.applied, before);
    assert!(
        execution
            .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
            .await
            .is_err()
    );
    execution.finish(RunState::Cancelled, None, 10).await;
    assert_eq!(execution.status().state, RunState::Cancelled);
    assert_eq!(execution.status().attempt.finished_at_unix_ms, Some(10));
    assert!(execution.snapshot().progress.history_gap);
}

#[tokio::test]
async fn execution_pending_is_bounded_and_cancelled_admission_does_not_poison_controls() {
    let execution = execution();
    execution.transition(RunState::Running, None).await;
    let mut ids = Vec::new();
    for _ in 0..MAX_PENDING {
        ids.push(
            execution
                .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
                .await
                .unwrap(),
        );
    }
    let before = execution.snapshot().progress.applied;
    assert!(
        execution
            .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
            .await
            .is_err()
    );
    assert_eq!(execution.snapshot().progress.applied, before);
    execution
        .failed(&ids[0], &anyhow::anyhow!("cancelled before dispatch"))
        .await;
    assert!(!execution.snapshot().progress.outcome_unknown);
    execution
        .enqueue(OperationKind::RunResume, ControlOrigin::Manual)
        .await
        .unwrap();
    execution.mark_dispatched(&ids[1]).await.unwrap();
    execution
        .failed(
            &ids[1],
            &crate::executor::vm::control::ControlFailure::BeforeDispatch(anyhow::anyhow!(
                "unsupported before native transmission"
            ))
            .into(),
        )
        .await;
    assert!(!execution.snapshot().progress.outcome_unknown);
    let rejected = execution
        .enqueue(
            OperationKind::RunCheckpoint {
                request_id: "rejected".into(),
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Raw,
            },
            ControlOrigin::Manual,
        )
        .await
        .unwrap();
    execution.mark_dispatched(&rejected).await.unwrap();
    execution
        .failed(
            &rejected,
            &crate::executor::vm::control::ControlFailure::Rejected {
                state: VmState::Paused,
                message: "checkpoint requires a running source VM".into(),
            }
            .into(),
        )
        .await;
    assert_eq!(execution.status().state, RunState::Suspended);
    execution
        .transition(RunState::Running, Some("late ready".into()))
        .await;
    assert_eq!(execution.status().state, RunState::Suspended);
    assert!(!execution.snapshot().progress.outcome_unknown);
    assert_eq!(
        replay(&records(&execution).await).unwrap().status.state,
        RunState::Suspended
    );
    execution
        .enqueue(OperationKind::RunResume, ControlOrigin::Manual)
        .await
        .unwrap();
    execution.mark_dispatched(&ids[2]).await.unwrap();
    execution
        .failed(&ids[2], &anyhow::anyhow!("acknowledgement lost"))
        .await;
    assert!(execution.snapshot().progress.outcome_unknown);
    assert!(
        execution
            .enqueue(OperationKind::RunResume, ControlOrigin::Manual)
            .await
            .is_err()
    );
    assert!(replay(&records(&execution).await).unwrap().outcome_unknown);
}

#[tokio::test]
async fn execution_termination_remains_available_during_a_native_wait() {
    let execution = execution();
    execution.transition(RunState::Running, None).await;
    let id = execution
        .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
        .await
        .unwrap();
    execution.mark_dispatched(&id).await.unwrap();
    execution
        .transition(RunState::Cancelling, Some("cancelled".into()))
        .await;
    execution.finish(RunState::Cancelled, None, 50).await;
    execution
        .succeeded(
            &id,
            Value::Vm {
                state: VmState::Paused,
                memory: None,
            },
        )
        .await
        .unwrap();
    execution.transition(RunState::Running, None).await;
    assert_eq!(execution.status().state, RunState::Cancelled);
    assert_eq!(execution.status().attempt.finished_at_unix_ms, Some(50));
    assert_eq!(execution.snapshot().progress.in_flight, 0);
    assert_eq!(
        replay(&records(&execution).await).unwrap().status.state,
        RunState::Cancelled
    );
}

#[tokio::test]
async fn execution_commit_outlives_its_waiter_and_projects_the_committed_fact() {
    use std::{future::Future, task::Poll};
    for rejected in [false, true] {
        let execution = execution();
        let before = execution.snapshot().progress.applied;
        let guard = execution.owner.lock().await;
        let mut pending = Box::pin(execution.commit(Input::Lifecycle {
            state: RunState::Running,
            message: rejected.then(|| "x".repeat(pvisor_core::event::MAX_EVENT_BYTES)),
            at: 5,
        }));
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(pending);
        let mut status = execution.subscribe();
        drop(guard);
        tokio::time::timeout(std::time::Duration::from_secs(5), status.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(execution.status().state, RunState::Running);
        let snapshot = execution.snapshot();
        assert_eq!(snapshot.progress.history_gap, rejected);
        if rejected {
            assert_eq!(snapshot.progress.applied, before);
        }
        assert_eq!(
            replay(&records(&execution).await).unwrap().status.state,
            if rejected {
                RunState::Created
            } else {
                RunState::Running
            }
        );
    }
}

#[tokio::test]
async fn query_snapshot_stays_coherent_while_a_transition_waits_for_history() {
    use std::{future::Future, task::Poll};
    let execution = execution();
    let before = execution.snapshot();
    let guard = execution.owner.lock().await;
    let mut pending = Box::pin(execution.ready());
    std::future::poll_fn(|cx| {
        assert!(pending.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(
        serde_json::to_value(execution.snapshot()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    drop(guard);
    pending.await;
    assert_eq!(execution.snapshot().status.state, RunState::Running);
}

#[tokio::test]
async fn execution_retained_history_recovers_a_torn_tail_without_reexecuting() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let record: super::super::RunRecord = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run_id":"execution-test","attempt_id":"attempt-test","session_id":"session",
        "agent":"test","pid":0,"command":["true"],"state":"running","started_at_unix_ms":1,
        "storage":root.path(),"network":{},"gateway_listen":null,"overlay":null
    })).unwrap();
    let execution = execution();
    execution.retain(Some(&record)).await.unwrap();
    execution.transition(RunState::Running, None).await;
    let id = execution
        .enqueue(OperationKind::RunPause, ControlOrigin::Manual)
        .await
        .unwrap();
    execution.mark_dispatched(&id).await.unwrap();
    let path = root.path().join("execution-attempt-test.jsonl");
    assert!(
        Journal::read(&path).is_err(),
        "a second writer cannot steal the live journal"
    );
    drop(execution);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"incomplete\":")
        .unwrap();
    let recovered = Journal::open(&path).unwrap();
    let state = replay(&recovered.records().unwrap()).unwrap();
    assert!(state.pending[&id].dispatched);
    assert_eq!(state.status.state, RunState::Running);
    assert_eq!(state.pending.len(), 1);
    let mut records = recovered.records().unwrap();
    records.last_mut().unwrap().event.scope[3] = "foreign-attempt".into();
    assert!(replay(&records).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshots_match_a_single_history_prefix_during_native_results() {
    let execution = execution();
    execution.ready().await;
    let writer = execution.clone();
    let writes = tokio::spawn(async move {
        for _ in 0..24 {
            for (kind, state) in [
                (OperationKind::RunPause, VmState::Paused),
                (OperationKind::RunResume, VmState::Running),
            ] {
                let id = writer.enqueue(kind, ControlOrigin::Manual).await.unwrap();
                writer.mark_dispatched(&id).await.unwrap();
                writer
                    .succeeded(
                        &id,
                        Value::Vm {
                            state,
                            memory: None,
                        },
                    )
                    .await
                    .unwrap();
            }
        }
    });
    for _ in 0..80 {
        let snapshot = execution.snapshot();
        let history = records(&execution).await;
        let index = history
            .iter()
            .position(|record| Some(&record.position) == snapshot.progress.applied.as_ref())
            .unwrap();
        let reconstructed = replay(&history[..=index]).unwrap();
        assert_eq!(
            serde_json::to_value(snapshot.status).unwrap(),
            serde_json::to_value(reconstructed.status).unwrap()
        );
        let dispatched = reconstructed
            .pending
            .values()
            .filter(|op| op.dispatched)
            .count();
        assert_eq!(snapshot.progress.in_flight, dispatched);
        assert_eq!(
            snapshot.progress.queued + dispatched,
            reconstructed.pending.len()
        );
    }
    writes.await.unwrap();
}

#[cfg(feature = "gateway")]
#[tokio::test]
async fn cooperative_manual_rejection_keeps_the_attempt_and_delivery_available() {
    let mut execution = execution();
    execution.ready().await;
    let coordinator = super::super::wait_coordinator::WaitCoordinator::new(
        Arc::new(execution.clone()),
        crate::CooperativeModelWait::default(),
    );
    execution.wait_coordinator = Some(coordinator.clone());
    // No attached channel: the real VM adapter rejects before transmitting bytes.
    let error = execution.pause().await.unwrap_err();
    assert!(
        error
            .downcast_ref::<crate::executor::vm::control::ControlFailure>()
            .is_some()
    );
    assert!(!execution.cancellation().is_cancelled());
    let snapshot = execution.snapshot();
    assert_eq!(snapshot.status.state, RunState::Running);
    assert!(!snapshot.progress.outcome_unknown);
    assert_eq!(snapshot.progress.queued + snapshot.progress.in_flight, 0);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        coordinator.passive_delivery(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(execution.resume().await.is_err());
    assert!(!execution.cancellation().is_cancelled());
}

#[tokio::test]
async fn only_unknown_controls_or_unreleased_automatic_pauses_cancel_execution() {
    use crate::executor::vm::control::ControlFailure;
    for origin in [ControlOrigin::Manual, ControlOrigin::ModelWait] {
        for (known, state) in [
            (true, VmState::Running),
            (true, VmState::Paused),
            (false, VmState::Running),
        ] {
            let execution = execution();
            execution.ready().await;
            let id = execution
                .enqueue(OperationKind::RunPause, origin)
                .await
                .unwrap();
            execution.mark_dispatched(&id).await.unwrap();
            let error: anyhow::Error = if known {
                ControlFailure::Rejected {
                    state,
                    message: "known healthy rejection".into(),
                }
                .into()
            } else {
                anyhow::anyhow!("native acknowledgement lost")
            };
            execution.control_error(&id, &error, origin).await;
            assert_eq!(
                execution.cancellation().is_cancelled(),
                !known || (origin == ControlOrigin::ModelWait && state == VmState::Paused)
            );
            assert_eq!(execution.snapshot().progress.outcome_unknown, !known);
        }
    }
}

#[tokio::test]
async fn idle_failures_are_recorded_and_only_execution_chooses_cancellation() {
    let execution = execution();
    execution.ready().await;
    let error = crate::executor::vm::control::ControlFailure::BeforeDispatch(anyhow::anyhow!(
        "idle pause unavailable"
    ))
    .into();
    execution.wait_failed(&error).await;
    assert!(!execution.cancellation().is_cancelled());
    let before = execution.snapshot();
    execution
        .wait_failed(&anyhow::anyhow!("idle retirement acknowledgement lost"))
        .await;
    let after = execution.snapshot();
    assert!(execution.cancellation().is_cancelled());
    assert_eq!(after.status.state, RunState::Cancelling);
    assert_ne!(before.progress.applied, after.progress.applied);
    let history = records(&execution).await;
    assert_eq!(replay(&history).unwrap().status.state, RunState::Cancelling);
}
