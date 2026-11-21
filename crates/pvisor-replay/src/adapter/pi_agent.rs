use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{RunContext, check_boundary, prepared_outcome, run_sdk_bridge};
use crate::api::{AgentKind, PlaybackRequest, ReplayMode};
use crate::api::{ReplayError, ReplayErrorKind};
use crate::error::ResultExt;
use crate::io::{atomic_write, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{ReplayOutcome, ReplayPlan, ToolBatch, ToolCall};

const SUPPORTED_TOOLS: &[&str] = &["read", "bash", "edit", "write"];

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let events = plan.native.as_array().ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "Pi replay plan has no native event array",
        )
    })?;
    let (prefix, _) = split_events_at_boundary(events, plan.batches.last())?;
    let path = context.output_dir.join("native/prepared-prefix.jsonl");
    write_jsonl(&path, prefix)?;
    journal.append(
        "session_rebuilt",
        [(
            "prepared_only".into(),
            json!(context.request.mode == ReplayMode::PrepareOnly),
        )],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(path, context.request));
    }
    run_sdk_bridge(plan, context, journal, AgentKind::PiAgent)
}

fn split_events_at_boundary<'a>(
    events: &'a [Value],
    selected: Option<&ToolBatch>,
) -> Result<(&'a [Value], &'a [Value]), ReplayError> {
    let selected = selected.ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "Pi replay plan has no selected batch",
        )
    })?;
    let end = selected
        .native
        .get("event_index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| index.checked_add(1))
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                "Pi replay plan has an invalid boundary event index",
            )
        })?;
    events.split_at_checked(end).ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "Pi replay plan boundary exceeds its native events",
        )
    })
}

pub(super) fn build(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let events = parse_jsonl_events(&raw)?;
    let last_turn_end = events
        .iter()
        .rposition(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"));
    let mut batches = Vec::new();
    for (event_index, event) in events.iter().enumerate() {
        if event.get("type").and_then(Value::as_str) != Some("turn_end") {
            continue;
        }
        let Some(message) = event.get("message") else {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has no message"
            )));
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] message is not an assistant message"
            )));
        }
        let native_calls = pi_calls(message, event_index)?;
        if native_calls.is_empty() {
            continue;
        }
        let results = event
            .get("toolResults")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ReplayError::trajectory(format!(
                    "Pi turn_end event[{event_index}] has no toolResults array"
                ))
            })?;
        if results.len() != native_calls.len() {
            // A killed source run can end with an incomplete final batch. It is
            // not selectable, but earlier complete batches remain replayable.
            if Some(event_index) == last_turn_end {
                break;
            }
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has {} tool calls but {} results",
                native_calls.len(),
                results.len()
            )));
        }
        let mut result_ids = BTreeSet::new();
        for result in results {
            let result_id = result
                .get("toolCallId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ReplayError::trajectory(format!(
                        "Pi turn_end event[{event_index}] has a result without toolCallId"
                    ))
                })?;
            if !result_ids.insert(result_id) {
                return Err(ReplayError::trajectory(format!(
                    "Pi turn_end event[{event_index}] has duplicate result id {result_id:?}"
                )));
            }
        }
        let mut calls = Vec::with_capacity(native_calls.len());
        for (ordinal, call) in native_calls.into_iter().enumerate() {
            let call_id = call.id.to_owned();
            let result = results
                .iter()
                .find(|result| {
                    result.get("toolCallId").and_then(Value::as_str) == Some(call_id.as_str())
                })
                .ok_or_else(|| {
                    ReplayError::trajectory(format!(
                        "Pi turn_end event[{event_index}] has no result for tool call {call_id:?}"
                    ))
                })?;
            if result.get("toolName").and_then(Value::as_str) != Some(call.name) {
                return Err(ReplayError::trajectory(format!(
                    "Pi result for {call_id:?} does not match its tool name"
                )));
            }
            let native = json!({
                "id": call.id,
                "name": call.name,
                "arguments": &call.arguments,
            });
            calls.push(ToolCall {
                ordinal: ordinal + 1,
                call_id,
                name: call.name.to_owned(),
                arguments: Value::Object(call.arguments),
                original_observation: result.get("content").cloned().unwrap_or(Value::Null),
                original_is_error: result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                native,
            });
        }
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("events:{event_index}"),
            tool_calls: calls,
            assistant_text: pi_text(message),
            native: json!({"event_index": event_index}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    let selected = batches
        .get(..request.after_step)
        .and_then(|selected| selected.last());
    let (prefix, suffix) = split_events_at_boundary(&events, selected)?;
    let prefix_model_turns = prefix
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
        .count();
    let original_next_action = suffix
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
        .and_then(|event| event.get("message"))
        .map(pi_action_signature);
    batches.truncate(request.after_step);
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        batches,
        prefix_model_turns,
        native: Value::Array(events),
        original_next_action,
    })
}

fn parse_jsonl_events(raw: &[u8]) -> Result<Vec<Value>, ReplayError> {
    let source = std::str::from_utf8(raw).map_err(|error| {
        ReplayError::trajectory(format!("Pi event JSONL is not UTF-8: {error}"))
    })?;
    let physical = source.split('\n').collect::<Vec<_>>();
    let mut events = Vec::new();
    for (index, line) in physical.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(event)) => events.push(Value::Object(event)),
            Ok(_) => {
                return Err(ReplayError::trajectory(format!(
                    "Pi event JSONL line {} must contain an object",
                    index + 1
                )));
            }
            Err(_) if index + 1 == physical.len() && !source.ends_with('\n') => {
                // The source process may be killed while writing its final line.
            }
            Err(error) => {
                return Err(ReplayError::trajectory(format!(
                    "invalid Pi event JSONL line {}: {error}",
                    index + 1
                )));
            }
        }
    }
    if !events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("message_end")
            && event.pointer("/message/role").and_then(Value::as_str) == Some("user")
    }) {
        return Err(ReplayError::trajectory(
            "Pi event trajectory has no native user message",
        ));
    }
    Ok(events)
}

struct PiCall<'a> {
    id: &'a str,
    name: &'a str,
    arguments: Map<String, Value>,
}

fn pi_calls(message: &Value, event_index: usize) -> Result<Vec<PiCall<'_>>, ReplayError> {
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] assistant content must be an array"
            ))
        })?;
    let mut calls = Vec::new();
    let mut call_ids = BTreeSet::new();
    for (part_index, part) in content.iter().enumerate() {
        if part.get("type").and_then(Value::as_str) != Some("toolCall") {
            continue;
        }
        let id = part.get("id").and_then(Value::as_str).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] toolCall[{part_index}] has no id"
            ))
        })?;
        let name = part.get("name").and_then(Value::as_str).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] toolCall[{part_index}] has no name"
            ))
        })?;
        if !call_ids.insert(id) {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has duplicate tool call id {id:?}"
            )));
        }
        if !SUPPORTED_TOOLS.contains(&name) {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("Pi Replay profile does not support tool {name:?}"),
            ));
        }
        let arguments = match part.get("arguments") {
            None => Map::new(),
            Some(Value::Object(arguments)) => arguments.clone(),
            Some(_) => {
                return Err(ReplayError::trajectory(format!(
                    "Pi tool call {id:?} arguments must be an object"
                )));
            }
        };
        calls.push(PiCall {
            id,
            name,
            arguments,
        });
    }
    Ok(calls)
}

fn pi_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pi_action_signature(message: &Value) -> Value {
    let mut reasoning = Vec::new();
    let mut tools = Vec::new();
    for part in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match part.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                if let Some(value) = part.get("thinking").and_then(Value::as_str) {
                    reasoning.push(value);
                }
            }
            Some("toolCall") => tools.push(json!({
                "name": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                "arguments": part.get("arguments").cloned().unwrap_or_else(|| json!({})),
            })),
            _ => {}
        }
    }
    json!({
        "text": pi_text(message),
        "reasoning": reasoning.join("\n\n"),
        "tools": tools,
    })
}

fn write_jsonl(path: &std::path::Path, values: &[Value]) -> Result<(), ReplayError> {
    let mut rendered = Vec::new();
    for value in values {
        serde_json::to_writer(&mut rendered, value)
            .replay_context(ReplayErrorKind::Executor, "serialize Pi event JSONL")?;
        rendered.push(b'\n');
    }
    atomic_write(path, &rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn request(root: &Path) -> PlaybackRequest {
        PlaybackRequest {
            agent: AgentKind::PiAgent,
            trajectory: root.join("events.jsonl"),
            after_step: 1,
            workspace: root.join("workspace"),
            state_dir: root.join("state"),
            output_dir: root.join("output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: vec![],
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::ReplayOnly,
            allow_stale_observations: false,
            run_id: Some("pi-boundary".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        }
    }

    fn events() -> Vec<Value> {
        vec![
            json!({"type": "message_end", "message": {"role": "user"}}),
            json!({
                "type": "turn_end",
                "message": {"role": "assistant", "content": [
                    {"type": "toolCall", "id": "c", "name": "read"}
                ]},
                "toolResults": [{"toolCallId": "c", "toolName": "read"}]
            }),
        ]
    }

    fn fixture_plan(request: &PlaybackRequest, events: &[Value]) -> ReplayPlan {
        let raw = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&request.trajectory, raw).unwrap();
        build(request).unwrap()
    }

    #[rstest::rstest]
    #[case::no_batches(|plan: &mut ReplayPlan| plan.batches.clear())]
    #[case::no_event_array(|plan: &mut ReplayPlan| plan.native = Value::Null)]
    #[case::empty_events(|plan: &mut ReplayPlan| plan.native = json!([]))]
    #[case::missing_index(|plan: &mut ReplayPlan| plan.batches[0].native = json!({}))]
    #[case::negative_index(|plan: &mut ReplayPlan| plan.batches[0].native["event_index"] = json!(-1))]
    #[case::fractional_index(|plan: &mut ReplayPlan| plan.batches[0].native["event_index"] = json!(1.5))]
    #[case::string_index(|plan: &mut ReplayPlan| plan.batches[0].native["event_index"] = json!("1"))]
    #[case::past_end(|plan: &mut ReplayPlan| plan.batches[0].native["event_index"] = json!(2))]
    #[case::maximum_index(|plan: &mut ReplayPlan| plan.batches[0].native["event_index"] = json!(u64::MAX))]
    fn invalid_internal_boundaries_fail_before_prefix_or_journal_publication(
        #[case] corrupt: fn(&mut ReplayPlan),
    ) {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path());
        let mut plan = fixture_plan(&request, &events());
        corrupt(&mut plan);
        let mut journal = Journal::open(&request.state_dir).unwrap();
        let before = fs::read(&journal.path).unwrap();
        let context = RunContext {
            request: &request,
            state_dir: &request.state_dir,
            output_dir: &request.output_dir,
            launch: None,
            session_id: "pi-boundary",
            nonce: "fixture",
        };
        let error = execute(&plan, &context, &mut journal).unwrap_err();
        assert_eq!(error.kind, ReplayErrorKind::Internal);
        assert!(!request.output_dir.exists());
        assert_eq!(fs::read(&journal.path).unwrap(), before);
        journal.append("fixture_after_rejection", []).unwrap();
    }

    #[rstest::rstest]
    #[case::first(0, 1)]
    #[case::last(1, 2)]
    fn checked_boundaries_partition_events_without_changing_them(
        #[case] index: u64,
        #[case] count: usize,
    ) {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path());
        let mut plan = fixture_plan(&request, &events());
        plan.batches[0].native["event_index"] = json!(index);
        let native = plan.native.as_array().unwrap();
        let (prefix, suffix) = split_events_at_boundary(native, plan.batches.last()).unwrap();
        assert_eq!(prefix.len(), count);
        assert_eq!([prefix, suffix].concat(), *native);
    }

    #[rstest::rstest]
    #[case::missing_id(json!({"type": "toolCall", "name": "read"}), ReplayErrorKind::Trajectory)]
    #[case::invalid_id(json!({"type": "toolCall", "id": 1, "name": "read"}), ReplayErrorKind::Trajectory)]
    #[case::missing_name(json!({"type": "toolCall", "id": "c"}), ReplayErrorKind::Trajectory)]
    #[case::invalid_name(json!({"type": "toolCall", "id": "c", "name": false}), ReplayErrorKind::Trajectory)]
    #[case::unsupported(json!({"type": "toolCall", "id": "c", "name": "python"}), ReplayErrorKind::UnsupportedVersion)]
    #[case::null_arguments(json!({"type": "toolCall", "id": "c", "name": "read", "arguments": null}), ReplayErrorKind::Trajectory)]
    #[case::array_arguments(json!({"type": "toolCall", "id": "c", "name": "read", "arguments": []}), ReplayErrorKind::Trajectory)]
    #[case::string_arguments(json!({"type": "toolCall", "id": "c", "name": "read", "arguments": "{}"}), ReplayErrorKind::Trajectory)]
    fn invalid_tool_fields_retain_their_error_categories(
        #[case] call: Value,
        #[case] kind: ReplayErrorKind,
    ) {
        let message = json!({"content": [call]});
        assert_eq!(pi_calls(&message, 7).err().unwrap().kind, kind);
    }

    #[rstest::rstest]
    #[case::read("read")]
    #[case::bash("bash")]
    #[case::edit("edit")]
    #[case::write("write")]
    fn supported_tools_keep_empty_ids_and_default_arguments(#[case] name: &str) {
        let message = json!({"content": [{"type": "toolCall", "id": "", "name": name}]});
        let calls = pi_calls(&message, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "");
        assert_eq!(calls[0].name, name);
        assert!(calls[0].arguments.is_empty());
    }

    #[rstest::rstest]
    fn normalized_calls_preserve_arguments_and_match_reordered_observations_by_id() {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path());
        let mut events = events();
        events[1] = json!({
            "type": "turn_end",
            "message": {"role": "assistant", "content": [
                {"type": "toolCall", "id": "a", "name": "read"},
                {"type": "toolCall", "id": "b", "name": "write", "arguments": {"nested": {"text": "内容\n"}}, "ignored": true}
            ]},
            "toolResults": [
                {"toolCallId": "b", "toolName": "write", "content": "second", "isError": true},
                {"toolCallId": "a", "toolName": "read", "content": "first", "isError": false}
            ]
        });
        let plan = fixture_plan(&request, &events);
        let calls = &plan.batches[0].tool_calls;
        assert_eq!(
            calls[0].native,
            json!({"id": "a", "name": "read", "arguments": {}})
        );
        assert_eq!(calls[0].original_observation, json!("first"));
        assert!(!calls[0].original_is_error);
        assert_eq!(calls[1].arguments, json!({"nested": {"text": "内容\n"}}));
        assert_eq!(
            calls[1].native,
            json!({"id": "b", "name": "write", "arguments": {"nested": {"text": "内容\n"}}})
        );
        assert_eq!(calls[1].original_observation, json!("second"));
        assert!(calls[1].original_is_error);
    }

    #[test]
    fn pi_signature_separates_text_reasoning_and_tools() {
        let signature = pi_action_signature(&json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "inspect"},
                {"type": "text", "text": "I will inspect."},
                {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "a"}}
            ]
        }));
        assert_eq!(signature["text"], "I will inspect.");
        assert_eq!(signature["reasoning"], "inspect");
        assert_eq!(signature["tools"][0]["name"], "read");
    }

    #[test]
    fn pi_jsonl_tolerates_only_a_truncated_final_line() {
        let raw = b"{\"type\":\"message_end\",\"message\":{\"role\":\"user\"}}\n{\"type\":";
        assert_eq!(parse_jsonl_events(raw).unwrap().len(), 1);
        let invalid = b"{\"type\":\"message_end\",\"message\":{\"role\":\"user\"}}\nnot-json\n";
        assert!(parse_jsonl_events(invalid).is_err());
    }
}
