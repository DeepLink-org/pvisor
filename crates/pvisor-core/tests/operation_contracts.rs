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
    let OperationKind::RunExecute { args, .. } = &mut after.kind;
    args.push("--adapted".into());
    event(Fact::Rewritten {
        before: before.clone(),
        after: after.clone(),
    })
    .validate()
    .unwrap();
    after.run_id = "different".into();
    assert!(
        event(Fact::Rewritten {
            before: before.clone(),
            after: after.clone()
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
            after: after.clone()
        })
        .validate()
        .is_err()
    );
    event(Fact::Placed { operation: after }).validate().unwrap();
}
