use proptest::prelude::*;
use pvisor_replay::api::{
    AgentKind, AgentStatus, PlaybackRequest, ReplayMode, ReplayPhase, ReplayRequest,
};
use serde_json::{Value, json};
use std::fs;

fn assert_prepared(steps: usize, selected: usize, actions: bool, string_args: bool, payload: &str) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let mut messages = vec![json!({"role": "user", "content": "prepare only"})];
    let command = format!("printf {payload}");
    for _ in 0..steps {
        messages
            .push(json!({"role": "assistant", "content": "thinking", "extra": {"response": {}}}));
        let calls: Vec<Value> = (0..2)
            .map(|index| {
                let arguments = json!({"command": command, "retained": payload});
                let arguments = if string_args {
                    json!(arguments.to_string())
                } else {
                    arguments
                };
                let mut call = json!({"function": {"name": "bash", "arguments": arguments}});
                if index == 0 {
                    call["id"] = json!("");
                }
                call
            })
            .collect();
        let mut message = json!({
            "role": "assistant", "content": "tools", "tool_calls": calls,
            "extra": {"response": {}}
        });
        if actions {
            message["extra"]["actions"] = json!([
                {"command": command, "tool_call_id": "", "retained": payload},
                {"command": command, "retained": payload}
            ]);
        }
        messages.push(message);
        messages.push(json!({"role": "tool", "content": "first", "extra": {"returncode": 0}}));
        messages.push(json!({"role": "tool", "content": "second", "extra": {"returncode": 1}}));
    }
    let source = json!({
        "trajectory_format": "mini-swe-agent-1.1",
        "info": {"mini_version": "2.4.6"}, "messages": messages
    });
    let trajectory = root.path().join("source.json");
    let raw = serde_json::to_vec(&source).unwrap();
    fs::write(&trajectory, &raw).unwrap();
    let output = root.path().join("output");
    let report = PlaybackRequest {
        agent: AgentKind::MiniSweAgent,
        trajectory: trajectory.clone(),
        after_step: selected,
        workspace: workspace.clone(),
        state_dir: root.path().join("state"),
        output_dir: output.clone(),
        agent_entrypoint: None,
        agent_runtime: None,
        disallowed_tools: vec![],
        trajectory_assets: None,
        session_id: None,
        max_steps: None,
        mode: ReplayMode::PrepareOnly,
        allow_stale_observations: false,
        run_id: Some("mini-contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
    .execute()
    .unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 0);
    let output = output.join("mini-contract");
    let prepared: Value =
        serde_json::from_slice(&fs::read(output.join("native/prepared-prefix.json")).unwrap())
            .unwrap();
    let mut expected = source.clone();
    expected["messages"] = json!(&messages[..1 + 4 * selected]);
    assert_eq!(prepared, expected);
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["boundary"]["prefix_model_turns"], 2 * selected);
    for (index, batch) in manifest["batches"].as_array().unwrap().iter().enumerate() {
        let calls = batch["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["call_id"], "");
        assert_eq!(calls[1]["call_id"], format!("mini-{}-2", 2 + 4 * index));
        for call in calls {
            assert_eq!(call["name"], "bash");
            assert_eq!(call["arguments"]["command"], command);
            if actions {
                assert_eq!(call["arguments"], json!({"command": command}));
            } else {
                assert_eq!(call["arguments"]["retained"], payload);
            }
        }
    }
    assert_eq!(fs::read(&trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(workspace).unwrap().count(), 0);
}

#[rstest::rstest]
#[case::native_first(true, false, 1)]
#[case::native_last(true, true, 3)]
#[case::tool_objects(false, false, 2)]
#[case::tool_strings(false, true, 3)]
fn fixed_multicall_prefixes_preserve_native_messages(
    #[case] actions: bool,
    #[case] string_args: bool,
    #[case] selected: usize,
) {
    assert_prepared(3, selected, actions, string_args, "内容\n\"quoted\"\0");
}

proptest! {
    #[test]
    fn generated_multicall_preparation_preserves_prefixes_and_executes_no_tools(
        steps in 1usize..5,
        selection in 0usize..4,
        actions in any::<bool>(),
        string_args in any::<bool>(),
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        assert_prepared(steps, 1 + selection % steps, actions, string_args, &payload);
    }
}
