use proptest::prelude::*;
use pvisor_replay::api::{
    AgentKind, AgentStatus, PlaybackRequest, ReplayErrorKind, ReplayMode, ReplayPhase,
    ReplayRequest,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn request(root: &Path, after_step: usize) -> PlaybackRequest {
    PlaybackRequest {
        agent: AgentKind::ClaudeCode,
        trajectory: root.join("source.jsonl"),
        after_step,
        workspace: root.join("workspace"),
        state_dir: root.join("state"),
        output_dir: root.join("output"),
        agent_entrypoint: None,
        agent_runtime: None,
        disallowed_tools: vec![],
        trajectory_assets: None,
        session_id: Some("router-session".into()),
        max_steps: None,
        mode: ReplayMode::PrepareOnly,
        allow_stale_observations: false,
        run_id: Some("claude-contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

fn event(kind: &str, uuid: &str, parent: Value, message: Value) -> Value {
    json!({
        "type": kind, "uuid": uuid, "parentUuid": parent, "message": message,
        "sessionId": "native-session", "version": "2.1.220"
    })
}

fn events(
    fragments: usize,
    calls: usize,
    streamed: bool,
    payload: &str,
    error: bool,
) -> Vec<Value> {
    let mut events = vec![event(
        "user",
        "root",
        Value::Null,
        json!({"role": "user", "content": "task"}),
    )];
    let mut parent = "root".to_owned();
    for fragment in 0..fragments {
        let assistant = format!("assistant-{fragment}");
        let result = format!("result-{fragment}");
        let mut content = vec![
            json!({"type": "text", "text": payload}),
            json!({"type": "thinking", "thinking": payload}),
        ];
        let mut results = Vec::new();
        for call in 0..calls {
            let id = format!("call-{fragment}-{call}");
            content.push(json!({
                "type": "tool_use", "id": id, "name": "Bash",
                "input": {"command": "touch must-not-run", "retained": payload}
            }));
            let mut block = json!({"type": "tool_result", "tool_use_id": id, "content": payload});
            if error {
                block["is_error"] = json!(true);
            }
            results.push(block);
        }
        let message_id = if streamed {
            "stream".into()
        } else {
            format!("message-{fragment}")
        };
        let stop = if streamed && fragment + 1 < fragments {
            Value::Null
        } else {
            json!("tool_use")
        };
        events.push(event(
            "assistant",
            &assistant,
            json!(parent),
            json!({
                "id": message_id, "role": "assistant", "stop_reason": stop, "content": content
            }),
        ));
        let mut result_event = event(
            "user",
            &result,
            json!(assistant),
            json!({"role": "user", "content": results}),
        );
        result_event["sourceToolAssistantUUID"] = json!(assistant);
        result_event["toolUseResult"] = json!({"summary": "must be removed"});
        events.push(result_event);
        parent = result;
    }
    events.push(event(
        "assistant",
        "next",
        json!(parent),
        json!({
            "id": "next-message", "role": "assistant", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "next"}]
        }),
    ));
    events
}

fn write_source(request: &PlaybackRequest, events: &[Value]) -> String {
    let raw = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&request.trajectory, &raw).unwrap();
    raw
}

fn assert_prefix(events: &[Value], selected: usize, retained_fragments: usize, calls: usize) {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), selected);
    fs::create_dir(&request.workspace).unwrap();
    let raw = write_source(&request, events);
    let report = request.clone().execute().unwrap();
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Prepared);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 0);
    assert_eq!(
        report.result.metadata["native_session_id"],
        "native-session"
    );
    let output = request.output_dir.join("claude-contract");
    let prepared = fs::read_to_string(output.join("native/prepared-prefix.jsonl")).unwrap();
    let actual = prepared
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let mut expected = events[..1 + 2 * retained_fragments].to_vec();
    for event in &mut expected {
        event.as_object_mut().unwrap().remove("toolUseResult");
    }
    assert_eq!(actual, expected);
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["boundary"]["prefix_model_turns"], selected);
    let batches = manifest["batches"].as_array().unwrap();
    assert_eq!(batches.len(), selected);
    let ids = batches
        .iter()
        .flat_map(|batch| batch["tool_calls"].as_array().unwrap())
        .map(|call| call["call_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let expected_ids = (0..retained_fragments)
        .flat_map(|fragment| (0..calls).map(move |call| format!("call-{fragment}-{call}")))
        .collect::<Vec<_>>();
    assert_eq!(ids, expected_ids);
    assert_eq!(fs::read_to_string(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
}

#[rstest::rstest]
#[case::single(1, 1, false, 1, false)]
#[case::streamed(3, 2, true, 1, false)]
#[case::independent_prefix(3, 2, false, 2, false)]
#[case::error_results(2, 2, true, 1, true)]
fn preparation_preserves_streamed_batches_and_native_owners(
    #[case] fragments: usize,
    #[case] calls: usize,
    #[case] streamed: bool,
    #[case] selected: usize,
    #[case] error: bool,
) {
    let source = events(fragments, calls, streamed, "内容\n\"quoted\"\0", error);
    assert_prefix(
        &source,
        selected,
        if streamed { fragments } else { selected },
        calls,
    );
}

#[rstest::rstest]
fn completed_response_does_not_merge_a_later_fragment_with_the_same_message_id() {
    let mut source = events(2, 1, true, "content", false);
    source[1]["message"]["stop_reason"] = json!("tool_use");
    assert_prefix(&source, 1, 1, 1);
}

proptest! {
    #[test]
    fn generated_preparation_preserves_exact_prefix_and_executes_no_tools(
        fragments in 1usize..4,
        calls in 1usize..4,
        streamed in any::<bool>(),
        selection in 0usize..3,
        error in any::<bool>(),
        payload in prop::collection::vec(any::<char>(), 0..33),
    ) {
        let payload: String = payload.into_iter().collect();
        let selected = if streamed { 1 } else { 1 + selection % fragments };
        let retained = if streamed { fragments } else { selected };
        assert_prefix(&events(fragments, calls, streamed, &payload, error), selected, retained, calls);
    }
}

#[rstest::rstest]
#[case::missing_session("missing-session", ReplayErrorKind::Trajectory)]
#[case::mixed_sessions("mixed-sessions", ReplayErrorKind::Trajectory)]
#[case::version_drift("version", ReplayErrorKind::UnsupportedVersion)]
#[case::missing_result("missing-result", ReplayErrorKind::Trajectory)]
#[case::result_owner("result-owner", ReplayErrorKind::Trajectory)]
#[case::duplicate_tool("duplicate-tool", ReplayErrorKind::Trajectory)]
#[case::duplicate_result("duplicate-result", ReplayErrorKind::Trajectory)]
#[case::dangling_parent("dangling-parent", ReplayErrorKind::Trajectory)]
#[case::cycle("cycle", ReplayErrorKind::Trajectory)]
#[case::no_active_result("no-active-result", ReplayErrorKind::Trajectory)]
fn invalid_trajectories_publish_nothing_and_allow_explicit_repair(
    #[case] fault: &str,
    #[case] kind: ReplayErrorKind,
) {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), 1);
    fs::create_dir(&request.workspace).unwrap();
    let valid = events(2, 1, true, "recorded", false);
    let mut invalid = valid.clone();
    match fault {
        "missing-session" => {
            for event in &mut invalid {
                event.as_object_mut().unwrap().remove("sessionId");
            }
        }
        "mixed-sessions" => invalid[2]["sessionId"] = json!("another-session"),
        "version" => invalid[2]["version"] = json!("2.1.219"),
        "missing-result" => invalid[2]["message"]["content"] = json!([]),
        "result-owner" => invalid[2]["sourceToolAssistantUUID"] = json!("another-owner"),
        "duplicate-tool" => invalid[3]["message"]["content"][2]["id"] = json!("call-0-0"),
        "duplicate-result" => {
            let block = invalid[2]["message"]["content"][0].clone();
            invalid[2]["message"]["content"]
                .as_array_mut()
                .unwrap()
                .push(block);
        }
        "dangling-parent" => invalid[5]["parentUuid"] = json!("missing"),
        "cycle" => invalid[5]["parentUuid"] = json!("next"),
        "no-active-result" => {
            invalid[3]["parentUuid"] = json!("assistant-0");
            invalid[5]["parentUuid"] = json!("assistant-1");
        }
        _ => unreachable!(),
    }
    let raw = write_source(&request, &invalid);
    let error = request.clone().execute().unwrap_err();
    assert_eq!(error.kind, kind);
    assert!(!request.state_dir.exists());
    assert!(!request.output_dir.exists());
    assert_eq!(fs::read_to_string(&request.trajectory).unwrap(), raw);
    assert_eq!(fs::read_dir(&request.workspace).unwrap().count(), 0);
    write_source(&request, &valid);
    assert_eq!(
        request.execute().unwrap().result.phase,
        ReplayPhase::Prepared
    );
}
