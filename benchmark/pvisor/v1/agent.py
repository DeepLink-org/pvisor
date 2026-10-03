"""Real CLI tool loops with deterministic, local model responses (no inference)."""

import json
import shutil
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .common import checked

COMMAND = "/usr/bin/python3 -c \"from pathlib import Path; Path('adder.py').write_text('def add(a, b):\\n    return a + b\\n')\" && /usr/bin/python3 grade.py"


class Model(BaseHTTPRequestHandler):
    def do_POST(self):
        value = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        with self.server.lock:
            self.server.requests.append({"path": self.path, "body": value})
        if "messages" in value:
            finished = any(
                m["role"] == "user"
                and isinstance(m.get("content"), list)
                and any(c.get("type") == "tool_result" for c in m["content"])
                for m in value["messages"]
            )
            tool = {
                "type": "tool_use",
                "id": "toolu_bench",
                "name": "Bash",
                "input": {
                    "command": COMMAND,
                    "description": "Repair fixture and execute grading tests",
                },
            }
            block = {"type": "text", "text": "BENCH_COMPLETE"} if finished else tool
            message = {
                "id": "msg_bench",
                "type": "message",
                "role": "assistant",
                "model": value["model"],
                "content": [block],
                "stop_reason": "end_turn" if finished else "tool_use",
                "stop_sequence": None,
                "usage": {"input_tokens": 64, "output_tokens": 16},
            }
            if value.get("stream"):
                initial = message | {"content": [], "stop_reason": None}
                start = block if finished else tool | {"input": {}}
                if finished:
                    start = block | {"text": ""}
                events = [
                    ("message_start", {"type": "message_start", "message": initial}),
                    (
                        "content_block_start",
                        {"type": "content_block_start", "index": 0, "content_block": start},
                    ),
                ]
                if finished:
                    events.append(
                        (
                            "content_block_delta",
                            {
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": {"type": "text_delta", "text": "BENCH_COMPLETE"},
                            },
                        )
                    )
                if not finished:
                    events.append(
                        (
                            "content_block_delta",
                            {
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": json.dumps(tool["input"]),
                                },
                            },
                        )
                    )
                events += [
                    ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                    (
                        "message_delta",
                        {
                            "type": "message_delta",
                            "delta": {"stop_reason": message["stop_reason"], "stop_sequence": None},
                            "usage": {"output_tokens": 16},
                        },
                    ),
                    ("message_stop", {"type": "message_stop"}),
                ]
                self.send_events(events)
            else:
                self.send_json(message)
        else:
            finished = any(
                item.get("type") == "function_call_output"
                for item in value.get("input", [])
                if isinstance(item, dict)
            )
            names = [t.get("name") for t in value.get("tools", []) if t.get("type") == "function"]
            name = "exec_command" if "exec_command" in names else "shell_command"
            arguments = {"cmd": COMMAND} if name == "exec_command" else {"command": COMMAND}
            item = (
                {
                    "type": "message",
                    "id": "msg_bench",
                    "role": "assistant",
                    "status": "completed",
                    "content": [
                        {"type": "output_text", "text": "BENCH_COMPLETE", "annotations": []}
                    ],
                }
                if finished
                else {
                    "type": "function_call",
                    "id": "fc_bench",
                    "call_id": "call_bench",
                    "name": name,
                    "arguments": json.dumps(arguments),
                    "status": "completed",
                }
            )
            response = {
                "id": "resp_bench",
                "object": "response",
                "created_at": int(time.time()),
                "status": "completed",
                "model": value["model"],
                "output": [item],
                "usage": {
                    "input_tokens": 64,
                    "output_tokens": 16,
                    "total_tokens": 80,
                    "input_tokens_details": {"cached_tokens": 0},
                    "output_tokens_details": {"reasoning_tokens": 0},
                },
            }
            events = [
                (
                    "response.created",
                    {
                        "type": "response.created",
                        "response": response | {"status": "in_progress", "output": []},
                    },
                ),
                (
                    "response.output_item.added",
                    {"type": "response.output_item.added", "output_index": 0, "item": item},
                ),
            ]
            if not finished:
                events.append(
                    (
                        "response.function_call_arguments.done",
                        {
                            "type": "response.function_call_arguments.done",
                            "item_id": "fc_bench",
                            "output_index": 0,
                            "arguments": item["arguments"],
                        },
                    )
                )
            events += [
                (
                    "response.output_item.done",
                    {"type": "response.output_item.done", "output_index": 0, "item": item},
                ),
                ("response.completed", {"type": "response.completed", "response": response}),
            ]
            self.send_events(events)

    def send_json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def send_events(self, events):
        data = "".join(
            f"event: {name}\ndata: {json.dumps(value)}\n\n" for name, value in events
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


def run(ctx):
    server = ThreadingHTTPServer(("127.0.0.1", 0), Model)
    server.lock = threading.Lock()
    server.requests = []
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    url = f"http://127.0.0.1:{server.server_port}"
    ctx.metadata["agent_protocol"] = {
        "model": "deterministic local response fixture",
        "real_inference": False,
        "tasks": 6,
        "repetitions": min(ctx.args.samples, 3),
        "tool_command": COMMAND,
        "billing_usd": 0,
    }
    ctx.metadata["agent_versions"] = {
        name: checked([name, "--version"]).stdout.strip() for name in ("claude", "codex")
    }
    try:
        for agent in ("claude", "codex"):
            for task in range(6):
                for trial in range(min(ctx.args.samples, 3)):
                    for backend in ("native", "staged"):
                        root = ctx.fresh(f"agent-{agent}-{task}-{backend}")
                        work = root / "workspace"
                        work.mkdir()
                        (work / "adder.py").write_text("def add(a, b):\n    return a - b\n")
                        cases = [(task + i, task - i) for i in range(8)] + [(-task, 7), (0, 0)]
                        (work / "grade.py").write_text(
                            f'from adder import add\nfor a,b in {cases!r}:\n    assert add(a,b)==a+b\nprint("GRADE_PASS")\n'
                        )
                        stage = root / "stage"
                        runs = root / "runs"
                        home = root / "home"
                        home.mkdir()
                        (home / "codex").mkdir()
                        env = {
                            "HOME": str(home),
                            "XDG_CONFIG_HOME": str(home / "config"),
                            "PVISOR_RUN_HOME": str(runs),
                            "ANTHROPIC_API_KEY": "benchmark-fake",
                            "ANTHROPIC_BASE_URL": url,
                            "BENCH_API_KEY": "benchmark-fake",
                            "CODEX_HOME": str(home / "codex"),
                            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                        }
                        if agent == "claude":
                            payload = [
                                shutil.which("claude"),
                                "--bare",
                                "--setting-sources",
                                "",
                                "--disable-slash-commands",
                                "--strict-mcp-config",
                                "--mcp-config",
                                '{"mcpServers":{}}',
                                "--no-session-persistence",
                                "--tools",
                                "Bash",
                                "--allowedTools",
                                "Bash",
                                "--model",
                                "claude-sonnet-4-6",
                                "--output-format",
                                "json",
                                "-p",
                                f"BENCH_TASK={task}; repair adder.py and run grade.py.",
                            ]
                        else:
                            payload = [
                                shutil.which("codex"),
                                "exec",
                                "--skip-git-repo-check",
                                "--ephemeral",
                                "--json",
                                "-s",
                                "workspace-write",
                                "-m",
                                "benchmark-fixture",
                                "-c",
                                'model_provider="benchmark"',
                                "-c",
                                f'model_providers.benchmark={{name="benchmark",base_url="{url}/v1",env_key="BENCH_API_KEY",wire_api="responses"}}',
                                f"BENCH_TASK={task}; repair adder.py and run grade.py.",
                            ]
                        argv = ctx.command(backend, work, stage, payload)
                        if backend != "native":
                            boundary = argv.index("--")
                            argv[boundary:boundary] = [
                                value
                                for key in (
                                    "ANTHROPIC_API_KEY",
                                    "ANTHROPIC_BASE_URL",
                                    "BENCH_API_KEY",
                                    "CODEX_HOME",
                                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                                )
                                for value in ("--pass-env", key)
                            ]
                        with server.lock:
                            server.requests = []
                        wall, stdout, _ = ctx.run(argv, cwd=work, env=env, timeout=120)
                        bundle = ctx.validate_bundle(backend, runs, stage)
                        if bundle:
                            stdout = bundle["run"]["output"]["stdout"]
                        target = stage / "upper" if backend == "staged" else work
                        checked(
                            ["/usr/bin/python3", "grade.py"],
                            cwd=target if backend == "native" else work,
                            env=ctx.env,
                        ) if backend == "native" else None
                        assert (
                            target / "adder.py"
                        ).read_text() == "def add(a, b):\n    return a + b\n"
                        if backend == "staged":
                            assert (
                                work / "adder.py"
                            ).read_text() == "def add(a, b):\n    return a - b\n"
                        if "BENCH_COMPLETE" not in stdout:
                            (root / "model-requests.json").write_text(
                                json.dumps(server.requests, indent=2)
                            )
                        assert "BENCH_COMPLETE" in stdout
                        with server.lock:
                            requests = list(server.requests)
                        (root / "model-requests.json").write_text(json.dumps(requests, indent=2))
                        assert len(requests) >= 2
                        assert "GRADE_PASS" in json.dumps(requests), (
                            "fresh graded tool observation must return to the CLI model loop"
                        )
                        ctx.record(
                            dict(
                                suite="agent",
                                workload="arithmetic-repair",
                                agent=agent,
                                backend=backend,
                                task=task,
                                trial=trial,
                                wall_ms=wall,
                                requests=len(requests),
                                synthetic_usage_per_response={
                                    "input_tokens": 64,
                                    "output_tokens": 16,
                                },
                                task_grade="passed",
                                correctness="passed",
                                logs=str(root),
                            )
                        )
                        print(f"agent {agent} task={task} {backend}: {wall:.1f} ms", flush=True)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
