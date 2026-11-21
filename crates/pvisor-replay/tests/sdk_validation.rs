#![cfg(unix)]

use pvisor_replay::api::{
    AgentKind, AgentProfile, AgentStatus, PlaybackRequest, ReplayErrorKind, ReplayMode,
    ReplayPhase, ReplayRequest,
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn executable(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn request(root: &Path, agent: AgentKind, scenario: &str) -> PlaybackRequest {
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("scenario"), scenario).unwrap();
    let trajectory = root.join("source");
    let value = match agent {
        AgentKind::MiniSweAgent => json!({
            "trajectory_format": "mini-swe-agent-1.1", "info": {"mini_version": "2.4.6"},
            "messages": [
                {"role": "user", "content": "task"},
                {"role": "assistant", "extra": {"response": {}, "actions": [{"command": "touch must-not-run"}]}},
                {"role": "tool", "content": "old"}
            ]
        }).to_string(),
        AgentKind::SweAgent => json!({
            "trajectory": [{"action": "touch must-not-run", "observation": "old"}],
            "history": [{"role": "user"}, {"role": "assistant"}, {"role": "tool"}],
            "replay_config": {}
        }).to_string(),
        AgentKind::PiAgent => [
            json!({"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "task"}]}}),
            json!({"type": "turn_end", "message": {"role": "assistant", "content": [{
                "type": "toolCall", "id": "call-1", "name": "bash", "arguments": {"command": "touch must-not-run"}
            }]}, "toolResults": [{"toolCallId": "call-1", "toolName": "bash", "content": "old", "isError": false}]}),
        ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"),
        _ => unreachable!(),
    };
    fs::write(&trajectory, value).unwrap();
    let runtime = root.join("runtime");
    let program = match agent {
        AgentKind::MiniSweAgent => runtime.join("bin/python3"),
        AgentKind::SweAgent => runtime.join("python-fixture"),
        AgentKind::PiAgent => runtime.join("node/bin/node"),
        _ => unreachable!(),
    };
    executable(&program, &r#"#!/usr/bin/env python3
import json, pathlib, signal, sys
signal.alarm(5)
if '-c' in sys.argv or '--version' in sys.argv:
    print('__VERSION__')
    sys.exit(0)
request = json.loads(pathlib.Path(sys.argv[-1]).read_text())
workspace = pathlib.Path(request['workspace'])
scenario = (workspace / 'scenario').read_text()
with (workspace / 'entered').open('a') as stream:
    stream.write('runner\n')
live = request['mode'] == 'replay_and_continue'
data = json.loads(pathlib.Path(request.get('source', request.get('trajectory'))).read_text())
if isinstance(data, list):
    data[-1]['toolResults'][0]['content'] = 'fresh'
    if live:
        data.append({'type': 'turn_end', 'message': {'role': 'assistant', 'content': [{'type': 'text', 'text': 'continued'}]}})
    encoded = '\n'.join(json.dumps(event) for event in data) + '\n'
elif 'messages' in data:
    data['messages'][-1]['content'] = 'fresh'
    if live:
        data['messages'].append({'role': 'assistant', 'extra': {'actions': [{'command': 'continued'}]}})
    encoded = json.dumps(data)
else:
    data['trajectory'][0]['observation'] = 'fresh'
    if live:
        data['trajectory'].append({'action': 'continued', 'observation': 'fresh'})
    encoded = json.dumps(data)
target = request['continued'] if live else request['reconstructed']
pathlib.Path(request['reconstructed']).write_text(encoded)
pathlib.Path(target).write_text(encoded)
if 'observations' in request and scenario != 'missing-observations':
    pathlib.Path(request['observations']).write_text(json.dumps([{'content': 'fresh', 'is_error': False}]))
count = 1 if live else 0
if scenario == 'overflow':
    count = 18446744073709551615
elif scenario == 'budget':
    count = 2
elif scenario == 'mismatch':
    count = 0
elif scenario == 'invalid-count':
    count = -1
pathlib.Path(request['result']).write_text(json.dumps({
    'phase': 'continued' if live else 'replayed',
    'replayed_steps': request['after_step'],
    'continued_steps': count,
    'agent_status': 'completed' if live else 'not_started',
    'boundary_user_prompt_injected': False,
    'trajectory': target,
}))
"#.replace("__VERSION__", agent.supported_version()));
    let entry = match agent {
        AgentKind::SweAgent => program,
        _ => {
            let entry = runtime.join(format!("bin/{}", agent.as_str()));
            let banner = if agent == AgentKind::MiniSweAgent {
                format!(
                    "This is mini-swe-agent version {}.",
                    agent.supported_version()
                )
            } else {
                agent.supported_version().to_owned()
            };
            executable(&entry, &format!("#!/bin/sh\nprintf '{banner}\\n'\n"));
            if agent == AgentKind::PiAgent {
                let package =
                    runtime.join("npm-global/lib/node_modules/@earendil-works/pi-coding-agent");
                fs::create_dir_all(&package).unwrap();
                fs::write(package.join("package.json"), "{}").unwrap();
            }
            entry
        }
    };
    PlaybackRequest {
        agent,
        trajectory,
        after_step: 1,
        workspace,
        state_dir: root.join("state"),
        output_dir: root.join("output"),
        agent_entrypoint: Some(entry),
        agent_runtime: None,
        disallowed_tools: vec![],
        trajectory_assets: None,
        session_id: None,
        max_steps: Some(2),
        mode: if scenario == "replay" {
            ReplayMode::ReplayOnly
        } else {
            ReplayMode::ReplayAndContinue
        },
        allow_stale_observations: false,
        run_id: Some("sdk-guard".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

#[rstest::rstest]
#[case::replay("replay")]
#[case::continuation("continue")]
#[case::overflow("overflow")]
#[case::budget("budget")]
#[case::mismatched_count("mismatch")]
#[case::invalid_count("invalid-count")]
fn sdk_results_preserve_validation_and_uncertain_execution_fences(
    #[case] scenario: &str,
    #[values(AgentKind::MiniSweAgent, AgentKind::SweAgent, AgentKind::PiAgent)] agent: AgentKind,
) {
    assert_sdk_result(agent, scenario);
}

#[rstest::rstest]
fn missing_separate_observations_remain_ambiguous(
    #[values(AgentKind::MiniSweAgent, AgentKind::PiAgent)] agent: AgentKind,
) {
    assert_sdk_result(agent, "missing-observations");
}

fn assert_sdk_result(agent: AgentKind, scenario: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path(), agent, scenario);
    let source = fs::read(&request.trajectory).unwrap();
    let report = request.clone().execute().unwrap();
    let journal =
        fs::read_to_string(request.state_dir.join("sdk-guard/replay-events.jsonl")).unwrap();
    assert!(journal.contains("continuation_started"));
    assert_eq!(
        fs::read_to_string(request.workspace.join("entered")).unwrap(),
        "runner\n"
    );
    assert!(!request.workspace.join("must-not-run").exists());
    assert_eq!(fs::read(&request.trajectory).unwrap(), source);
    if matches!(scenario, "replay" | "continue") {
        assert_eq!(report.exit_code, 0, "{:?}", report.result.failure);
        assert_eq!(
            report.result.phase,
            if scenario == "replay" {
                ReplayPhase::Replayed
            } else {
                ReplayPhase::Continued
            }
        );
        assert_eq!(
            report.result.agent_status,
            if scenario == "replay" {
                AgentStatus::NotStarted
            } else {
                AgentStatus::Completed
            }
        );
        assert_eq!(
            report.result.continued_steps,
            usize::from(scenario == "continue")
        );
        assert_eq!(
            report.result.metadata["boundary_user_prompt"],
            json!({"requested": false, "injected": false, "injection_count": 0})
        );
        assert!(journal.contains("continuation_finished"));
    } else {
        assert_eq!(report.exit_code, 31);
        assert!(!report.result.retryable);
        let failure = report.result.failure.unwrap();
        assert_eq!(failure.category, "ambiguous_execution");
        let expected = match scenario {
            "overflow" if usize::BITS < 64 => "continued_steps exceeds platform capacity",
            "overflow" | "budget" => "invalid terminal status or step count",
            "mismatch" => "runner result disagrees with its trajectory",
            "invalid-count" => "omitted continued_steps",
            "missing-observations" => "fresh-observations",
            _ => unreachable!(),
        };
        assert!(failure.message.contains(expected), "{}", failure.message);
        assert!(!journal.contains("continuation_finished"));
        request.output_dir = root.path().join("output-again");
        let error = request.clone().execute().unwrap_err();
        assert_eq!(error.kind, ReplayErrorKind::AmbiguousExecution);
        assert!(!request.output_dir.join("sdk-guard").exists());
        assert_eq!(
            fs::read_to_string(request.workspace.join("entered")).unwrap(),
            "runner\n"
        );
    }
}
