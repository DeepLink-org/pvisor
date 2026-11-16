//! B-REPLAY task-0 fixtures, ported from replay_fixtures.py without a Python runner.

use serde_json::{Value, json};

pub(super) const AGENTS: [&str; 7] = [
    "claude-code",
    "codex",
    "opencode",
    "mini-swe-agent",
    "openhands",
    "pi-agent",
    "swe-agent",
];
pub(super) const COMMAND: &str = "printf fixture-0 > marker-0.txt";
pub(super) const NEXT_COMMAND: &str = "cat marker-0.txt";
pub(super) const OBSERVATION: &str = "historical observation";
pub(super) const SENTINEL: &str = "existing workspace must survive preparation\n";

/// Return the original native document and whether its on-disk format is JSONL.
pub(super) fn trajectory(agent: &str) -> (Value, bool) {
    let mut events = match agent {
        "claude-code" => vec![json!({
            "type":"user", "uuid":"user-1", "parentUuid":null, "isSidechain":false,
            "sessionId":"session-1", "version":"2.1.220",
            "message":{"role":"user", "content":"fixture 0"}
        })],
        "codex" => vec![
            json!({"type":"session_meta", "payload":{"id":"sess-benchmark"}}),
            json!({"type":"response_item", "payload":{"type":"message", "role":"user",
                "content":[{"type":"input_text", "text":"fixture 0"}]}}),
        ],
        "opencode" => vec![json!({"type":"user", "sessionID":"ses-benchmark",
            "parts":[{"type":"text", "text":"fixture 0"}]})],
        "mini-swe-agent" => vec![json!({"role":"user", "content":"fixture 0", "extra":{}})],
        "openhands" => vec![json!({"id":0, "source":"user", "action":"message",
            "args":{"content":"fixture 0"}})],
        "pi-agent" => vec![json!({"type":"message_end",
            "message":{"role":"user", "content":"fixture 0", "timestamp":1}})],
        "swe-agent" => Vec::new(),
        _ => panic!("unknown B-REPLAY adapter: {agent}"),
    };
    let mut history = vec![json!({"role":"user", "content":"fixture 0"})];
    for (i, cmd) in [COMMAND, NEXT_COMMAND].iter().enumerate() {
        let text = format!("fixture-text-0-{i}");
        let call = format!("call-{i}");
        match agent {
            "claude-code" => {
                let assistant = format!("assistant-{i}");
                let result = format!("result-{i}");
                let parent = if i == 0 { "user-1".into() } else { format!("result-{}", i - 1) };
                events.push(json!({
                    "type":"assistant", "uuid":assistant, "parentUuid":parent,
                    "isSidechain":false, "sessionId":"session-1", "version":"2.1.220",
                    "message":{"id":format!("message-{i}"), "role":"assistant", "content":[
                        {"type":"text", "text":text},
                        {"type":"tool_use", "id":format!("tool-{i}"), "name":"Bash",
                         "input":{"command":cmd}}
                    ]}
                }));
                events.push(json!({
                    "type":"user", "uuid":result, "parentUuid":assistant,
                    "sourceToolAssistantUUID":assistant, "isSidechain":false,
                    "sessionId":"session-1", "version":"2.1.220",
                    "message":{"role":"user", "content":[{"type":"tool_result",
                        "tool_use_id":format!("tool-{i}"), "content":OBSERVATION}]}
                }));
            }
            "codex" => events.extend([
                json!({"type":"response_item", "payload":{"type":"message", "role":"assistant",
                    "content":[{"type":"output_text", "text":text}]}}),
                json!({"type":"response_item", "payload":{"type":"function_call", "call_id":call,
                    "name":"exec_command",
                    // Preserve the embedded JSON string, including Python's separator.
                    "arguments":format!("{{\"cmd\": {}}}", serde_json::to_string(cmd).unwrap())}}),
                json!({"type":"response_item", "payload":{"type":"function_call_output",
                    "call_id":call, "output":OBSERVATION}}),
            ]),
            "opencode" => events.extend([
                json!({"type":"step_start", "sessionID":"ses-benchmark"}),
                json!({"type":"tool_use", "sessionID":"ses-benchmark", "part":{
                    "type":"tool", "tool":"bash", "callID":call,
                    "state":{"status":"completed", "input":{"command":cmd}, "output":OBSERVATION}}}),
                json!({"type":"step_finish", "sessionID":"ses-benchmark",
                    "part":{"reason":"tool-calls"}}),
            ]),
            "mini-swe-agent" => events.extend([
                json!({"role":"assistant", "content":text, "extra":{"response":{},
                    "actions":[{"tool_call_id":call, "command":cmd}]}}),
                json!({"role":"tool", "content":OBSERVATION, "extra":{"returncode":0}}),
            ]),
            "openhands" => events.extend([
                json!({"id":1+i*2, "source":"agent", "action":"run",
                    "args":{"command":cmd, "thought":text}}),
                json!({"id":2+i*2, "source":"environment", "observation":"run",
                    "cause":1+i*2, "message":OBSERVATION,
                    "args":{"command":cmd, "metadata":{"exit_code":0}}}),
            ]),
            "pi-agent" => events.push(json!({
                "type":"turn_end", "message":{"role":"assistant", "provider":"benchmark",
                    "model":"fixture", "api":"openai-completions", "content":[
                        {"type":"text", "text":text},
                        {"type":"toolCall", "id":call, "name":"bash", "arguments":{"command":cmd}}
                    ], "stopReason":"toolUse", "usage":{"input":1, "output":1}, "timestamp":2+i*2},
                "toolResults":[{"role":"toolResult", "toolCallId":call, "toolName":"bash",
                    "content":[{"type":"text", "text":OBSERVATION}], "isError":false,
                    "timestamp":3+i*2}]
            })),
            "swe-agent" => {
                events.push(json!({"action":cmd, "observation":OBSERVATION, "thought":text, "state":{}}));
                history.extend([
                    json!({"role":"assistant", "content":text, "tool_calls":[{"id":call}]}),
                    json!({"role":"tool", "content":OBSERVATION, "tool_call_id":call}),
                ]);
            }
            _ => unreachable!(),
        }
    }
    match agent {
        "swe-agent" => (
            json!({"trajectory":events, "history":history, "replay_config":{}}),
            false,
        ),
        "mini-swe-agent" => (
            json!({
                "trajectory_format":"mini-swe-agent-1.1",
                "info":{"mini_version":"2.4.6", "config":{"model":{}, "agent":{}, "environment":{}}},
                "messages":events
            }),
            false,
        ),
        "openhands" => (json!(events), false),
        _ => (json!(events), true),
    }
}

pub(super) fn encoded(agent: &str) -> String {
    let (source, jsonl) = trajectory(agent);
    let mut text = if jsonl {
        source
            .as_array()
            .unwrap()
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        serde_json::to_string(&source).unwrap()
    };
    text.push('\n');
    text
}

/// Include JSON objects encoded inside strings (Codex's function arguments).
pub(super) fn nested_strings(value: &Value, strings: &mut Vec<String>) {
    match value {
        Value::Object(object) => object.values().for_each(|v| nested_strings(v, strings)),
        Value::Array(array) => array.iter().for_each(|v| nested_strings(v, strings)),
        Value::String(text) => {
            strings.push(text.clone());
            if let Ok(parsed @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(text) {
                nested_strings(&parsed, strings);
            }
        }
        _ => {}
    }
}
