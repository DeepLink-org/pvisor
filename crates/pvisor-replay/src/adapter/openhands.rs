use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    LaunchSpec, MAX_TOOL_OUTPUT_BYTES, RunContext, agent_command, check_boundary, prepared_outcome,
    with_boundary_user_prompt_metadata,
};
use crate::api::{PlaybackRequest, ReplayMode};
use crate::api::{ReplayError, ReplayErrorKind};
use crate::error::ResultExt;
use crate::io::{atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{FreshObservation, ReplayOutcome, ReplayPlan, ToolBatch, ToolCall};
use crate::process::{ProcessSpec, run_process};

const SUPPORTED_ACTIONS: &[&str] = &["run", "read", "edit", "run_ipython", "think"];

fn replay_action(event: &Value) -> Result<Option<&str>, ReplayError> {
    if event.get("source").and_then(Value::as_str) != Some("agent") {
        return Ok(None);
    }
    let Some(action) = event.get("action").and_then(Value::as_str) else {
        return Ok(None);
    };
    if matches!(action, "system" | "finish" | "message") {
        return Ok(None);
    }
    if !SUPPORTED_ACTIONS.contains(&action) {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!("unsupported OpenHands action {action:?}"),
        ));
    }
    Ok(Some(action))
}

fn boundary_action_id(batches: &[ToolBatch]) -> Result<i64, ReplayError> {
    let batch = batches.last().ok_or_else(|| {
        ReplayError::new(
            ReplayErrorKind::Internal,
            "OpenHands replay plan has no selected batch",
        )
    })?;
    let [call] = batch.tool_calls.as_slice() else {
        return Err(ReplayError::new(
            ReplayErrorKind::Internal,
            "OpenHands replay boundary must contain exactly one action",
        ));
    };
    call.call_id.parse().replay_context(
        ReplayErrorKind::Internal,
        "OpenHands replay boundary call ID is not an i64",
    )
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_openhands(plan, context, journal)
}

pub(super) fn build(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let events: Vec<Value> = serde_json::from_slice(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "invalid OpenHands trajectory JSON",
    )?;
    if events.is_empty() {
        return Err(ReplayError::trajectory(
            "OpenHands trajectory must be a non-empty event array",
        ));
    }
    let mut ids = BTreeSet::new();
    for event in &events {
        let id = event_id(event)?;
        if !ids.insert(id) {
            return Err(ReplayError::trajectory(format!(
                "duplicate OpenHands event id {id}"
            )));
        }
    }
    let observations: BTreeMap<i64, &Value> = events
        .iter()
        .filter_map(|event| {
            (event.get("observation").is_some() && !event["observation"].is_null())
                .then(|| {
                    event
                        .get("cause")
                        .and_then(Value::as_i64)
                        .map(|cause| (cause, event))
                })
                .flatten()
        })
        .collect();
    let mut batches = Vec::new();
    for action in &events {
        let Some(action_name) = replay_action(action)? else {
            continue;
        };
        let id = event_id(action)?;
        let Some(observation) = observations.get(&id) else {
            break;
        };
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("event:{id}"),
            tool_calls: vec![ToolCall {
                ordinal: batches.len() + 1,
                call_id: id.to_string(),
                name: action_name.to_owned(),
                arguments: action.get("args").cloned().unwrap_or_else(|| json!({})),
                original_observation: json!({
                    "observation": observation.get("observation"),
                    "message": observation.get("message"),
                    "args": observation.get("args"),
                }),
                original_is_error: observation.get("observation").and_then(Value::as_str)
                    == Some("error"),
                native: action.clone(),
            }],
            assistant_text: action
                .get("args")
                .and_then(|args| args.get("thought"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            native: json!({"observation_id": observation.get("id")}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    batches.truncate(request.after_step);
    let boundary_id = boundary_action_id(&batches)?;
    let initial_user_event = events
        .iter()
        .find(|event| {
            event.get("source").and_then(Value::as_str) == Some("user")
                && event.get("action").and_then(Value::as_str) == Some("message")
                && event.get("id").and_then(Value::as_i64).unwrap_or(i64::MAX) <= boundary_id
        })
        .cloned()
        .ok_or_else(|| {
            ReplayError::trajectory("OpenHands replay has no user message through the boundary")
        })?;
    let original_next_action = events.iter().find_map(|event| {
        let id = event.get("id").and_then(Value::as_i64)?;
        let action = event.get("action").and_then(Value::as_str)?;
        if id <= boundary_id
            || event.get("source").and_then(Value::as_str) != Some("agent")
            || action == "finish"
        {
            return None;
        }
        Some(openhands_action_signature(event))
    });
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        prefix_model_turns: request.after_step,
        batches,
        native: json!({"events": events, "initial_user_event": initial_user_event}),
        original_next_action,
    })
}

fn openhands_action_signature(event: &Value) -> Value {
    let response_message = event
        .get("tool_call_metadata")
        .and_then(|metadata| metadata.get("model_response"))
        .and_then(|response| response.get("choices"))
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"));
    let text = response_message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning = response_message
        .and_then(|message| message.get("reasoning_content"))
        .and_then(Value::as_str)
        .or_else(|| {
            response_message.is_none().then(|| {
                event
                    .get("args")
                    .and_then(|args| args.get("thought"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default();
    json!({
        "text": text,
        "reasoning": reasoning,
        "tools": [{
            "name": event.get("action").and_then(Value::as_str).unwrap_or_default(),
            "arguments": openhands_reconstructed_tool_arguments(event),
        }],
    })
}

fn openhands_reconstructed_tool_metadata(event: &Value) -> Result<Value, ReplayError> {
    let event_id = event_id(event)?;
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("OpenHands replay action has no action"))?;
    let tool_name = match action {
        "run" => "execute_bash",
        "read" | "edit" => "str_replace_editor",
        "run_ipython" => "execute_ipython_cell",
        "think" => "think",
        _ => {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("unsupported OpenHands action {action:?}"),
            ));
        }
    };
    let tool_call_id = format!("sandbox-playback-replay-{event_id}");
    let arguments = openhands_reconstructed_tool_arguments(event);
    let serialized_arguments = serde_json::to_string(&arguments).replay_context(
        ReplayErrorKind::Internal,
        "serialize reconstructed OpenHands tool arguments",
    )?;
    let thought = event
        .get("args")
        .and_then(|args| args.get("thought"))
        .and_then(Value::as_str)
        .filter(|thought| !thought.is_empty())
        .map(str::to_owned);
    Ok(json!({
        "function_name": tool_name,
        "tool_call_id": tool_call_id.clone(),
        "total_calls_in_response": 1,
        "model_response": {
            "id": format!("sandbox-playback-response-{event_id}"),
            "created": 0,
            "model": "sandbox-playback/reconstructed",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": thought,
                    "tool_calls": [{
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": serialized_arguments,
                        },
                    }],
                },
            }],
        },
    }))
}

fn openhands_reconstructed_tool_arguments(event: &Value) -> Value {
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = event.get("args").cloned().unwrap_or_else(|| json!({}));
    match action {
        "run" => {
            let mut arguments = serde_json::Map::from_iter([(
                "command".to_owned(),
                source.get("command").cloned().unwrap_or_else(|| json!("")),
            )]);
            if let Some(value) = source.get("is_input") {
                arguments.insert(
                    "is_input".to_owned(),
                    if let Some(value) = value.as_bool() {
                        Value::String(value.to_string())
                    } else {
                        value.clone()
                    },
                );
            }
            if let Some(value) = source.get("timeout").filter(|value| !value.is_null()) {
                arguments.insert("timeout".to_owned(), value.clone());
            }
            Value::Object(arguments)
        }
        "run_ipython" => json!({
            "code": source.get("code").cloned().unwrap_or_else(|| json!("")),
        }),
        "read" => {
            let mut arguments = serde_json::Map::from_iter([
                ("command".to_owned(), json!("view")),
                (
                    "path".to_owned(),
                    source.get("path").cloned().unwrap_or_else(|| json!("")),
                ),
            ]);
            if let Some(value) = source.get("view_range").filter(|value| !value.is_null()) {
                arguments.insert("view_range".to_owned(), value.clone());
            }
            Value::Object(arguments)
        }
        "edit" => {
            let mut arguments = serde_json::Map::new();
            for key in [
                "command",
                "path",
                "file_text",
                "old_str",
                "new_str",
                "insert_line",
                "view_range",
            ] {
                if let Some(value) = source.get(key) {
                    arguments.insert(key.to_owned(), value.clone());
                }
            }
            arguments
                .entry("command".to_owned())
                .or_insert(json!("str_replace"));
            Value::Object(arguments)
        }
        "think" => json!({
            "thought": source.get("thought").cloned().unwrap_or_else(|| json!("")),
        }),
        _ => source,
    }
}

fn event_id(event: &Value) -> Result<i64, ReplayError> {
    event
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| ReplayError::trajectory("OpenHands event has no integer id"))
}

fn run_openhands(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let events = plan
        .native
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                "OpenHands replay plan has no native event array",
            )
        })?;
    let initial = plan
        .native
        .get("initial_user_event")
        .filter(|event| event.is_object())
        .cloned()
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                "OpenHands replay plan has no initial user object",
            )
        })?;
    let boundary_id = boundary_action_id(&plan.batches)?;
    let boundary_event = events
        .iter()
        .find(|event| event.get("id").and_then(Value::as_i64) == Some(boundary_id))
        .ok_or_else(|| {
            ReplayError::new(
                ReplayErrorKind::Internal,
                "OpenHands boundary action is absent from native events",
            )
        })?;
    if replay_action(boundary_event)?.is_none() {
        return Err(ReplayError::new(
            ReplayErrorKind::Internal,
            "OpenHands boundary event is not a replayable agent action",
        ));
    }
    let mut prepared_events = vec![initial.clone()];
    for event in events {
        let id = event_id(event)?;
        let historical_observation = context.request.mode == ReplayMode::PrepareOnly
            && event
                .get("observation")
                .is_some_and(|value| !value.is_null())
            && plan
                .batches
                .iter()
                .any(|batch| batch.native["observation_id"].as_i64() == Some(id));
        if id > boundary_id && !historical_observation {
            if context.request.mode == ReplayMode::PrepareOnly {
                // A causal result can arrive after a later action. Preserve
                // that result while excluding the unselected action itself.
                continue;
            }
            break;
        }
        if event == &initial || event.get("action").and_then(Value::as_str) == Some("system") {
            continue;
        }
        if event.get("action").is_some() && !event["action"].is_null() {
            let mut reconstructed = event.clone();
            if reconstructed.get("source").and_then(Value::as_str) == Some("agent")
                && reconstructed
                    .get("action")
                    .and_then(Value::as_str)
                    .is_some_and(|action| SUPPORTED_ACTIONS.contains(&action))
                && reconstructed
                    .get("tool_call_metadata")
                    .is_none_or(Value::is_null)
            {
                reconstructed["tool_call_metadata"] =
                    openhands_reconstructed_tool_metadata(&reconstructed)?;
            }
            prepared_events.push(reconstructed);
        } else if historical_observation {
            prepared_events.push(event.clone());
        }
    }
    let prompt_injected = if context.request.mode == ReplayMode::ReplayAndContinue {
        if let Some(prompt) = context.request.boundary_user_prompt() {
            prepared_events.push(openhands_boundary_user_prompt_event(
                &initial, events, prompt,
            )?);
            true
        } else {
            false
        }
    } else {
        false
    };
    let prepared = context
        .output_dir
        .join("native/prepared-replay-events.json");
    atomic_write_json(&prepared, &prepared_events)?;
    journal.append(
        "session_rebuilt",
        [
            (
                "prepared_only".into(),
                json!(context.request.mode == ReplayMode::PrepareOnly),
            ),
            (
                "boundary_user_prompt_injected".into(),
                json!(prompt_injected),
            ),
        ],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(prepared, context.request));
    }
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("OpenHands replay has no launch spec"))?;
    let replayed_trajectory = match context.request.mode {
        ReplayMode::ReplayOnly => context
            .output_dir
            .join("native/reconstructed-trajectory.json"),
        ReplayMode::ReplayAndContinue => {
            context.output_dir.join("native/continued-trajectory.json")
        }
        ReplayMode::PrepareOnly => {
            return Err(ReplayError::new(
                ReplayErrorKind::Internal,
                "prepare-only reached OpenHands launch",
            ));
        }
    };
    let mut command = agent_command(&launch.entrypoint, context);
    command.args(["-m", "openhands.core.main"]);
    command.env("REPLAY_TRAJECTORY_PATH", &prepared);
    command.env("SAVE_TRAJECTORY_PATH", &replayed_trajectory);
    command.env("FILE_STORE", "local");
    command.env(
        "FILE_STORE_PATH",
        context.state_dir.join("openhands-file-store"),
    );
    command.env("RUNTIME", "local");
    command.env("SU_TO_USER", "false");
    command.env("RUN_AS_OPENHANDS", "false");
    command.env("SKIP_DEPENDENCY_CHECK", "1");
    command.env("INIT_PLUGIN_TIMEOUT", "240");
    command.env("AGENT_ENABLE_PROMPT_EXTENSIONS", "false");
    command.env("AGENT_ENABLE_BROWSING", "false");
    command.env("ENABLE_BROWSER", "false");
    command.env("SANDBOX_ENABLE_AUTO_LINT", "true");
    command.env(
        "SANDBOX_VOLUMES",
        format!("{}:/workspace:rw", context.request.workspace.display()),
    );
    prepend_openhands_runtime_tools(&mut command, launch)?;
    command.env(
        "OPENAI_CUSTOM_HEADERS",
        format!("X-LiteLLM-Session-ID: {}", context.session_id),
    );
    let iteration_limit = match context.request.mode {
        ReplayMode::ReplayOnly => Some(plan.prefix_model_turns),
        ReplayMode::ReplayAndContinue => context.request.max_steps,
        ReplayMode::PrepareOnly => None,
    };
    if let Some(max) = iteration_limit {
        command.env("MAX_ITERATIONS", max.to_string());
    }
    journal.append("continuation_started", std::iter::empty())?;
    let logs = context.output_dir.join("logs");
    let log = logs.join("openhands.log");
    fs::create_dir_all(&logs)
        .replay_context(ReplayErrorKind::Executor, "create OpenHands log directory")?;
    let output = run_process(ProcessSpec {
        command,
        stdin: Some(b"\n".to_vec()),
        idle_timeout: None,
        step_finish_limit: None,
        stdout_redirect: None,
        timeout: Duration::from_secs(24 * 60 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log.clone(),
        log_bytes_limit: None,
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
    if !output.stderr_tail.is_empty() {
        rendered.push('\n');
        rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
    }
    let fatal_marker = openhands_fatal_controller_marker(&rendered);
    if output.timed_out || !output.status.success() || !replayed_trajectory.is_file() {
        let detail = fatal_marker
            .map(|marker| format!("; OpenHands controller reported {marker:?}"))
            .unwrap_or_default();
        return Err(ReplayError::classify_continuation(
            format!(
                "OpenHands replay/continuation exited {}{detail}; see {}",
                output.status,
                log.display()
            ),
            &rendered,
        ));
    }
    if let Some(marker) = fatal_marker {
        return Err(ReplayError::classify_continuation(
            format!(
                "OpenHands controller reported {marker:?} despite exiting successfully; partial trajectory retained at {}; see {}",
                replayed_trajectory.display(),
                log.display()
            ),
            &rendered,
        ));
    }
    let continued_events: Vec<Value> =
        serde_json::from_slice(&read_regular_file(&replayed_trajectory)?).replay_context(
            ReplayErrorKind::Trajectory,
            "parse replayed OpenHands trajectory",
        )?;
    let complete = openhands_complete_batches(&continued_events)?;
    let replayed = checked_replay_prefix(
        &complete,
        plan.after_step,
        context.request.mode,
        context.request.max_steps,
    )?;
    let observations = plan
        .calls()
        .zip(replayed.iter())
        .map(|(call, (_, observation))| FreshObservation {
            call_id: call.call_id.clone(),
            content: openhands_observation_content(observation),
            is_error: observation.get("observation").and_then(Value::as_str) == Some("error"),
            return_code: None,
            duration_ms: 0,
            truncated: false,
            metadata: BTreeMap::new(),
        })
        .collect::<Vec<_>>();
    let comparisons = plan
        .calls()
        .zip(&observations)
        .map(|(call, fresh)| {
            json!({
                "call_id": call.call_id,
                "tool": call.name,
                "exact": call.original_observation == fresh.content
                    && call.original_is_error == fresh.is_error,
                "original_is_error": call.original_is_error,
                "replayed_is_error": fresh.is_error,
            })
        })
        .collect::<Vec<_>>();
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )?;
    let continued_steps = complete.len() - replayed.len();
    journal.append(
        "continuation_finished",
        [
            ("continued_steps".into(), json!(continued_steps)),
            ("agent_error".into(), Value::Null),
        ],
    )?;
    let reached_max_steps = context.request.mode == ReplayMode::ReplayAndContinue
        && context
            .request
            .max_steps
            .is_some_and(|max_steps| complete.len() == max_steps)
        && rendered.contains("Agent reached maximum iteration");
    Ok(ReplayOutcome {
        status: if context.request.mode == ReplayMode::ReplayOnly {
            "replayed".into()
        } else if reached_max_steps {
            "max_steps".into()
        } else {
            "completed".into()
        },
        reconstructed_path: (context.request.mode == ReplayMode::ReplayOnly)
            .then_some(replayed_trajectory.clone()),
        continued_path: (context.request.mode == ReplayMode::ReplayAndContinue)
            .then_some(replayed_trajectory),
        observations,
        continued_steps,
        metadata: with_boundary_user_prompt_metadata(Map::new(), context.request, prompt_injected),
    })
}

fn openhands_boundary_user_prompt_event(
    initial: &Value,
    events: &[Value],
    prompt: &str,
) -> Result<Value, ReplayError> {
    let next_id = events
        .iter()
        .map(event_id)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| ReplayError::trajectory("OpenHands event IDs are exhausted"))?;
    let mut event = initial.clone();
    let object = event
        .as_object_mut()
        .ok_or_else(|| ReplayError::trajectory("OpenHands initial user event is not an object"))?;
    object.insert("id".into(), json!(next_id));
    object.insert("source".into(), json!("user"));
    object.insert("action".into(), json!("message"));
    object.insert("args".into(), json!({"content": prompt}));
    object.remove("cause");
    object.remove("observation");
    Ok(event)
}

fn openhands_fatal_controller_marker(output: &str) -> Option<&'static str> {
    if output.contains("Agent reached maximum iteration") {
        return None;
    }
    [
        "AgentState.ERROR",
        "Error while running the agent",
        "There was an unexpected error while running the agent",
    ]
    .into_iter()
    .find(|marker| output.contains(marker))
}

fn openhands_observation_content(observation: &Value) -> Value {
    json!({
        "observation": observation.get("observation"),
        "message": observation.get("message"),
        "args": observation.get("args"),
    })
}

fn checked_replay_prefix<T>(
    complete: &[T],
    after_step: usize,
    mode: ReplayMode,
    max_steps: Option<usize>,
) -> Result<&[T], ReplayError> {
    let prefix = complete.get(..after_step).ok_or_else(|| {
        ReplayError::trajectory("OpenHands output lost replayed action/observation batches")
    })?;
    if mode == ReplayMode::ReplayOnly && complete.len() != after_step {
        return Err(ReplayError::continuation(format!(
            "OpenHands replay-only crossed the selected boundary: expected {} actions, observed {}",
            after_step,
            complete.len()
        )));
    }
    if let Some(max_steps) = max_steps
        && complete.len() > max_steps
    {
        return Err(ReplayError::continuation(format!(
            "OpenHands exceeded the total max_steps budget: allowed {}, observed {} actions",
            max_steps,
            complete.len()
        )));
    }
    Ok(prefix)
}

fn openhands_complete_batches(events: &[Value]) -> Result<Vec<(&Value, &Value)>, ReplayError> {
    let mut observations = BTreeMap::new();
    for event in events {
        let Some(cause) = event
            .get("observation")
            .filter(|value| !value.is_null())
            .and_then(|_| event.get("cause"))
            .and_then(Value::as_i64)
        else {
            continue;
        };
        if observations.insert(cause, event).is_some() {
            return Err(ReplayError::trajectory(format!(
                "multiple OpenHands observations for action {cause}"
            )));
        }
    }

    let mut batches = Vec::new();
    for event in events {
        let Some(_) = replay_action(event)? else {
            continue;
        };
        let id = event_id(event)?;
        let Some(observation) = observations.get(&id) else {
            break;
        };
        batches.push((event, *observation));
    }
    Ok(batches)
}

fn prepend_openhands_runtime_tools(
    command: &mut Command,
    launch: &LaunchSpec,
) -> Result<(), ReplayError> {
    let inferred_root = launch
        .entrypoint
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/"));
    let tools = launch
        .runtime_root
        .as_deref()
        .unwrap_or(inferred_root)
        .join("tools");
    if !tools.is_dir() {
        return Ok(());
    }
    let current = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let paths = std::iter::once(tools.clone()).chain(std::env::split_paths(&current));
    let path = std::env::join_paths(paths).map_err(|error| {
        ReplayError::configuration(format!(
            "cannot prepend OpenHands runtime tools {} to PATH: {error}",
            tools.display()
        ))
    })?;
    command.env("PATH", path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use serde_json::{Value, json};

    use super::*;
    use crate::api::AgentKind;

    fn request(root: &Path) -> PlaybackRequest {
        PlaybackRequest {
            agent: AgentKind::Openhands,
            trajectory: root.join("events.json"),
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
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: Some("openhands-boundary".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        }
    }

    fn fixture_plan(request: &PlaybackRequest) -> ReplayPlan {
        fs::create_dir(&request.workspace).unwrap();
        let events = json!([
            {"id": 0, "source": "user", "action": "message", "args": {"content": "task"}},
            {"id": 1, "source": "agent", "action": "run", "args": {"command": "true"}},
            {"id": 2, "source": "environment", "observation": "run", "cause": 1, "message": "old"}
        ]);
        fs::write(&request.trajectory, serde_json::to_vec(&events).unwrap()).unwrap();
        build(request).unwrap()
    }

    #[rstest::rstest]
    #[case::no_batches(|plan: &mut ReplayPlan| plan.batches.clear())]
    #[case::empty_batch(|plan: &mut ReplayPlan| plan.batches[0].tool_calls.clear())]
    #[case::multiple_calls(|plan: &mut ReplayPlan| {
        let call = plan.batches[0].tool_calls[0].clone();
        plan.batches[0].tool_calls.push(call);
    })]
    #[case::invalid_id(|plan: &mut ReplayPlan| plan.batches[0].tool_calls[0].call_id = "invalid".into())]
    #[case::overflow_id(|plan: &mut ReplayPlan| plan.batches[0].tool_calls[0].call_id = "9223372036854775808".into())]
    #[case::underflow_id(|plan: &mut ReplayPlan| plan.batches[0].tool_calls[0].call_id = "-9223372036854775809".into())]
    #[case::no_events(|plan: &mut ReplayPlan| plan.native["events"] = Value::Null)]
    #[case::empty_events(|plan: &mut ReplayPlan| plan.native["events"] = json!([]))]
    #[case::no_initial_user(|plan: &mut ReplayPlan| plan.native["initial_user_event"] = Value::Null)]
    #[case::non_agent_boundary(|plan: &mut ReplayPlan| plan.native["events"][1]["source"] = json!("user"))]
    fn invalid_internal_boundaries_do_not_publish_prepared_events_or_journal_facts(
        #[case] corrupt: fn(&mut ReplayPlan),
    ) {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path());
        let mut plan = fixture_plan(&request);
        corrupt(&mut plan);
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
        let error = execute(&plan, &context, &mut journal).unwrap_err();
        assert_eq!(error.kind, ReplayErrorKind::Internal);
        assert_eq!(fs::read(&journal.path).unwrap(), before);
        assert!(!request.output_dir.exists());
        journal.append("fixture_after_rejection", []).unwrap();
    }

    #[rstest::rstest]
    #[case::minimum(i64::MIN)]
    #[case::zero(0)]
    #[case::maximum(i64::MAX)]
    fn boundary_ids_retain_the_full_signed_integer_range(#[case] id: i64) {
        let root = tempfile::tempdir().unwrap();
        let request = request(root.path());
        let mut plan = fixture_plan(&request);
        plan.batches[0].tool_calls[0].call_id = id.to_string();
        assert_eq!(boundary_action_id(&plan.batches).unwrap(), id);
    }

    #[rstest::rstest]
    #[case::run(json!("agent"), json!("run"), Ok(Some("run")))]
    #[case::read(json!("agent"), json!("read"), Ok(Some("read")))]
    #[case::edit(json!("agent"), json!("edit"), Ok(Some("edit")))]
    #[case::ipython(json!("agent"), json!("run_ipython"), Ok(Some("run_ipython")))]
    #[case::think(json!("agent"), json!("think"), Ok(Some("think")))]
    #[case::system(json!("agent"), json!("system"), Ok(None))]
    #[case::finish(json!("agent"), json!("finish"), Ok(None))]
    #[case::message(json!("agent"), json!("message"), Ok(None))]
    #[case::null_action(json!("agent"), Value::Null, Ok(None))]
    #[case::non_string_action(json!("agent"), json!(false), Ok(None))]
    #[case::non_agent(json!("user"), json!("unknown"), Ok(None))]
    #[case::unsupported(json!("agent"), json!("unknown"), Err(ReplayErrorKind::UnsupportedVersion))]
    fn source_and_output_share_action_classification(
        #[case] source: Value,
        #[case] action: Value,
        #[case] expected: Result<Option<&str>, ReplayErrorKind>,
    ) {
        let event = json!({"id": 1, "source": source, "action": action});
        assert_eq!(replay_action(&event).map_err(|error| error.kind), expected);
        let events = [
            event,
            json!({"id": 2, "source": "environment", "observation": "run", "cause": 1}),
        ];
        assert_eq!(
            openhands_complete_batches(&events)
                .map(|batches| batches.len())
                .map_err(|error| error.kind),
            expected.map(|action| usize::from(action.is_some()))
        );
    }

    #[rstest::rstest]
    #[case::missing(0, 1, ReplayMode::ReplayAndContinue, None, Err((ReplayErrorKind::Trajectory, "lost replayed")))]
    #[case::exact_replay(1, 1, ReplayMode::ReplayOnly, None, Ok(1))]
    #[case::replay_crosses_boundary(2, 1, ReplayMode::ReplayOnly, Some(1), Err((ReplayErrorKind::Continuation, "replay-only crossed")))]
    #[case::unbounded(3, 1, ReplayMode::ReplayAndContinue, None, Ok(1))]
    #[case::within_budget(2, 1, ReplayMode::ReplayAndContinue, Some(3), Ok(1))]
    #[case::at_budget(2, 1, ReplayMode::ReplayAndContinue, Some(2), Ok(1))]
    #[case::exceeds_budget(3, 1, ReplayMode::ReplayAndContinue, Some(2), Err((ReplayErrorKind::Continuation, "allowed 2, observed 3")))]
    #[case::missing_precedes_budget(0, 1, ReplayMode::ReplayAndContinue, Some(0), Err((ReplayErrorKind::Trajectory, "lost replayed")))]
    #[case::maximum_budget(2, 1, ReplayMode::ReplayAndContinue, Some(usize::MAX), Ok(1))]
    #[case::invalid_boundary(1, usize::MAX, ReplayMode::ReplayAndContinue, None, Err((ReplayErrorKind::Trajectory, "lost replayed")))]
    fn checked_output_prefix_preserves_budget_boundaries_and_error_precedence(
        #[case] count: usize,
        #[case] after_step: usize,
        #[case] mode: ReplayMode,
        #[case] max_steps: Option<usize>,
        #[case] expected: Result<usize, (ReplayErrorKind, &str)>,
    ) {
        let complete = vec![(); count];
        let result = checked_replay_prefix(&complete, after_step, mode, max_steps);
        match expected {
            Ok(length) => assert_eq!(result.unwrap().len(), length),
            Err((kind, message)) => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, kind);
                assert!(error.message.contains(message), "{}", error.message);
            }
        }
    }

    #[rstest::rstest]
    fn log_setup_failure_retains_the_existing_continuation_start_fence() {
        let root = tempfile::tempdir().unwrap();
        let mut request = request(root.path());
        request.mode = ReplayMode::ReplayOnly;
        let plan = fixture_plan(&request);
        fs::create_dir(&request.output_dir).unwrap();
        fs::write(request.output_dir.join("logs"), b"obstruction").unwrap();
        let launch = LaunchSpec {
            entrypoint: root.path().join("must-not-run"),
            version: "0.53.0".into(),
            source: "fixture".into(),
            runtime_root: Some(root.path().join("runtime")),
        };
        let mut journal = Journal::open(&request.state_dir).unwrap();
        let context = RunContext {
            request: &request,
            state_dir: &request.state_dir,
            output_dir: &request.output_dir,
            launch: Some(&launch),
            session_id: "fixture",
            nonce: "fixture",
        };
        let error = execute(&plan, &context, &mut journal).unwrap_err();
        assert_eq!(error.kind, ReplayErrorKind::Executor);
        let observations = crate::journal::read_observations(&journal.path).unwrap();
        assert!(
            observations
                .iter()
                .any(|event| event["event"] == "continuation_started")
        );
        assert!(
            !observations
                .iter()
                .any(|event| event["event"] == "continuation_finished")
        );
        assert_eq!(
            fs::read(request.output_dir.join("logs")).unwrap(),
            b"obstruction"
        );
        drop(journal);
        assert_eq!(
            Journal::open(&request.state_dir).err().unwrap().kind,
            ReplayErrorKind::AmbiguousExecution
        );
    }

    #[test]
    fn openhands_boundary_prompt_is_a_unique_native_user_message() {
        let initial = json!({
            "id": 0,
            "source": "user",
            "action": "message",
            "args": {"content": "original task"},
            "timestamp": "source timestamp"
        });
        let events = vec![
            initial.clone(),
            json!({"id": 7, "source": "agent", "action": "run", "args": {"command": "pwd"}}),
            json!({"id": 8, "source": "environment", "observation": "run", "cause": 7}),
        ];

        let prompt =
            openhands_boundary_user_prompt_event(&initial, &events, "review O-prime N").unwrap();

        assert_eq!(prompt["id"], 9);
        assert_eq!(prompt["source"], "user");
        assert_eq!(prompt["action"], "message");
        assert_eq!(prompt["args"]["content"], "review O-prime N");
        assert_eq!(prompt["timestamp"], "source timestamp");
    }

    #[test]
    fn openhands_reconstructs_legacy_native_tool_metadata() {
        let event = json!({
            "id": 7,
            "source": "agent",
            "action": "read",
            "args": {"path": "/workspace/file", "view_range": [1, 2], "thought": "inspect"},
        });
        let metadata = openhands_reconstructed_tool_metadata(&event).unwrap();
        assert_eq!(metadata["function_name"], "str_replace_editor");
        assert_eq!(metadata["tool_call_id"], "sandbox-playback-replay-7");
        let arguments = metadata["model_response"]["choices"][0]["message"]["tool_calls"][0]
            ["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments: Value = serde_json::from_str(arguments).unwrap();
        assert_eq!(arguments["command"], "view");
        assert_eq!(arguments["path"], "/workspace/file");
    }

    #[test]
    fn openhands_signature_separates_visible_text_reasoning_and_tool_arguments() {
        let event = json!({
            "id": 7,
            "source": "agent",
            "action": "run",
            "args": {"command": "pwd", "thought": "legacy thought"},
            "tool_call_metadata": {
                "model_response": {
                    "choices": [{
                        "message": {
                            "content": "visible preamble",
                            "reasoning_content": "hidden reasoning"
                        }
                    }]
                }
            }
        });

        let signature = openhands_action_signature(&event);

        assert_eq!(signature["text"], "visible preamble");
        assert_eq!(signature["reasoning"], "hidden reasoning");
        assert_eq!(
            signature["tools"][0]["arguments"],
            json!({"command": "pwd"})
        );
    }

    #[test]
    fn openhands_complete_batches_preserve_fresh_observations() {
        let events = vec![
            json!({
                "id": 5,
                "source": "agent",
                "action": "run",
                "args": {"command": "pwd"},
            }),
            json!({
                "id": 6,
                "source": "environment",
                "observation": "run",
                "cause": 5,
                "message": "ok",
                "args": {"command": "pwd", "metadata": {"exit_code": 0}},
            }),
        ];
        let batches = openhands_complete_batches(&events).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0["id"], 5);
        assert_eq!(
            openhands_observation_content(batches[0].1),
            json!({
                "observation": "run",
                "message": "ok",
                "args": {"command": "pwd", "metadata": {"exit_code": 0}},
            })
        );
    }

    #[test]
    fn openhands_runtime_tools_are_prepended_to_path() {
        let runtime = tempfile::tempdir().unwrap();
        let bin = runtime.path().join("bin/openhands-python");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::create_dir(runtime.path().join("tools")).unwrap();
        let launch = LaunchSpec {
            entrypoint: bin,
            version: "0.53.0".into(),
            source: "explicit_entrypoint".into(),
            runtime_root: None,
        };
        let mut command = Command::new(&launch.entrypoint);
        prepend_openhands_runtime_tools(&mut command, &launch).unwrap();
        let path = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "PATH").then(|| value.expect("PATH value").to_os_string())
            })
            .expect("PATH override");
        let first = std::env::split_paths(&path).next().unwrap();
        assert_eq!(first, runtime.path().join("tools"));
    }

    #[test]
    fn openhands_zero_exit_controller_errors_are_detected_for_partial_results() {
        assert_eq!(
            openhands_fatal_controller_marker("Error while running the agent"),
            Some("Error while running the agent")
        );
        assert_eq!(
            openhands_fatal_controller_marker("Agent reached maximum iteration AgentState.ERROR"),
            None
        );
    }
}
