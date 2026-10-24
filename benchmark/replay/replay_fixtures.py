"""B-REPLAY: pinned synthetic native prefixes; no timing or sampling.

Fixture contracts preserved from benchmark/pvisor/v1/replay.py. This cohort
uses one fixed task (0) per adapter; it does not replace twenty-task fidelity.
"""

import json


def trajectory(agent, task, command):
    next_command = f"cat marker-{task}.txt"
    if agent == "swe-agent":
        steps = []
        history = [dict(role="user", content=f"fixture {task}")]
        for i, cmd in enumerate((command, next_command)):
            steps.append(
                dict(
                    action=cmd,
                    observation="historical observation",
                    thought=f"fixture-text-{task}-{i}",
                    state={},
                )
            )
            history.extend(
                [
                    dict(
                        role="assistant",
                        content=f"fixture-text-{task}-{i}",
                        tool_calls=[dict(id=f"call-{i}")],
                    ),
                    dict(role="tool", content="historical observation", tool_call_id=f"call-{i}"),
                ]
            )
        return dict(trajectory=steps, history=history, replay_config={}), False
    if agent == "claude-code":
        events = [
            {
                "type": "user",
                "uuid": "user-1",
                "parentUuid": None,
                "isSidechain": False,
                "sessionId": "session-1",
                "version": "2.1.220",
                "message": {"role": "user", "content": f"fixture {task}"},
            }
        ]
        parent = "user-1"
        for i, cmd in enumerate((command, next_command)):
            assistant = f"assistant-{i}"
            result = f"result-{i}"
            events.append(
                {
                    "type": "assistant",
                    "uuid": assistant,
                    "parentUuid": parent,
                    "isSidechain": False,
                    "sessionId": "session-1",
                    "version": "2.1.220",
                    "message": {
                        "id": f"message-{i}",
                        "role": "assistant",
                        "content": [
                            {"type": "text", "text": f"fixture-text-{task}-{i}"},
                            {
                                "type": "tool_use",
                                "id": f"tool-{i}",
                                "name": "Bash",
                                "input": {"command": cmd},
                            },
                        ],
                    },
                }
            )
            events.append(
                {
                    "type": "user",
                    "uuid": result,
                    "parentUuid": assistant,
                    "sourceToolAssistantUUID": assistant,
                    "isSidechain": False,
                    "sessionId": "session-1",
                    "version": "2.1.220",
                    "message": {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": f"tool-{i}",
                                "content": "historical observation",
                            }
                        ],
                    },
                }
            )
            parent = result
        return events, True
    if agent == "codex":
        events = [
            {"type": "session_meta", "payload": {"id": "sess-benchmark"}},
            {
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": f"fixture {task}"}],
                },
            },
        ]
        for i, cmd in enumerate((command, next_command)):
            events.append(
                {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": f"fixture-text-{task}-{i}"}],
                    },
                }
            )
            events += [
                {
                    "type": "response_item",
                    "payload": {
                        "type": "function_call",
                        "call_id": f"call-{i}",
                        "name": "exec_command",
                        "arguments": json.dumps({"cmd": cmd}),
                    },
                },
                {
                    "type": "response_item",
                    "payload": {
                        "type": "function_call_output",
                        "call_id": f"call-{i}",
                        "output": "historical observation",
                    },
                },
            ]
        return events, True
    if agent == "opencode":
        events = [
            {
                "type": "user",
                "sessionID": "ses-benchmark",
                "parts": [{"type": "text", "text": f"fixture {task}"}],
            }
        ]
        for i, cmd in enumerate((command, next_command)):
            events += [
                {"type": "step_start", "sessionID": "ses-benchmark"},
                {
                    "type": "tool_use",
                    "sessionID": "ses-benchmark",
                    "part": {
                        "type": "tool",
                        "tool": "bash",
                        "callID": f"call-{i}",
                        "state": {
                            "status": "completed",
                            "input": {"command": cmd},
                            "output": "historical observation",
                        },
                    },
                },
                {
                    "type": "step_finish",
                    "sessionID": "ses-benchmark",
                    "part": {"reason": "tool-calls"},
                },
            ]
        return events, True
    if agent == "mini-swe-agent":
        messages = [{"role": "user", "content": f"fixture {task}", "extra": {}}]
        for i, cmd in enumerate((command, next_command)):
            messages += [
                {
                    "role": "assistant",
                    "content": f"fixture-text-{task}-{i}",
                    "extra": {
                        "response": {},
                        "actions": [{"tool_call_id": f"call-{i}", "command": cmd}],
                    },
                },
                {"role": "tool", "content": "historical observation", "extra": {"returncode": 0}},
            ]
        return {
            "trajectory_format": "mini-swe-agent-1.1",
            "info": {
                "mini_version": "2.4.6",
                "config": {"model": {}, "agent": {}, "environment": {}},
            },
            "messages": messages,
        }, False
    if agent == "openhands":
        events = [
            {"id": 0, "source": "user", "action": "message", "args": {"content": f"fixture {task}"}}
        ]
        for i, cmd in enumerate((command, next_command)):
            events += [
                {
                    "id": 1 + i * 2,
                    "source": "agent",
                    "action": "run",
                    "args": {"command": cmd, "thought": f"fixture-text-{task}-{i}"},
                },
                {
                    "id": 2 + i * 2,
                    "source": "environment",
                    "observation": "run",
                    "cause": 1 + i * 2,
                    "message": "historical observation",
                    "args": {"command": cmd, "metadata": {"exit_code": 0}},
                },
            ]
        return events, False
    events = [
        {
            "type": "message_end",
            "message": {"role": "user", "content": f"fixture {task}", "timestamp": 1},
        }
    ]
    for i, cmd in enumerate((command, next_command)):
        events.append(
            {
                "type": "turn_end",
                "message": {
                    "role": "assistant",
                    "provider": "benchmark",
                    "model": "fixture",
                    "api": "openai-completions",
                    "content": [
                        {"type": "text", "text": f"fixture-text-{task}-{i}"},
                        {
                            "type": "toolCall",
                            "id": f"call-{i}",
                            "name": "bash",
                            "arguments": {"command": cmd},
                        },
                    ],
                    "stopReason": "toolUse",
                    "usage": {"input": 1, "output": 1},
                    "timestamp": 2 + i * 2,
                },
                "toolResults": [
                    {
                        "role": "toolResult",
                        "toolCallId": f"call-{i}",
                        "toolName": "bash",
                        "content": [{"type": "text", "text": "historical observation"}],
                        "isError": False,
                        "timestamp": 3 + i * 2,
                    }
                ],
            }
        )
    return events, True


def nested_strings(value):
    if isinstance(value, dict):
        return [s for child in value.values() for s in nested_strings(child)]
    if isinstance(value, list):
        return [s for child in value for s in nested_strings(child)]
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            if isinstance(parsed, (dict, list)):
                return [value, *nested_strings(parsed)]
        except (ValueError, TypeError):
            pass
        return [value]
    return []


def validate_prefix(manifest, output, agent, command, next_command, source_sha):
    boundary = manifest["boundary"]
    if (
        manifest["agent"]["name"] != agent
        or manifest["source"]["sha256"] != source_sha
        or boundary["after_step"] != 1
        or boundary["tool_calls"] != 1
        or boundary["complete_tool_batch"] is not True
    ):
        raise ValueError("wrong source, adapter or complete prefix boundary")
    batches = manifest["batches"]
    if len(batches) != 1 or len(batches[0]["tool_calls"]) != 1:
        raise ValueError("wrong prefix structure")
    call = batches[0]["tool_calls"][0]
    arguments = call["arguments"]
    if arguments.get("command", arguments.get("cmd", arguments.get("raw_action"))) != command:
        raise ValueError("prefix tool arguments differ")
    filename = {
        "mini-swe-agent": "prepared-prefix.json",
        "openhands": "prepared-replay-events.json",
        "swe-agent": "prepared-prefix.traj",
    }.get(agent, "prepared-prefix.jsonl")
    raw = (output / "native" / filename).read_text()
    native = (
        [json.loads(line) for line in raw.splitlines() if line.strip()]
        if filename.endswith(".jsonl")
        else json.loads(raw)
    )
    strings = nested_strings(native)
    if command not in strings or "historical observation" not in strings or next_command in strings:
        raise ValueError(
            "prepared native prefix lost tool/observation or included action beyond boundary"
        )
