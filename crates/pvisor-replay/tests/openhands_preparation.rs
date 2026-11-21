use proptest::prelude::*;
use pvisor_replay::api::{
    AgentKind, AgentStatus, PlaybackRequest, ReplayErrorKind, ReplayMode, ReplayPhase,
    ReplayRequest,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn request(root: &Path, after_step: usize) -> PlaybackRequest {
    PlaybackRequest {
        agent: AgentKind::Openhands,
        trajectory: root.join("events.json"),
        after_step,
        workspace: root.join("workspace"),
        state_dir: root.join("state"),
        output_dir: root.join("output"),
        agent_entrypoint: None,
        agent_runtime: None,
        disallowed_tools: vec![],
        trajectory_assets: None,
        session_id: None,
        max_steps: None,
        mode: ReplayMode::PrepareOnly,
        allow_stale_observations: false,
        run_id: Some("openhands-contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

fn events(steps: usize, action: &str, late: bool, payload: &str, is_error: bool) -> Vec<Value> {
    let mut events = vec![
        json!({"id": 0, "source": "user", "action": "message", "args": {"content": "prepare only"}}),
        json!({"id": 1, "source": "agent", "action": "system"}),
    ];
    let mut observations = Vec::new();
    for step in 0..steps {
        let id = 10 + 10 * step;
        events.push(json!({
            "id": id, "source": "agent", "action": action,
            "args": {
                "command": "touch must-not-run", "path": "/workspace/must-not-run",
                "code": "print('must not run')", "file_text": payload, "thought": payload
            }
        }));
        let observation = json!({
            "id": id + 1, "source": "environment",
            "observation": if is_error { "error" } else { action },
            "cause": id, "message": payload, "args": {"metadata": {"exit_code": 0}}
        });
        if late {
            observations.push(observation);
        } else {
            events.push(observation);
        }
    }
    events.extend(observations);
    events.push(json!({"id": 10 * steps + 3, "source": "agent", "action": "finish"}));
    events
}

fn assert_prepared(
    steps: usize,
    after_step: usize,
    action: &str,
    late: bool,
    payload: &str,
    is_error: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), after_step);
    fs::create_dir(&request.workspace).unwrap();
    let events = events(steps, action, late, payload, is_error);
    let raw = serde_json::to_vec(&events).unwrap();
    fs::write(&request.trajectory, &raw).unwrap();
    let report = request.clone().execute().unwrap();
    assert_eq!(report.exit_code, 0, "{:?}", report.result);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 0);
    let output = request.output_dir.join("openhands-contract");
    let actual: Vec<Value> = serde_json::from_slice(
        &fs::read(output.join("native/prepared-replay-events.json")).unwrap(),
    )
    .unwrap();
    let selected: Vec<usize> = (0..after_step).map(|step| 10 + 10 * step).collect();
    let mut ids = vec![0];
    if late {
        ids.extend(selected.iter().copied());
        ids.extend(selected.iter().map(|id| id + 1));
    } else {
        ids.extend(selected.iter().flat_map(|id| [*id, id + 1]));
    }
    assert_eq!(
        actual
            .iter()
            .map(|event| event["id"].as_u64().unwrap() as usize)
            .collect::<Vec<_>>(),
        ids
    );
    let function = match action {
        "run" => "execute_bash",
        "read" | "edit" => "str_replace_editor",
        "run_ipython" => "execute_ipython_cell",
        "think" => "think",
        _ => unreachable!(),
    };
    for mut event in actual {
        if event["source"] == "agent" {
            let metadata = event
                .as_object_mut()
                .unwrap()
                .remove("tool_call_metadata")
                .unwrap();
            assert_eq!(metadata["function_name"], function);
        }
        let source = events
            .iter()
            .find(|source| source["id"] == event["id"])
            .unwrap();
        assert_eq!(&event, source);
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["boundary"]["prefix_model_turns"], after_step);
    let batches = manifest["batches"].as_array().unwrap();
    assert_eq!(batches.len(), after_step);
    for (step, batch) in batches.iter().enumerate() {
        assert_eq!(
            batch["tool_calls"][0]["call_id"],
            (10 + 10 * step).to_string()
        );
        assert_eq!(batch["tool_calls"][0]["name"], action);
    }
    assert_eq!(fs::read(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
}

#[rstest::rstest]
#[case::first("run", 3, 1, true, false)]
#[case::middle("read", 3, 2, false, false)]
#[case::last("edit", 3, 3, true, true)]
#[case::ipython("run_ipython", 1, 1, false, false)]
#[case::think("think", 2, 2, true, false)]
fn preparation_preserves_selected_actions_and_causally_late_observations(
    #[case] action: &str,
    #[case] steps: usize,
    #[case] after_step: usize,
    #[case] late: bool,
    #[case] is_error: bool,
) {
    assert_prepared(
        steps,
        after_step,
        action,
        late,
        "内容\n\"quoted\"\0",
        is_error,
    );
}

proptest! {
    #[test]
    fn generated_preparation_keeps_observations_and_never_executes_actions(
        steps in 1usize..5,
        selection in 0usize..4,
        action in prop::sample::select(vec!["run", "read", "edit", "run_ipython", "think"]),
        late in any::<bool>(),
        is_error in any::<bool>(),
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        assert_prepared(steps, 1 + selection % steps, action, late, &payload, is_error);
    }
}

#[rstest::rstest]
#[case::missing_id("missing-id", ReplayErrorKind::Trajectory)]
#[case::fractional_id("fractional-id", ReplayErrorKind::Trajectory)]
#[case::overflow_id("overflow-id", ReplayErrorKind::Trajectory)]
#[case::duplicate_id("duplicate-id", ReplayErrorKind::Trajectory)]
#[case::unsupported_action("unsupported-action", ReplayErrorKind::UnsupportedVersion)]
#[case::missing_user("missing-user", ReplayErrorKind::Trajectory)]
#[case::missing_observation("missing-observation", ReplayErrorKind::Trajectory)]
#[case::non_array("non-array", ReplayErrorKind::Trajectory)]
#[case::zero_boundary("zero-boundary", ReplayErrorKind::Configuration)]
#[case::excessive_boundary("excessive-boundary", ReplayErrorKind::Trajectory)]
fn invalid_sources_are_rejected_without_publication_and_can_be_explicitly_repaired(
    #[case] fault: &str,
    #[case] kind: ReplayErrorKind,
) {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), 1);
    fs::create_dir(&request.workspace).unwrap();
    let valid = events(1, "run", false, "original", false);
    let mut invalid = Value::Array(valid.clone());
    match fault {
        "missing-id" => {
            invalid[2].as_object_mut().unwrap().remove("id");
        }
        "fractional-id" => invalid[2]["id"] = json!(1.5),
        "overflow-id" => invalid[2]["id"] = json!(u64::MAX),
        "duplicate-id" => invalid[2]["id"] = json!(0),
        "unsupported-action" => invalid[2]["action"] = json!("browse"),
        "missing-user" => invalid[0]["source"] = json!("environment"),
        "missing-observation" => {
            invalid.as_array_mut().unwrap().remove(3);
        }
        "non-array" => invalid = json!({}),
        "zero-boundary" => request.after_step = 0,
        "excessive-boundary" => request.after_step = usize::MAX,
        _ => unreachable!(),
    }
    let raw = serde_json::to_vec(&invalid).unwrap();
    fs::write(&request.trajectory, &raw).unwrap();
    assert_eq!(request.clone().execute().err().unwrap().kind, kind);
    assert!(!request.state_dir.exists());
    assert!(!request.output_dir.exists());
    assert_eq!(fs::read(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
    request.after_step = 1;
    fs::write(&request.trajectory, serde_json::to_vec(&valid).unwrap()).unwrap();
    let report = request.execute().unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
}
