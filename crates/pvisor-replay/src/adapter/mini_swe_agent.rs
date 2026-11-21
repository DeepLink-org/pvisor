use serde_json::{Value, json};

use super::{RunContext, check_boundary, prepared_outcome, run_sdk_bridge};
use crate::api::{AgentKind, PlaybackRequest, ReplayMode};
use crate::api::{ReplayError, ReplayErrorKind};
use crate::error::ResultExt;
use crate::io::{atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{ReplayOutcome, ReplayPlan, ToolBatch, ToolCall};

struct MiniCall {
    id: String,
    arguments: Value,
    native: Value,
}

fn message_index(batch: &ToolBatch, key: &str) -> Result<usize, ReplayError> {
    batch
        .native
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                format!("mini-swe-agent batch has invalid {key}"),
            )
        })
}

fn selected_prefix<'a>(
    messages: &'a [Value],
    batches: &[ToolBatch],
    key: &str,
) -> Result<&'a [Value], ReplayError> {
    let batch = batches.last().ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "mini-swe-agent plan has no selected batch",
        )
    })?;
    let end = message_index(batch, key)?.checked_add(1).ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "mini-swe-agent prefix boundary overflows",
        )
    })?;
    messages.get(..end).ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "mini-swe-agent prefix boundary is out of bounds",
        )
    })
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_mini(plan, context, journal)
}

fn mini_reasoning(message: &Value) -> &str {
    message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .pointer("/extra/response/choices/0/message/reasoning_content")
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
}

fn mini_batch_signature(batch: &ToolBatch, message: &Value) -> Value {
    json!({
        "text": batch.assistant_text.as_str(),
        "reasoning": mini_reasoning(message),
        "tools": batch.tool_calls.iter().map(|call| json!({
            "name": call.name.as_str(),
            "arguments": &call.arguments,
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn build(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let value: Value = serde_json::from_slice(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "invalid mini-swe-agent trajectory JSON",
    )?;
    if value.get("trajectory_format").and_then(Value::as_str) != Some("mini-swe-agent-1.1") {
        return Err(ReplayError::trajectory(
            "mini-swe-agent trajectory_format must be mini-swe-agent-1.1",
        ));
    }
    if value
        .get("info")
        .and_then(|info| info.get("mini_version"))
        .and_then(Value::as_str)
        != Some("2.4.6")
    {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            "mini-swe-agent trajectory requires exact version 2.4.6",
        ));
    }
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ReplayError::trajectory("mini-swe-agent messages must be an array"))?;
    let mut batches = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        let native_calls = mini_calls(message, message_index)?;
        if native_calls.is_empty() {
            continue;
        }
        let mut observations = Vec::new();
        let mut observation_end = message_index;
        for (candidate_index, candidate) in messages.iter().enumerate().skip(message_index + 1) {
            if !mini_calls(candidate, candidate_index)?.is_empty() {
                break;
            }
            if matches!(
                candidate.get("role").and_then(Value::as_str),
                Some("tool" | "user")
            ) || candidate.get("type").and_then(Value::as_str) == Some("function_call_output")
            {
                observations.push(candidate);
                observation_end = candidate_index;
                if observations.len() == native_calls.len() {
                    break;
                }
            }
        }
        if observations.len() != native_calls.len() {
            break;
        }
        let batch_is_in_prefix = batches.len() < request.after_step;
        let calls = native_calls
            .into_iter()
            .zip(observations)
            .enumerate()
            .map(|(index, (native, observation))| {
                let command = native
                    .arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if mini_submission_in_prefix(batch_is_in_prefix, command) {
                    return Err(ReplayError::new(
                        ReplayErrorKind::UnsupportedVersion,
                        "mini-swe-agent submission cannot appear inside a replay prefix",
                    ));
                }
                let return_code = observation
                    .get("extra")
                    .and_then(|extra| extra.get("returncode"))
                    .and_then(Value::as_i64);
                let normalized = json!({
                    "id": &native.id,
                    "arguments": &native.arguments,
                    "native": &native.native,
                });
                Ok(ToolCall {
                    ordinal: index + 1,
                    call_id: native.id,
                    name: "bash".into(),
                    arguments: native.arguments,
                    original_observation: mini_observation(observation),
                    original_is_error: return_code.is_some_and(|code| code != 0),
                    native: normalized,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("messages:{message_index}"),
            tool_calls: calls,
            assistant_text: message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            native: json!({"message_index": message_index, "observation_end": observation_end}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    let original_next_action = if let Some(batch) = batches.get(request.after_step) {
        let message_index = message_index(batch, "message_index")?;
        let message = messages.get(message_index).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "mini-swe-agent next action message index {message_index} is out of bounds"
            ))
        })?;
        Some(mini_batch_signature(batch, message))
    } else {
        None
    };
    batches.truncate(request.after_step);
    let prefix_model_turns = selected_prefix(messages, &batches, "message_index")?
        .iter()
        .filter(|message| {
            message
                .get("extra")
                .and_then(|extra| extra.get("response"))
                .is_some_and(Value::is_object)
        })
        .count();
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        prefix_model_turns,
        batches,
        native: value,
        original_next_action,
    })
}

fn mini_submission_in_prefix(batch_is_in_prefix: bool, command: &str) -> bool {
    batch_is_in_prefix
        && command
            .trim_start()
            .starts_with("echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT")
}

fn mini_calls(message: &Value, message_index: usize) -> Result<Vec<MiniCall>, ReplayError> {
    if let Some(actions) = message
        .get("extra")
        .and_then(|extra| extra.get("actions"))
        .and_then(Value::as_array)
    {
        let native_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let command = action
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ReplayError::trajectory(format!(
                            "mini-swe-agent message[{message_index}] has an invalid native action"
                        ))
                    })?;
                let call_id = action
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        native_calls
                            .get(index)
                            .and_then(|call| call.get("id"))
                            .and_then(Value::as_str)
                    })
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("mini-{message_index}-{}", index + 1));
                Ok(MiniCall {
                    id: call_id,
                    arguments: json!({"command": command}),
                    native: action.clone(),
                })
            })
            .collect();
    }
    let mut result = Vec::new();
    for (index, call) in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ReplayError::trajectory(format!(
                    "mini-swe-agent message[{message_index}] has an invalid tool call"
                ))
            })?;
        if function.get("name").and_then(Value::as_str) != Some("bash") {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                "mini-swe-agent playback supports only native bash actions",
            ));
        }
        let arguments = match function.get("arguments") {
            Some(Value::String(raw)) => serde_json::from_str(raw).replay_context(
                ReplayErrorKind::Trajectory,
                "invalid mini-swe-agent tool arguments",
            )?,
            Some(value) => value.clone(),
            None => json!({}),
        };
        if arguments.get("command").and_then(Value::as_str).is_none() {
            return Err(ReplayError::trajectory(
                "mini-swe-agent bash action has no command",
            ));
        }
        result.push(MiniCall {
            id: call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("mini-{message_index}-{}", index + 1)),
            arguments,
            native: call.clone(),
        });
    }
    Ok(result)
}

fn mini_observation(message: &Value) -> Value {
    message
        .get("extra")
        .and_then(|extra| extra.get("raw_output"))
        .cloned()
        .or_else(|| message.get("output").cloned())
        .or_else(|| message.get("content").cloned())
        .unwrap_or(Value::String(String::new()))
}

fn run_mini(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    // A prepare-only artifact must include every result of the selected batch.
    // Live replay continues to reconstruct fresh results through the SDK bridge.
    let boundary_key = if context.request.mode == ReplayMode::PrepareOnly {
        "observation_end"
    } else {
        "message_index"
    };
    let messages = plan
        .native
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                "mini-swe-agent plan has no messages array",
            )
        })?;
    let prefix = selected_prefix(messages, &plan.batches, boundary_key)?;
    let mut prepared = plan.native.clone();
    prepared["messages"] = Value::Array(prefix.to_vec());
    let path = context.output_dir.join("native/prepared-prefix.json");
    atomic_write_json(&path, &prepared)?;
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
    run_sdk_bridge(plan, context, journal, AgentKind::MiniSweAgent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[rstest::rstest]
    #[case::missing(Value::Null, "boundary")]
    #[case::negative(json!(-1), "boundary")]
    #[case::fractional(json!(1.5), "boundary")]
    #[case::out_of_bounds(json!(2), "boundary")]
    #[case::maximum(json!(u64::MAX), "boundary")]
    #[case::no_batches(json!(0), "batches")]
    #[case::no_messages(json!(0), "messages")]
    fn invalid_boundaries_fail_before_publication(
        #[case] boundary: Value,
        #[case] fault: &str,
        #[values(ReplayMode::PrepareOnly, ReplayMode::ReplayOnly)] mode: ReplayMode,
    ) {
        let root = tempfile::tempdir().unwrap();
        let request = PlaybackRequest {
            agent: AgentKind::MiniSweAgent,
            trajectory: root.path().join("source.json"),
            after_step: 1,
            workspace: root.path().join("workspace"),
            state_dir: root.path().join("state"),
            output_dir: root.path().join("output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: vec![],
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode,
            allow_stale_observations: false,
            run_id: Some("mini-boundary".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        };
        let mut plan = ReplayPlan {
            agent: request.agent,
            source_path: request.trajectory.clone(),
            source_sha256: String::new(),
            after_step: 1,
            prefix_model_turns: 1,
            batches: vec![ToolBatch {
                ordinal: 1,
                native_locator: String::new(),
                tool_calls: vec![],
                assistant_text: String::new(),
                native: json!({"observation_end": boundary, "message_index": boundary}),
            }],
            native: json!({"messages": [{"role": "assistant"}]}),
            original_next_action: None,
        };
        let mut journal = Journal::open(&request.state_dir).unwrap();
        let before = fs::read(&journal.path).unwrap();
        let context = RunContext {
            request: &request,
            state_dir: &request.state_dir,
            output_dir: &request.output_dir,
            launch: None,
            session_id: "fixture",
            nonce: "fixture",
        };
        if fault == "messages" {
            plan.native = Value::Null;
        } else if fault == "batches" {
            plan.batches.clear();
        }
        assert_eq!(
            execute(&plan, &context, &mut journal).unwrap_err().kind,
            ReplayErrorKind::Internal
        );
        assert_eq!(fs::read(&journal.path).unwrap(), before);
        assert!(!request.output_dir.exists());
    }

    #[rstest::rstest]
    fn replay_and_preparation_select_different_valid_prefixes() {
        let messages = vec![json!("action"), json!("result one"), json!("result two")];
        let batches = vec![ToolBatch {
            ordinal: 1,
            native_locator: String::new(),
            tool_calls: vec![],
            assistant_text: String::new(),
            native: json!({"message_index": 0, "observation_end": 2}),
        }];
        assert_eq!(
            selected_prefix(&messages, &batches, "message_index").unwrap(),
            &messages[..1]
        );
        assert_eq!(
            selected_prefix(&messages, &batches, "observation_end").unwrap(),
            messages
        );
    }

    #[rstest::rstest]
    #[case::invalid_native_action(json!({"extra": {"actions": [{"command": 1}]}}), ReplayErrorKind::Trajectory)]
    #[case::missing_function(json!({"tool_calls": [{}]}), ReplayErrorKind::Trajectory)]
    #[case::unsupported_tool(json!({"tool_calls": [{"function": {"name": "read"}}]}), ReplayErrorKind::UnsupportedVersion)]
    #[case::invalid_json(json!({"tool_calls": [{"function": {"name": "bash", "arguments": "{"}}]}), ReplayErrorKind::Trajectory)]
    #[case::missing_command(json!({"tool_calls": [{"function": {"name": "bash", "arguments": {}}}]}), ReplayErrorKind::Trajectory)]
    fn malformed_calls_keep_their_error_classification(
        #[case] message: Value,
        #[case] kind: ReplayErrorKind,
    ) {
        assert_eq!(mini_calls(&message, 0).err().unwrap().kind, kind);
    }

    #[test]
    fn mini_submit_is_rejected_only_inside_the_selected_prefix() {
        let command = "  echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT";
        assert!(mini_submission_in_prefix(true, command));
        assert!(!mini_submission_in_prefix(false, command));
        assert!(!mini_submission_in_prefix(true, "echo still-working"));
    }
}
