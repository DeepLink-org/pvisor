use proptest::prelude::*;
use pvisor_control::{
    CapabilityDimension, EnforcementPlan, RunState,
    run_plan::{
        Context, Outcome, Placement, PlanRule, RUN_PLAN_VERSION, RunObservation, RunPlan, Value,
    },
    trace::{Event, Fact, Granularity, Level, Origin, VERSION},
};
use std::collections::BTreeMap;

fn plan() -> RunPlan {
    RunPlan {
        version: RUN_PLAN_VERSION,
        run_id: "run-test".into(),
        context: Context {
            revision: 0,
            principal: "agent".into(),
            scope: vec!["run".into()],
            policy: "enforce".into(),
            bindings: BTreeMap::new(),
        },
        placements: vec![],
        rules: vec![PlanRule {
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
        serde_json::from_value::<RunPlan>(value.clone())
            .unwrap()
            .validate()
            .is_err()
    );
    value["version"] = serde_json::json!(RUN_PLAN_VERSION);
    value["rewrites"] = serde_json::json!([]);
    assert!(serde_json::from_value::<RunPlan>(value).is_err());
    assert!(serde_json::from_str::<Fact>(r#"{"fact":"rewritten"}"#).is_err());
    assert!(serde_json::from_str::<Placement>(r#"{"placement":"mock","value":0}"#).is_err());
    assert!(serde_json::from_str::<Placement>(r#"{"placement":"deny","reason":"test"}"#).is_err());
    assert!(serde_json::from_str::<Value>(r#"{"bytes":[1]}"#).is_err());
}

#[test]
fn trace_retains_plan_dispatch_and_terminal_result_contracts() {
    let plan = plan();
    for fact in [
        Fact::Requested { plan: plan.clone() },
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
        fact.version = 3;
        assert!(fact.validate().is_err());
        fact.version = VERSION;
        fact.context = None;
        assert!(fact.validate().is_err());
    }
    let mut observation = RunObservation {
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
    fn plan_json_preserves_identity_and_placement_order(run_id in ".{1,100}", names in prop::collection::vec("[a-z]{1,8}", 0..16)) {
        let mut plan = plan();
        plan.run_id = run_id;
        plan.placements = names.into_iter().enumerate().map(|(index, name)|
            if index % 2 == 0 { Placement::Vm { name } } else { Placement::Overlay { name } }
        ).collect();
        plan.validate().unwrap();
        let decoded: RunPlan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        decoded.validate().unwrap();
        prop_assert_eq!(decoded, plan);
    }
}
