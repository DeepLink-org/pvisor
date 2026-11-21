use crate::api::AgentProfile;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod claude_code;
mod generic;
mod mini_swe_agent;
mod openhands;
mod pi_agent;
mod runtime;
mod swe_agent;

use serde_json::{Map, Value, json};

use crate::api::{AgentKind, PlaybackRequest, ReplayMode};
use crate::api::{ReplayError, ReplayErrorKind};
use crate::error::ResultExt;
use crate::io::{atomic_write, atomic_write_json, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{FreshObservation, ReplayOutcome, ReplayPlan};
use crate::process::{ProcessSpec, run_process};
pub(crate) use runtime::{LaunchSpec, resolve_launch_spec};
use runtime::{
    configure_mini_python_environment, mini_python_library_path, mini_python_runtime,
    pi_node_runtime,
};

const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct RunContext<'a> {
    pub(crate) request: &'a PlaybackRequest,
    pub(crate) state_dir: &'a Path,
    pub(crate) output_dir: &'a Path,
    pub(crate) launch: Option<&'a LaunchSpec>,
    pub(crate) session_id: &'a str,
    pub(crate) nonce: &'a str,
}

pub(crate) fn build_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    match request.agent {
        AgentKind::ClaudeCode => claude_code::build(request),
        AgentKind::Codex => generic::build(request, generic::NativeJsonlAgent::Codex),
        AgentKind::MiniSweAgent => mini_swe_agent::build(request),
        AgentKind::Openhands => openhands::build(request),
        AgentKind::Opencode => generic::build(request, generic::NativeJsonlAgent::Opencode),
        AgentKind::PiAgent => pi_agent::build(request),
        AgentKind::SweAgent => swe_agent::build(request),
    }
}

pub(crate) fn run(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    match plan.agent {
        AgentKind::ClaudeCode => claude_code::execute(plan, context, journal),
        AgentKind::Codex => {
            generic::execute(plan, context, journal, generic::NativeJsonlAgent::Codex)
        }
        AgentKind::MiniSweAgent => mini_swe_agent::execute(plan, context, journal),
        AgentKind::Openhands => openhands::execute(plan, context, journal),
        AgentKind::Opencode => {
            generic::execute(plan, context, journal, generic::NativeJsonlAgent::Opencode)
        }
        AgentKind::PiAgent => pi_agent::execute(plan, context, journal),
        AgentKind::SweAgent => swe_agent::execute(plan, context, journal),
    }
}

fn check_boundary(after_step: usize, complete: usize) -> Result<(), ReplayError> {
    if after_step == 0 || after_step > complete {
        return Err(ReplayError::trajectory(format!(
            "requested after-step {after_step}, trajectory has {complete} complete batches"
        )));
    }
    Ok(())
}

fn prepared_outcome(path: PathBuf, request: &PlaybackRequest) -> ReplayOutcome {
    ReplayOutcome {
        status: "prepared".into(),
        reconstructed_path: Some(path),
        continued_path: None,
        observations: Vec::new(),
        continued_steps: 0,
        metadata: with_boundary_user_prompt_metadata(
            Map::from_iter([("replay_only_execution".into(), json!(false))]),
            request,
            false,
        ),
    }
}

pub(super) fn with_boundary_user_prompt_metadata(
    mut metadata: Map<String, Value>,
    request: &PlaybackRequest,
    injected: bool,
) -> Value {
    let prompt = request.boundary_user_prompt();
    let mut detail = json!({
        "requested": prompt.is_some(),
        "injected": injected,
        "injection_count": usize::from(injected),
    });
    if let Some(prompt) = prompt {
        detail["sha256"] = json!(sha256(prompt.as_bytes()));
        detail["length"] = json!(prompt.chars().count());
        if injected {
            detail["position"] = json!("after_boundary_observation");
        } else {
            detail["reason"] = json!(match request.mode {
                ReplayMode::PrepareOnly => "prepare_only",
                ReplayMode::ReplayOnly => "replay_only",
                ReplayMode::ReplayAndContinue => "not_injected",
            });
        }
    }
    metadata.insert("boundary_user_prompt".into(), detail);
    Value::Object(metadata)
}

fn valid_sdk_terminal_status(
    mode: ReplayMode,
    status: &str,
    continued_steps: usize,
    prefix_model_turns: usize,
    max_steps: Option<usize>,
) -> bool {
    match mode {
        ReplayMode::ReplayOnly => status == "not_started" && continued_steps == 0,
        ReplayMode::ReplayAndContinue => {
            matches!(status, "completed" | "max_steps")
                && prefix_model_turns
                    .checked_add(continued_steps)
                    .is_some_and(|total| max_steps.is_none_or(|limit| total <= limit))
        }
        ReplayMode::PrepareOnly => false,
    }
}

fn run_sdk_bridge(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    agent: AgentKind,
) -> Result<ReplayOutcome, ReplayError> {
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("SDK continuation has no launch spec"))?;
    let native_dir = context.output_dir.join("native");
    let logs_dir = context.output_dir.join("logs");
    fs::create_dir_all(&native_dir)
        .replay_context(ReplayErrorKind::Executor, "create native output directory")?;
    fs::create_dir_all(&logs_dir)
        .replay_context(ReplayErrorKind::Executor, "create Agent log directory")?;

    let runner_result = context
        .state_dir
        .join(format!("{}-runner-result.json", agent.as_str()));
    let mode = match context.request.mode {
        ReplayMode::ReplayOnly => "replay_only",
        ReplayMode::ReplayAndContinue => "replay_and_continue",
        ReplayMode::PrepareOnly => {
            return Err(ReplayError::new(
                ReplayErrorKind::Internal,
                "prepare-only unexpectedly started an SDK runner",
            ));
        }
    };
    let (
        program,
        bridge_source,
        bridge_name,
        request_value,
        reconstructed,
        continued,
        observations_path,
    ) = match agent {
        AgentKind::MiniSweAgent => {
            let source = context.state_dir.join("mini-source.json");
            let reconstructed = native_dir.join("reconstructed-trajectory.json");
            let continued = native_dir.join("continued-trajectory.json");
            let observations = context.state_dir.join("mini-fresh-observations.json");
            atomic_write_json(&source, &plan.native)?;
            let runtime = mini_python_runtime(&launch.entrypoint)?;
            let program = runtime
                .loader
                .clone()
                .unwrap_or_else(|| runtime.python.clone());
            (
                program,
                include_str!("../../assets/mini_swe_agent_runner.py"),
                "mini-swe-agent-runner.py",
                json!({
                    "source": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "observations": observations,
                    "result": runner_result,
                    "mode": mode,
                    "workspace": context.request.workspace,
                    "after_step": plan.after_step,
                    "max_steps": context.request.max_steps,
                    "session_id": context.session_id,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                }),
                reconstructed,
                continued,
                Some(observations),
            )
        }
        AgentKind::SweAgent => {
            let source = native_dir.join("continuation-source.traj");
            let run_output = native_dir.join("swe-agent-run");
            let reconstructed = native_dir.join("reconstructed-trajectory.traj");
            let continued = native_dir.join("continued-trajectory.traj");
            atomic_write_json(&source, &plan.native)?;
            (
                launch.entrypoint.clone(),
                include_str!("../../assets/swe_agent_runner.py"),
                "swe-agent-runner.py",
                json!({
                    "trajectory": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "trajectory_assets": context.request.trajectory_assets,
                    "after_step": plan.after_step,
                    "max_steps": context.request.max_steps,
                    "mode": mode,
                    "result": runner_result,
                    "workspace": context.request.workspace,
                    "output_dir": run_output,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                }),
                reconstructed,
                continued,
                None,
            )
        }
        AgentKind::PiAgent => {
            let source = context.state_dir.join("pi-source.json");
            let reconstructed = native_dir.join("reconstructed-events.jsonl");
            let continued = native_dir.join("continued-events.jsonl");
            let observations = context.state_dir.join("pi-fresh-observations.json");
            atomic_write_json(&source, &plan.native)?;
            let runtime = pi_node_runtime(&launch.entrypoint)?;
            (
                runtime.node,
                include_str!("../../assets/pi_agent_runner.mjs"),
                "pi-agent-runner.mjs",
                json!({
                    "source": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "observations": observations,
                    "result": runner_result,
                    "mode": mode,
                    "workspace": context.request.workspace,
                    "after_step": plan.after_step,
                    "prefix_model_turns": plan.prefix_model_turns,
                    "max_steps": context.request.max_steps,
                    "session_id": context.session_id,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                    "disable_thinking": context.request.disable_thinking,
                    "package_json": runtime.package_json,
                    "config_dir": context.state_dir.join("pi-config"),
                    "session_dir": context.state_dir.join("pi-sessions"),
                }),
                reconstructed,
                continued,
                Some(observations),
            )
        }
        _ => {
            return Err(ReplayError::new(
                ReplayErrorKind::Internal,
                "SDK bridge selected for a non-SDK agent",
            ));
        }
    };

    let bridge = context.state_dir.join(bridge_name);
    let request_path = context
        .state_dir
        .join(format!("{}-request.json", agent.as_str()));
    atomic_write(&bridge, bridge_source.as_bytes())?;
    atomic_write_json(&request_path, &request_value)?;
    let mut command = agent_command(&program, context);
    if agent == AgentKind::MiniSweAgent {
        let runtime = mini_python_runtime(&launch.entrypoint)?;
        if runtime.loader.is_some() {
            let library_path = mini_python_library_path(&runtime)?.ok_or_else(|| {
                ReplayError::continuation("bundled mini-swe-agent Python has no library path")
            })?;
            let argv0 = runtime
                .virtual_env
                .as_deref()
                .map(|venv| venv.join("bin/python"))
                .unwrap_or_else(|| runtime.python.clone());
            command
                .arg("--argv0")
                .arg(argv0)
                .arg("--library-path")
                .arg(library_path)
                .arg(&runtime.python);
        }
        configure_mini_python_environment(&mut command, &runtime)?;
        command.env("MSWEA_CONFIGURED", "true");
        command.env("MSWEA_COST_TRACKING", "ignore_errors");
        command.env("SWE_EVAL_MINI_RUNTIME", "1");
    }
    command.arg(&bridge).arg(&request_path);
    journal.append("continuation_started", std::iter::empty())?;
    let log = logs_dir.join(format!("{}.log", agent.as_str()));
    let output = run_process(ProcessSpec {
        command,
        stdin: None,
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
    if !output.status.success() {
        let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
        if !output.stderr_tail.is_empty() {
            rendered.push('\n');
            rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
        }
        return Err(ReplayError::classify_continuation(
            format!(
                "{} replay/continuation exited {}; see {}",
                agent.as_str(),
                output.status,
                log.display()
            ),
            &rendered,
        ));
    }

    let runner: Value = serde_json::from_slice(&read_regular_file(&runner_result)?)
        .replay_context(
            ReplayErrorKind::Continuation,
            "parse SDK replay runner result",
        )?;
    let expected_phase = if context.request.mode == ReplayMode::ReplayOnly {
        "replayed"
    } else {
        "continued"
    };
    if runner.get("phase").and_then(Value::as_str) != Some(expected_phase)
        || runner.get("replayed_steps").and_then(Value::as_u64) != Some(plan.after_step as u64)
    {
        return Err(ReplayError::continuation(format!(
            "{} runner returned an invalid replay boundary",
            agent.as_str()
        )));
    }
    let runner_continued_steps = runner
        .get("continued_steps")
        .and_then(Value::as_u64)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted continued_steps"))?;
    let runner_continued_steps = usize::try_from(runner_continued_steps).map_err(|_| {
        ReplayError::continuation("SDK runner continued_steps exceeds platform capacity")
    })?;
    let runner_agent_status = runner
        .get("agent_status")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted agent_status"))?;
    let prompt_injected = runner
        .get("boundary_user_prompt_injected")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            ReplayError::continuation("SDK runner omitted boundary_user_prompt_injected")
        })?;
    let expected_prompt_injected = context.request.mode == ReplayMode::ReplayAndContinue
        && context.request.boundary_user_prompt().is_some();
    if prompt_injected != expected_prompt_injected {
        return Err(ReplayError::continuation(format!(
            "{} runner reported an invalid boundary user prompt injection state",
            agent.as_str()
        )));
    }
    let status_is_valid = valid_sdk_terminal_status(
        context.request.mode,
        runner_agent_status,
        runner_continued_steps,
        plan.prefix_model_turns,
        context.request.max_steps,
    );
    if !status_is_valid {
        return Err(ReplayError::continuation(format!(
            "{} runner returned an invalid terminal status or step count",
            agent.as_str()
        )));
    }
    let runner_trajectory = runner
        .get("trajectory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted trajectory"))?;
    let expected_trajectory = if context.request.mode == ReplayMode::ReplayOnly {
        &reconstructed
    } else {
        &continued
    };
    if runner_trajectory != *expected_trajectory || !runner_trajectory.is_file() {
        return Err(ReplayError::continuation(format!(
            "{} runner produced an unexpected trajectory path",
            agent.as_str()
        )));
    }

    let (observations, continued_steps) = match observations_path {
        Some(observations_path) => {
            let raw_observations: Vec<Value> =
                serde_json::from_slice(&read_regular_file(&observations_path)?).replay_context(
                    ReplayErrorKind::Trajectory,
                    format!("parse {} fresh observations", agent.as_str()),
                )?;
            if raw_observations.len() != plan.calls().count() {
                return Err(ReplayError::trajectory(format!(
                    "{} output lost replayed observations",
                    agent.as_str()
                )));
            }
            let observations = plan
                .calls()
                .zip(raw_observations)
                .map(|(call, value)| FreshObservation {
                    call_id: call.call_id.clone(),
                    content: value.get("content").cloned().unwrap_or(Value::Null),
                    is_error: value
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    return_code: value
                        .get("return_code")
                        .and_then(Value::as_i64)
                        .map(|code| code as i32),
                    duration_ms: value
                        .get("duration_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or_default() as u128,
                    truncated: false,
                    metadata: BTreeMap::new(),
                })
                .collect::<Vec<_>>();
            let measured = if agent == AgentKind::MiniSweAgent {
                let continued_value: Value =
                    serde_json::from_slice(&read_regular_file(&runner_trajectory)?)
                        .replay_context(
                            ReplayErrorKind::Trajectory,
                            "parse continued mini-swe-agent trajectory",
                        )?;
                continued_value["messages"]
                    .as_array()
                    .map(|messages| {
                        messages
                            .iter()
                            .filter(|message| {
                                message
                                    .get("extra")
                                    .and_then(|extra| extra.get("actions"))
                                    .and_then(Value::as_array)
                                    .is_some_and(|actions| !actions.is_empty())
                            })
                            .count()
                    })
                    .unwrap_or_default()
                    .saturating_sub(plan.after_step)
            } else {
                let raw = read_regular_file(&runner_trajectory)?;
                let turns = String::from_utf8_lossy(&raw)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
                    .count();
                turns.saturating_sub(plan.prefix_model_turns)
            };
            if measured != runner_continued_steps {
                return Err(ReplayError::trajectory(format!(
                    "{} runner result disagrees with its trajectory",
                    agent.as_str()
                )));
            }
            (observations, measured)
        }
        None => {
            let replayed: Value = serde_json::from_slice(&read_regular_file(&runner_trajectory)?)
                .replay_context(
                ReplayErrorKind::Trajectory,
                "parse SWE-agent replay runner trajectory",
            )?;
            let steps = replayed["trajectory"].as_array().ok_or_else(|| {
                ReplayError::trajectory("continued SWE-agent trajectory is invalid")
            })?;
            if steps.len() < plan.after_step {
                return Err(ReplayError::trajectory(
                    "SWE-agent output lost replayed steps",
                ));
            }
            let observations = plan
                .calls()
                .zip(steps.iter())
                .map(|(call, step)| FreshObservation {
                    call_id: call.call_id.clone(),
                    content: step.get("observation").cloned().unwrap_or(Value::Null),
                    is_error: false,
                    return_code: None,
                    duration_ms: 0,
                    truncated: false,
                    metadata: BTreeMap::new(),
                })
                .collect::<Vec<_>>();
            let continued_steps = steps[plan.after_step..]
                .iter()
                .filter(|step| {
                    step.get("action")
                        .and_then(Value::as_str)
                        .is_some_and(|action| !action.trim().is_empty())
                })
                .count();
            if continued_steps != runner_continued_steps {
                return Err(ReplayError::trajectory(
                    "SWE-agent runner result disagrees with its trajectory",
                ));
            }
            (observations, continued_steps)
        }
    };
    if context.request.mode == ReplayMode::ReplayAndContinue && continued_steps == 0 {
        return Err(ReplayError::continuation(format!(
            "{} produced no actionable continuation step; see {}",
            agent.as_str(),
            log.display()
        )));
    }
    let comparisons: Vec<_> = plan
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
        .collect();
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )?;
    journal.append(
        "continuation_finished",
        [
            ("return_code".into(), json!(output.status.code())),
            ("continued_steps".into(), json!(continued_steps)),
        ],
    )?;
    Ok(ReplayOutcome {
        status: if context.request.mode == ReplayMode::ReplayOnly {
            "replayed".into()
        } else {
            runner_agent_status.into()
        },
        reconstructed_path: Some(reconstructed),
        continued_path: (context.request.mode == ReplayMode::ReplayAndContinue)
            .then_some(continued),
        observations,
        continued_steps,
        metadata: with_boundary_user_prompt_metadata(
            Map::from_iter([("sdk_bridge".into(), json!(bridge_name))]),
            context.request,
            prompt_injected,
        ),
    })
}

fn agent_command(entrypoint: &Path, context: &RunContext<'_>) -> Command {
    let mut command = Command::new(entrypoint);
    command.current_dir(&context.request.workspace);
    sanitized_environment(&mut command, context.request.agent == AgentKind::ClaudeCode);
    if context.request.agent != AgentKind::ClaudeCode {
        command.env("X_LITELLM_SESSION_ID", context.session_id);
        command.env(
            "LITELLM_EXTRA_HEADERS",
            json!({"X-LiteLLM-Session-ID": context.session_id}).to_string(),
        );
    }
    command
}

fn sanitized_environment(command: &mut Command, strip_credentials: bool) {
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let rendered = name.to_string_lossy().to_ascii_uppercase();
        if !environment_name_allowed(&rendered, strip_credentials) {
            continue;
        }
        command.env(name, value);
    }
}

fn environment_name_allowed(rendered: &str, strip_credentials: bool) -> bool {
    let credential = ["API_KEY", "TOKEN", "SECRET", "AUTHORIZATION", "PASSWORD"]
        .iter()
        .any(|fragment| rendered.contains(fragment));
    let claude_provider_override = strip_credentials
        && matches!(
            rendered,
            "CLAUDE_CODE_USE_BEDROCK" | "CLAUDE_CODE_USE_VERTEX" | "CLAUDE_CODE_USE_FOUNDRY"
        );
    !(claude_provider_override
        || matches!(rendered, "PYTHONHOME" | "PYTHONPATH" | "VIRTUAL_ENV")
        || strip_credentials && credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn metadata_request(mode: ReplayMode, prompt: Option<&str>) -> PlaybackRequest {
        PlaybackRequest {
            agent: AgentKind::SweAgent,
            trajectory: "unused".into(),
            after_step: 1,
            workspace: "unused".into(),
            state_dir: "unused".into(),
            output_dir: "unused".into(),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: vec![],
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode,
            allow_stale_observations: false,
            run_id: None,
            disable_thinking: false,
            boundary_user_prompt: prompt.map(str::to_owned),
        }
    }

    #[rstest::rstest]
    #[case::absent(None)]
    #[case::empty(Some(""))]
    #[case::unicode(Some("A😀中\n"))]
    #[case::spaces(Some("  "))]
    fn metadata_preserves_fields_and_non_injection_reasons(
        #[case] prompt: Option<&str>,
        #[values(
            ReplayMode::PrepareOnly,
            ReplayMode::ReplayOnly,
            ReplayMode::ReplayAndContinue
        )]
        mode: ReplayMode,
    ) {
        let request = metadata_request(mode, prompt);
        let retained = json!({"nested": [null, true, "unchanged"]});
        let result = with_boundary_user_prompt_metadata(
            Map::from_iter([
                ("retained".into(), retained.clone()),
                ("boundary_user_prompt".into(), json!("stale")),
            ]),
            &request,
            false,
        );
        assert_eq!(result["retained"], retained);
        let mut expected = json!({
            "requested": prompt.is_some_and(|prompt| !prompt.is_empty()),
            "injected": false,
            "injection_count": 0,
        });
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
            expected["sha256"] = json!(sha256(prompt.as_bytes()));
            expected["length"] = json!(if prompt == "A😀中\n" { 4 } else { 2 });
            expected["reason"] = json!(match mode {
                ReplayMode::PrepareOnly => "prepare_only",
                ReplayMode::ReplayOnly => "replay_only",
                ReplayMode::ReplayAndContinue => "not_injected",
            });
        }
        assert_eq!(result["boundary_user_prompt"], expected);
    }

    #[rstest::rstest]
    fn injected_prompt_metadata_contains_only_the_digest_and_position() {
        let request = metadata_request(ReplayMode::ReplayAndContinue, Some("A😀中\n"));
        let result = with_boundary_user_prompt_metadata(Map::new(), &request, true);
        assert_eq!(
            result["boundary_user_prompt"],
            json!({
                "requested": true, "injected": true, "injection_count": 1,
                "length": 4, "sha256": sha256("A😀中\n".as_bytes()),
                "position": "after_boundary_observation"
            })
        );
    }

    #[rstest::rstest]
    #[case::replay(ReplayMode::ReplayOnly, "not_started", 0, 1, None, true)]
    #[case::replay_wrong_status(ReplayMode::ReplayOnly, "completed", 0, 1, None, false)]
    #[case::replay_continued(ReplayMode::ReplayOnly, "not_started", 1, 1, None, false)]
    #[case::continued(ReplayMode::ReplayAndContinue, "completed", 1, 1, Some(2), true)]
    #[case::at_limit(ReplayMode::ReplayAndContinue, "max_steps", 1, 1, Some(2), true)]
    #[case::over_limit(ReplayMode::ReplayAndContinue, "completed", 2, 1, Some(2), false)]
    #[case::unbounded(ReplayMode::ReplayAndContinue, "completed", 3, 1, None, true)]
    #[case::overflow_unbounded(
        ReplayMode::ReplayAndContinue,
        "completed",
        usize::MAX,
        1,
        None,
        false
    )]
    #[case::overflow_bounded(
        ReplayMode::ReplayAndContinue,
        "completed",
        usize::MAX,
        1,
        Some(usize::MAX),
        false
    )]
    #[case::maximum(ReplayMode::ReplayAndContinue, "completed", usize::MAX - 1, 1, Some(usize::MAX), true)]
    #[case::unknown_status(ReplayMode::ReplayAndContinue, "failed", 1, 1, None, false)]
    #[case::prepare(ReplayMode::PrepareOnly, "not_started", 0, 1, None, false)]
    fn sdk_terminal_status_keeps_exact_step_boundaries(
        #[case] mode: ReplayMode,
        #[case] status: &str,
        #[case] continued: usize,
        #[case] prefix: usize,
        #[case] limit: Option<usize>,
        #[case] expected: bool,
    ) {
        assert_eq!(
            valid_sdk_terminal_status(mode, status, continued, prefix, limit),
            expected
        );
    }

    proptest! {
        #[test]
        fn sdk_total_budget_matches_widened_integer_arithmetic(
            prefix in any::<usize>(),
            continued in any::<usize>(),
            limit in prop::option::of(any::<usize>()),
        ) {
            let total = prefix as u128 + continued as u128;
            let expected = total <= usize::MAX as u128
                && limit.is_none_or(|limit| total <= limit as u128);
            prop_assert_eq!(
                valid_sdk_terminal_status(ReplayMode::ReplayAndContinue, "completed", continued, prefix, limit),
                expected
            );
        }

        #[test]
        fn metadata_preserves_generated_payloads_without_persisting_prompt_text(
            payload in prop::collection::vec(any::<char>(), 0..33),
            prompt in prop::option::of(prop::collection::vec(any::<char>(), 0..33)),
            injected in any::<bool>(),
        ) {
            let prompt = prompt.map(|chars| chars.into_iter().collect::<String>());
            let payload: String = payload.into_iter().collect();
            let request = metadata_request(ReplayMode::ReplayAndContinue, prompt.as_deref());
            let present = prompt.as_ref().is_some_and(|prompt| !prompt.is_empty());
            let injected = injected && present;
            let result = with_boundary_user_prompt_metadata(
                Map::from_iter([("opaque".into(), json!({"nested": [payload]}))]),
                &request, injected,
            );
            prop_assert_eq!(&result["opaque"], &json!({"nested": [payload]}));
            let detail = &result["boundary_user_prompt"];
            prop_assert_eq!(&detail["requested"], &json!(present));
            prop_assert_eq!(&detail["injected"], &json!(injected));
            prop_assert_eq!(&detail["injection_count"], &json!(usize::from(injected)));
            prop_assert!(detail.get("text").is_none() && detail.get("prompt").is_none());
            if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
                prop_assert_eq!(&detail["sha256"], &json!(sha256(prompt.as_bytes())));
                prop_assert_eq!(&detail["length"], &json!(prompt.chars().count()));
            } else {
                prop_assert!(detail.get("sha256").is_none());
            }
        }
    }

    #[test]
    fn direct_agents_keep_model_credentials_but_claude_tools_do_not() {
        assert!(environment_name_allowed("OPENAI_API_KEY", false));
        assert!(environment_name_allowed("LLM_API_KEY", false));
        assert!(environment_name_allowed("OPENAI_BASE_URL", false));
        assert!(!environment_name_allowed("OPENAI_API_KEY", true));
        assert!(!environment_name_allowed("ANTHROPIC_AUTH_TOKEN", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_BEDROCK", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_VERTEX", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_FOUNDRY", true));
        assert!(!environment_name_allowed("PYTHONPATH", false));
    }
}
