use proptest::prelude::*;
use pvisor_core::{
    CapabilityDimension, EnforcementPlan, RunState,
    event::{Event, Fact, Granularity, Level, Origin, VERSION},
    operation::{
        Context, OPERATION_VERSION, Operation, OperationDecision, OperationKind,
        OperationObservation, Outcome, Placement, Value,
    },
};
use std::collections::BTreeMap;

fn plan() -> Operation {
    Operation {
        version: OPERATION_VERSION,
        run_id: "run-test".into(),
        kind: OperationKind::RunExecute {
            program: "/bin/true".into(),
            args: vec![],
            cwd: None,
        },
        context: Context {
            revision: 0,
            principal: "agent".into(),
            scope: vec!["run".into()],
            policy: "enforce".into(),
            bindings: BTreeMap::new(),
        },
        placements: vec![],
        rules: vec![OperationDecision {
            id: "run.execute".into(),
            dimension: CapabilityDimension::Subprocess,
            target: "run-test".into(),
            action: "execute".into(),
            enforcement_plan: EnforcementPlan::default(),
        }],
    }
}

#[test]
fn execution_checkpoint_is_bound_to_its_run_encoding_and_capture_operation() {
    use pvisor_core::operation::{ExecutionCheckpoint, SnapshotRamStorage};
    let mut operation = plan();
    operation.rules.clear();
    operation.kind = OperationKind::RunCheckpoint {
        request_id: "save-1".into(),
        ram_storage: SnapshotRamStorage::Compressed,
    };
    operation.validate().unwrap();
    let checkpoint = ExecutionCheckpoint {
        snapshot_id: "a".repeat(64),
        store: "/private/snapshots".into(),
        source_run_id: operation.run_id.clone(),
        source_attempt_id: "attempt-1".into(),
        created_at_unix_ms: 1,
        ram_storage: SnapshotRamStorage::Compressed,
    };
    let observation = |checkpoint| OperationObservation {
        outcome: Outcome::success(Value::ExecutionCheckpoint { checkpoint }),
        rules: Default::default(),
        filesystem: None,
    };
    observation(checkpoint.clone())
        .validate(&operation)
        .unwrap();
    let mut suspend = operation.clone();
    suspend.kind = OperationKind::RunSuspend {
        request_id: "suspend-1".into(),
        ram_storage: SnapshotRamStorage::Compressed,
    };
    let encoded = serde_json::to_string(&suspend.kind).unwrap();
    assert_eq!(
        serde_json::from_str::<OperationKind>(&encoded).unwrap(),
        suspend.kind
    );
    observation(checkpoint.clone()).validate(&suspend).unwrap();
    assert!(
        serde_json::from_str::<OperationKind>(
            r#"{"op":"run.suspend","request_id":"one","ram_storage":"raw","extra":true}"#
        )
        .is_err()
    );
    for corruption in ["run", "encoding", "relative", "identity", "attempt"] {
        let mut bad = checkpoint.clone();
        match corruption {
            "run" => bad.source_run_id = "other".into(),
            "encoding" => bad.ram_storage = SnapshotRamStorage::Raw,
            "relative" => bad.store = "relative".into(),
            "identity" => bad.snapshot_id = "g".repeat(64),
            "attempt" => bad.source_attempt_id.clear(),
            _ => unreachable!(),
        }
        assert!(
            observation(bad).validate(&operation).is_err(),
            "{corruption}"
        );
    }
    operation.kind = OperationKind::RunPause;
    assert!(observation(checkpoint).validate(&operation).is_err());
    for request_id in [" ".to_owned(), "x".repeat(257)] {
        assert!(
            OperationKind::RunCheckpoint {
                request_id,
                ram_storage: SnapshotRamStorage::Raw
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        serde_json::from_str::<OperationKind>(
            r#"{"op":"run.checkpoint","request_id":"one","ram_storage":"raw","extra":true}"#
        )
        .is_err()
    );
}

fn event(data: Fact) -> Event {
    Event {
        version: VERSION,
        id: "fact".into(),
        trace_id: "run-test".into(),
        producer: "test".into(),
        observed_at_unix_ms: 0,
        scope: vec!["run".into()],
        context: Some("context".into()),
        operation: Some("operation".into()),
        caused_by: vec![],
        level: Level::Info,
        granularity: Granularity::Operation,
        data,
    }
}

#[test]
fn removed_language_and_old_schemas_are_rejected() {
    let mut value = serde_json::to_value(plan()).unwrap();
    value["version"] = serde_json::json!(2);
    assert!(
        serde_json::from_value::<Operation>(value.clone())
            .unwrap()
            .validate()
            .is_err()
    );
    value["version"] = serde_json::json!(OPERATION_VERSION);
    value["rewrites"] = serde_json::json!([]);
    assert!(serde_json::from_value::<Operation>(value).is_err());
    assert!(serde_json::from_str::<Fact>(r#"{"fact":"rewritten"}"#).is_err());
    assert!(serde_json::from_str::<Placement>(r#"{"placement":"mock","value":0}"#).is_err());
    assert!(serde_json::from_str::<Placement>(r#"{"placement":"deny","reason":"test"}"#).is_err());
    assert!(serde_json::from_str::<Value>(r#"{"bytes":[1]}"#).is_err());
}

#[test]
fn events_retain_operation_dispatch_and_terminal_result_contracts() {
    let plan = plan();
    for fact in [
        Fact::Requested {
            operation: plan.clone(),
        },
        Fact::Dispatched {
            backend: "process".into(),
            run_id: plan.run_id.clone(),
        },
        Fact::Completed {
            run_id: plan.run_id.clone(),
            outcome: Outcome::success(Value::Run {
                state: RunState::Completed,
                exit_code: Some(0),
            }),
            origin: Origin::Backend,
        },
    ] {
        let mut fact = event(fact);
        fact.validate().unwrap();
        assert_eq!(fact.data.domain(), "run");
        assert_eq!(
            serde_json::from_slice::<Event>(&serde_json::to_vec(&fact).unwrap()).unwrap(),
            fact
        );
        fact.to_text().unwrap();
        fact.version = VERSION - 1;
        assert!(fact.validate().is_err());
        fact.version = VERSION;
        fact.context = None;
        assert!(fact.validate().is_err());
    }
    let mut observation = OperationObservation {
        outcome: Outcome::success(Value::Run {
            state: RunState::Running,
            exit_code: None,
        }),
        rules: BTreeMap::from([("run.execute".into(), Default::default())]),
        filesystem: None,
    };
    assert!(observation.validate(&plan).is_err());
    observation.outcome = Outcome::success(Value::Run {
        state: RunState::Completed,
        exit_code: Some(0),
    });
    observation.validate(&plan).unwrap();
    observation.rules.clear();
    assert!(observation.validate(&plan).is_err());
}

proptest! {
    #[test]
    fn operation_json_preserves_identity_and_placement_order(run_id in ".{1,100}", names in prop::collection::vec("[a-z]{1,8}", 0..16)) {
        let mut plan = plan();
        plan.run_id = run_id;
        plan.placements = names.into_iter().enumerate().map(|(index, name)|
            if index % 2 == 0 { Placement::Vm { name } } else { Placement::Overlay { name } }
        ).collect();
        plan.validate().unwrap();
        let decoded: Operation = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        decoded.validate().unwrap();
        prop_assert_eq!(decoded, plan);
    }
}

#[test]
fn rewrite_preserves_identity_and_placement_is_a_separate_fact() {
    let before = plan();
    let mut after = before.clone();
    let OperationKind::RunExecute { args, .. } = &mut after.kind else {
        panic!("expected run.execute")
    };
    args.push("--adapted".into());
    event(Fact::Rewritten {
        before: before.clone(),
        after: Box::new(after.clone()),
    })
    .validate()
    .unwrap();
    after.run_id = "different".into();
    assert!(
        event(Fact::Rewritten {
            before: before.clone(),
            after: Box::new(after.clone())
        })
        .validate()
        .is_err()
    );
    after.run_id = before.run_id.clone();
    after.placements.push(Placement::Overlay {
        name: "stage".into(),
    });
    assert!(
        event(Fact::Rewritten {
            before,
            after: Box::new(after.clone())
        })
        .validate()
        .is_err()
    );
    event(Fact::Placed { operation: after }).validate().unwrap();
}

#[test]
fn vm_control_primitives_round_trip_and_require_matching_results() {
    use pvisor_core::operation::{VmMemory, VmState};
    for (kind, state) in [
        (OperationKind::RunPause, VmState::Paused),
        (OperationKind::RunResume, VmState::Running),
    ] {
        let mut operation = plan();
        operation.kind = kind;
        operation.rules.clear();
        operation.validate().unwrap();
        let decoded: Operation =
            serde_json::from_slice(&serde_json::to_vec(&operation).unwrap()).unwrap();
        assert_eq!(operation, decoded);
        let mut observation = OperationObservation {
            outcome: Outcome::success(Value::Vm {
                state,
                memory: None,
            }),
            rules: Default::default(),
            filesystem: None,
        };
        observation.validate(&operation).unwrap();
        observation.outcome = Outcome::success(Value::Vm {
            state: VmState::Offloaded,
            memory: None,
        });
        assert!(observation.validate(&operation).is_err());
    }
    let mut operation = plan();
    operation.kind = OperationKind::RunOffload {
        file: Some("/data/guest.ram".into()),
    };
    operation.rules.clear();
    let observation = OperationObservation {
        outcome: Outcome::success(Value::Vm {
            state: VmState::Offloaded,
            memory: Some(VmMemory {
                backing_file: "/data/guest.ram".into(),
                backed_bytes: 4096,
                resident_before_bytes: Some(4096),
                resident_after_bytes: Some(0),
            }),
        }),
        rules: Default::default(),
        filesystem: None,
    };
    observation.validate(&operation).unwrap();
    let decoded: Operation =
        serde_json::from_slice(&serde_json::to_vec(&operation).unwrap()).unwrap();
    assert_eq!(operation, decoded);
}

#[test]
fn vm_control_contract_rejects_bad_paths_unknown_fields_and_mismatched_states() {
    use pvisor_core::operation::{VmMemory, VmState};
    for json in [
        r#"{"op":"run.pause","file":"/ram"}"#,
        r#"{"op":"run.resume","extra":true}"#,
        r#"{"op":"run.offload","file":42}"#,
    ] {
        assert!(
            serde_json::from_str::<OperationKind>(json).is_err(),
            "{json}"
        );
    }
    for json in [
        r#"{"op":"run.offload"}"#,
        r#"{"op":"run.offload","file":null}"#,
    ] {
        assert_eq!(
            serde_json::from_str::<OperationKind>(json).unwrap(),
            OperationKind::RunOffload { file: None }
        );
    }
    for path in ["".to_owned(), "/".to_owned(), "x".repeat(4097)] {
        assert!(
            OperationKind::RunOffload {
                file: Some(path.into())
            }
            .validate()
            .is_err()
        );
    }
    let mut operation = plan();
    operation.kind = OperationKind::RunPause;
    operation.rules.clear();
    let mut observation = OperationObservation {
        outcome: Outcome::success(Value::Vm {
            state: VmState::Running,
            memory: None,
        }),
        rules: Default::default(),
        filesystem: None,
    };
    assert!(observation.validate(&operation).is_err());
    operation.kind = OperationKind::RunOffload { file: None };
    for (path, bytes) in [("relative.ram", 4096), ("/ram", 0)] {
        observation.outcome = Outcome::success(Value::Vm {
            state: VmState::Offloaded,
            memory: Some(VmMemory {
                backing_file: path.into(),
                backed_bytes: bytes,
                resident_before_bytes: None,
                resident_after_bytes: None,
            }),
        });
        assert!(observation.validate(&operation).is_err());
    }
}
