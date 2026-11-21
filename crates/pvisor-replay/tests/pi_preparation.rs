use proptest::prelude::*;
use pvisor_replay::api::{
    AgentKind, AgentStatus, PlaybackRequest, ReplayErrorKind, ReplayMode, ReplayPhase,
    ReplayRequest,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn request(root: &Path, after_step: usize) -> PlaybackRequest {
    PlaybackRequest {
        agent: AgentKind::PiAgent,
        trajectory: root.join("events.jsonl"),
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
        run_id: Some("pi-contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

fn events(steps: usize, tool: &str, payload: &str, reversed: bool) -> Vec<Value> {
    let mut events = vec![json!({
        "type": "message_end",
        "message": {"role": "user", "content": [{"type": "text", "text": "prepare only"}]}
    })];
    for step in 0..steps {
        events.push(json!({
            "type": "turn_end",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "no tools"}]}
        }));
        let id = format!("call-{step}");
        let mut results = vec![
            json!({"toolCallId": "", "toolName": tool, "content": "first", "isError": false}),
            json!({"toolCallId": id, "toolName": tool, "content": "second", "isError": true}),
        ];
        if reversed {
            results.reverse();
        }
        events.push(json!({
            "type": "turn_end",
            "message": {"role": "assistant", "content": [
                {
                    "type": "toolCall", "id": "", "name": tool,
                    "arguments": {
                        "path": "must-not-run", "command": "touch must-not-run",
                        "content": payload, "nested": {"step": step}
                    }
                },
                {"type": "toolCall", "id": id, "name": tool}
            ]},
            "toolResults": results
        }));
    }
    events.push(json!({
        "type": "turn_end",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "next action"}]}
    }));
    events
}

fn render(events: &[Value], truncated_tail: bool) -> String {
    let mut raw = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n";
    if truncated_tail {
        raw.push_str("{\"type\":");
    }
    raw
}

fn assert_prepared(
    steps: usize,
    after_step: usize,
    tool: &str,
    reversed: bool,
    tail: bool,
    payload: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), after_step);
    fs::create_dir(&request.workspace).unwrap();
    let events = events(steps, tool, payload, reversed);
    let raw = render(&events, tail);
    fs::write(&request.trajectory, &raw).unwrap();
    let report = request.clone().execute().unwrap();
    assert_eq!(report.exit_code, 0, "{:?}", report.result);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 0);
    let output = request.output_dir.join("pi-contract");
    let prefix = fs::read_to_string(output.join("native/prepared-prefix.jsonl")).unwrap();
    assert!(prefix.ends_with('\n'));
    let actual: Vec<Value> = prefix
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(actual, events[..1 + 2 * after_step]);
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["boundary"]["after_step"], after_step);
    assert_eq!(manifest["boundary"]["prefix_model_turns"], 2 * after_step);
    let batches = manifest["batches"].as_array().unwrap();
    assert_eq!(batches.len(), after_step);
    for (step, batch) in batches.iter().enumerate() {
        assert_eq!(batch["ordinal"], step + 1);
        assert_eq!(batch["native_locator"], format!("events:{}", 2 + 2 * step));
        let calls = batch["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["call_id"], "");
        assert_eq!(calls[0]["name"], tool);
        assert_eq!(
            calls[0]["arguments"],
            events[2 + 2 * step]["message"]["content"][0]["arguments"]
        );
        assert_eq!(calls[1]["call_id"], format!("call-{step}"));
        assert_eq!(calls[1]["name"], tool);
        assert_eq!(calls[1]["arguments"], json!({}));
    }
    assert_eq!(fs::read_to_string(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
}

#[rstest::rstest]
#[case::first(3, 1, "read", false, false)]
#[case::middle(3, 2, "bash", true, false)]
#[case::last(3, 3, "edit", false, false)]
#[case::single(1, 1, "write", true, false)]
#[case::truncated_last_line(2, 2, "read", true, true)]
fn preparation_preserves_exact_logical_prefix_and_never_executes_tools(
    #[case] steps: usize,
    #[case] after_step: usize,
    #[case] tool: &str,
    #[case] reversed: bool,
    #[case] tail: bool,
) {
    assert_prepared(
        steps,
        after_step,
        tool,
        reversed,
        tail,
        "内容\n\"quoted\"\0",
    );
}

proptest! {
    #[test]
    fn generated_preparation_preserves_boundaries_and_native_arguments(
        steps in 1usize..5,
        selection in 0usize..4,
        tool in prop::sample::select(vec!["read", "bash", "edit", "write"]),
        reversed in any::<bool>(),
        tail in any::<bool>(),
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        assert_prepared(steps, 1 + selection % steps, tool, reversed, tail, &payload);
    }
}

#[rstest::rstest]
#[case::earlier_complete_prefix(true, 1, true)]
#[case::incomplete_boundary(true, 2, false)]
#[case::incomplete_nonfinal_batch(false, 1, false)]
fn incomplete_tool_batches_are_never_selected_as_complete_boundaries(
    #[case] final_batch: bool,
    #[case] after_step: usize,
    #[case] accepted: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), after_step);
    fs::create_dir(&request.workspace).unwrap();
    let mut events = events(2, "read", "original", false);
    if final_batch {
        events.truncate(5);
    }
    events[4]["toolResults"].as_array_mut().unwrap().pop();
    let raw = render(&events, false);
    fs::write(&request.trajectory, &raw).unwrap();
    let result = request.clone().execute();
    if accepted {
        let report = result.unwrap();
        assert_eq!(report.exit_code, 0);
        let path = request
            .output_dir
            .join("pi-contract/native/prepared-prefix.jsonl");
        let actual: Vec<Value> = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(actual, events[..3]);
    } else {
        assert_eq!(result.err().unwrap().kind, ReplayErrorKind::Trajectory);
        assert!(!request.state_dir.exists());
        assert!(!request.output_dir.exists());
    }
    assert_eq!(fs::read_to_string(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
}

#[rstest::rstest]
#[case::missing_id("missing-id", ReplayErrorKind::Trajectory)]
#[case::invalid_name("invalid-name", ReplayErrorKind::Trajectory)]
#[case::invalid_arguments("invalid-arguments", ReplayErrorKind::Trajectory)]
#[case::unsupported_tool("unsupported-tool", ReplayErrorKind::UnsupportedVersion)]
#[case::duplicate_calls("duplicate-calls", ReplayErrorKind::Trajectory)]
#[case::duplicate_results("duplicate-results", ReplayErrorKind::Trajectory)]
#[case::unmatched_result("unmatched-result", ReplayErrorKind::Trajectory)]
#[case::zero_boundary("zero-boundary", ReplayErrorKind::Configuration)]
#[case::excessive_boundary("excessive-boundary", ReplayErrorKind::Trajectory)]
fn invalid_trajectories_publish_no_state_and_allow_retry_after_explicit_repair(
    #[case] fault: &str,
    #[case] kind: ReplayErrorKind,
) {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), 1);
    fs::create_dir(&request.workspace).unwrap();
    let valid = events(1, "read", "original", false);
    let mut invalid = valid.clone();
    match fault {
        "missing-id" => {
            invalid[2]["message"]["content"][0]
                .as_object_mut()
                .unwrap()
                .remove("id");
        }
        "invalid-name" => invalid[2]["message"]["content"][0]["name"] = json!(false),
        "invalid-arguments" => invalid[2]["message"]["content"][0]["arguments"] = Value::Null,
        "unsupported-tool" => invalid[2]["message"]["content"][0]["name"] = json!("python"),
        "duplicate-calls" => invalid[2]["message"]["content"][1]["id"] = json!(""),
        "duplicate-results" => invalid[2]["toolResults"][1]["toolCallId"] = json!(""),
        "unmatched-result" => invalid[2]["toolResults"][0]["toolCallId"] = json!("unknown"),
        "zero-boundary" => request.after_step = 0,
        "excessive-boundary" => request.after_step = usize::MAX,
        _ => unreachable!(),
    }
    let raw = render(&invalid, false);
    fs::write(&request.trajectory, &raw).unwrap();
    let error = request.clone().execute().err().unwrap();
    assert_eq!(error.kind, kind);
    assert!(!request.state_dir.exists());
    assert!(!request.output_dir.exists());
    assert_eq!(fs::read_to_string(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
    request.after_step = 1;
    fs::write(&request.trajectory, render(&valid, false)).unwrap();
    let report = request.execute().unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
}
