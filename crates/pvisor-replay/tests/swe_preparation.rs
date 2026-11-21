use proptest::prelude::*;
use pvisor_replay::api::{
    AgentKind, AgentStatus, PlaybackRequest, ReplayErrorKind, ReplayMode, ReplayPhase,
    ReplayRequest,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn request(root: &Path, selected: usize) -> PlaybackRequest {
    PlaybackRequest {
        agent: AgentKind::SweAgent,
        trajectory: root.join("source.traj"),
        after_step: selected,
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
        run_id: Some("swe-contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

fn source(steps: usize, call_shape: usize, config: Value, payload: &str) -> Value {
    let mut history = vec![
        json!({"role": "system", "content": "system"}),
        json!({"role": "user", "content": "task"}),
    ];
    let mut trajectory = Vec::new();
    for step in 0..steps {
        trajectory.push(json!({
            "action": format!("printf {payload}"), "observation": payload,
            "thought": payload, "state": {"step": step}
        }));
        let mut assistant = json!({"role": "assistant", "content": payload});
        match call_shape {
            0 => {}
            1 => assistant["tool_calls"] = json!([{"id": ""}]),
            2 => assistant["tool_calls"] = json!([{"id": format!("native-{step}")}]),
            3 => assistant["tool_calls"] = json!([{"id": "one"}, {"id": "two"}]),
            4 => assistant["tool_calls"] = json!([{"id": null}]),
            _ => unreachable!(),
        }
        history.extend([
            json!({"role": "user", "content": "before action"}),
            assistant,
            json!({"role": "tool", "content": payload}),
            json!({"role": "user", "content": "after result"}),
        ]);
    }
    json!({"trajectory": trajectory, "history": history, "replay_config": config})
}

fn assert_prepared(
    steps: usize,
    selected: usize,
    call_shape: usize,
    encoded: bool,
    asset: usize,
    payload: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), selected);
    fs::create_dir(&request.workspace).unwrap();
    let mut config = json!({"problem_statement": {
        "type": "text", "text": payload, "id": {"opaque": 7}
    }, "retained": payload});
    let mut expected_config = config.clone();
    if asset > 0 {
        let assets = root.path().join("assets");
        fs::create_dir(&assets).unwrap();
        fs::write(assets.join("task.md"), payload).unwrap();
        request.trajectory_assets = Some(assets);
        config["problem_statement"] = if asset == 1 {
            json!({"type": "file", "path": "task.md"})
        } else {
            json!({"type": "path", "file": "task.md", "id": {"opaque": 7}})
        };
        if asset == 1 {
            expected_config["problem_statement"]["id"] = json!("replay");
        }
    }
    let config = if encoded {
        json!(config.to_string())
    } else {
        config
    };
    let source = source(steps, call_shape, config, payload);
    let raw = serde_json::to_vec(&source).unwrap();
    fs::write(&request.trajectory, &raw).unwrap();
    let report = request.clone().execute().unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 0);
    let output = request.output_dir.join("swe-contract");
    let prepared: Value =
        serde_json::from_slice(&fs::read(output.join("native/prepared-prefix.traj")).unwrap())
            .unwrap();
    let mut expected = source.clone();
    expected["trajectory"] = json!(&source["trajectory"].as_array().unwrap()[..selected]);
    expected["history"] = json!(&source["history"].as_array().unwrap()[..4 * selected]);
    expected["replay_config"] = expected_config;
    assert_eq!(prepared, expected);
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["boundary"]["prefix_model_turns"], selected);
    let batches = manifest["batches"].as_array().unwrap();
    assert_eq!(batches.len(), selected);
    for (step, batch) in batches.iter().enumerate() {
        let calls = batch["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        let id = match call_shape {
            1 => String::new(),
            2 => format!("native-{step}"),
            _ => format!("swe-agent-step-{}", step + 1),
        };
        assert_eq!(calls[0]["call_id"], id);
        assert_eq!(calls[0]["name"], "swe_agent_action");
        assert_eq!(
            calls[0]["arguments"],
            json!({"raw_action": format!("printf {payload}")})
        );
    }
    assert_eq!(fs::read(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
    if let Some(assets) = request.trajectory_assets {
        assert_eq!(
            fs::read(assets.join("task.md")).unwrap(),
            payload.as_bytes()
        );
    }
}

#[rstest::rstest]
#[case::first(1, 0, false, 0)]
#[case::middle_empty_id(2, 1, true, 0)]
#[case::last_native_id(3, 2, false, 1)]
#[case::multiple_calls(2, 3, true, 2)]
#[case::null_id(1, 4, true, 1)]
fn exact_prefixes_preserve_history_config_and_call_ids(
    #[case] selected: usize,
    #[case] call_shape: usize,
    #[case] encoded: bool,
    #[case] asset: usize,
) {
    assert_prepared(
        3,
        selected,
        call_shape,
        encoded,
        asset,
        "内容\n\"quoted\"\0",
    );
}

proptest! {
    #[test]
    fn generated_preparation_preserves_prefixes_and_never_executes_actions(
        steps in 1usize..5,
        selection in 0usize..4,
        call_shape in 0usize..5,
        encoded in any::<bool>(),
        asset in 0usize..3,
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        assert_prepared(steps, 1 + selection % steps, call_shape, encoded, asset, &payload);
    }
}

#[rstest::rstest]
#[case::encoded_json("encoded-json", ReplayErrorKind::Trajectory)]
#[case::trajectory("trajectory", ReplayErrorKind::Trajectory)]
#[case::history("history", ReplayErrorKind::Trajectory)]
#[case::action("action", ReplayErrorKind::Trajectory)]
#[case::observation("observation", ReplayErrorKind::Trajectory)]
#[case::submit("submit", ReplayErrorKind::UnsupportedVersion)]
#[case::multiline_submit("multiline-submit", ReplayErrorKind::UnsupportedVersion)]
#[case::missing_assets("missing-assets", ReplayErrorKind::Trajectory)]
#[case::traversal("traversal", ReplayErrorKind::Configuration)]
#[case::absolute("absolute", ReplayErrorKind::Configuration)]
#[case::empty_path("empty-path", ReplayErrorKind::Configuration)]
#[case::invalid_utf8("invalid-utf8", ReplayErrorKind::Trajectory)]
fn invalid_sources_publish_no_run_state_and_allow_explicit_repair(
    #[case] fault: &str,
    #[case] kind: ReplayErrorKind,
) {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), 1);
    fs::create_dir(&request.workspace).unwrap();
    let assets = root.path().join("assets");
    fs::create_dir(&assets).unwrap();
    fs::write(assets.join("task.md"), b"task").unwrap();
    request.trajectory_assets = Some(assets.clone());
    let valid = source(
        1,
        0,
        json!({"problem_statement": {"type": "text", "text": "task"}}),
        "task",
    );
    let mut invalid = valid.clone();
    match fault {
        "encoded-json" => invalid["replay_config"] = json!("{"),
        "trajectory" => invalid["trajectory"] = Value::Null,
        "history" => invalid["history"] = json!([{"role": "user"}]),
        "action" => invalid["trajectory"][0]["action"] = Value::Null,
        "observation" => invalid["trajectory"][0]["observation"] = Value::Null,
        "submit" => invalid["trajectory"][0]["action"] = json!("  submit  "),
        "multiline-submit" => invalid["trajectory"][0]["action"] = json!("  submit\nresult"),
        _ => {
            let path = match fault {
                "traversal" => "../outside.md".to_owned(),
                "absolute" => assets.join("task.md").to_str().unwrap().to_owned(),
                "empty-path" => String::new(),
                _ => "task.md".into(),
            };
            invalid["replay_config"]["problem_statement"] = json!({"type": "file", "path": path});
            if fault == "missing-assets" {
                request.trajectory_assets = None;
            }
            if fault == "invalid-utf8" {
                fs::write(assets.join("task.md"), [0xff]).unwrap();
            }
        }
    }
    let raw = serde_json::to_vec(&invalid).unwrap();
    fs::write(&request.trajectory, &raw).unwrap();
    assert_eq!(request.clone().execute().err().unwrap().kind, kind);
    assert!(!request.state_dir.exists());
    assert!(!request.output_dir.exists());
    assert_eq!(fs::read(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
    fs::write(&request.trajectory, serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(
        request.execute().unwrap().result.phase,
        ReplayPhase::Prepared
    );
}

#[cfg(unix)]
#[rstest::rstest]
fn asset_symlinks_cannot_escape_the_declared_root() {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), 1);
    fs::create_dir(&request.workspace).unwrap();
    let assets = root.path().join("assets");
    fs::create_dir(&assets).unwrap();
    let outside = root.path().join("outside.md");
    fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, assets.join("task.md")).unwrap();
    request.trajectory_assets = Some(assets);
    let source = source(
        1,
        0,
        json!({"problem_statement": {"type": "path", "file": "task.md"}}),
        "task",
    );
    let raw = serde_json::to_vec(&source).unwrap();
    fs::write(&request.trajectory, &raw).unwrap();
    assert_eq!(
        request.clone().execute().err().unwrap().kind,
        ReplayErrorKind::Trajectory
    );
    assert!(!request.state_dir.exists());
    assert!(!request.output_dir.exists());
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert_eq!(fs::read(&request.trajectory).unwrap(), raw);
}
