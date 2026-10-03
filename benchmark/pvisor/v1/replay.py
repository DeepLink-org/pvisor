"""Twenty synthetic native-format prefixes per adapter, three fresh repetitions."""

import json
import shutil

from .common import digest


def trajectory(agent, task, command):
    next_command = f"cat marker-{task}.txt"
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


def run(ctx):
    binary = ctx.output / "bin/pvisor-replay"
    shutil.copy2(ctx.args.replay_binary.resolve(), binary)
    ctx.metadata["replay_binary_sha256"] = digest(binary)
    ctx.metadata["replay_protocol"] = {
        "model": "none; no model requests",
        "tasks": 20,
        "repetitions": min(ctx.args.samples, 3),
        "mode": "prepare-only",
        "fixtures": "synthetic; not model task success",
    }
    for agent in ("claude-code", "codex", "opencode", "mini-swe-agent", "openhands", "pi-agent"):
        for task in range(20):
            command = f"printf fixture-{task} > marker-{task}.txt"
            source, jsonl = trajectory(agent, task, command)
            for trial in range(min(ctx.args.samples, 3)):
                root = ctx.fresh(f"replay-{agent}-{task}")
                work = root / "workspace"
                work.mkdir()
                path = root / ("trajectory.jsonl" if jsonl else "trajectory.json")
                path.write_text(
                    (
                        "\n".join(json.dumps(item) for item in source)
                        if jsonl
                        else json.dumps(source)
                    )
                    + "\n"
                )
                argv = [
                    str(binary),
                    "--agent",
                    agent,
                    "--trajectory",
                    str(path),
                    "--after-step",
                    "1",
                    "--prepare-only",
                    "--workspace",
                    str(work),
                    "--state-dir",
                    str(root / "state"),
                    "--output-dir",
                    str(root / "output"),
                ]
                wall, _, _ = ctx.run(argv, cwd=work)
                results = list((root / "output").glob("*/result.json"))
                assert len(results) == 1
                result = json.loads(results[0].read_text())
                manifest = json.loads(results[0].with_name("manifest.json").read_text())
                assert result["failure"] is None and result["replayed_tool_calls"] == 0
                assert (
                    manifest["boundary"]["after_step"] == 1
                    and manifest["boundary"]["tool_calls"] == 1
                )
                call = manifest["batches"][0]["tool_calls"][0]
                assert call["arguments"].get("command", call["arguments"].get("cmd")) == command
                assert not list(work.iterdir()), "prepare-only executed an operation"
                ctx.record(
                    dict(
                        suite="replay",
                        workload="prepare-only",
                        agent=agent,
                        profile=manifest["agent"]["profile"],
                        task=task,
                        trial=trial,
                        wall_ms=wall,
                        prefix_arguments_exact=True,
                        executed_tools=0,
                        correctness="passed",
                        logs=str(root),
                    )
                )
        print(f"replay {agent}: 20 prefixes prepared", flush=True)
