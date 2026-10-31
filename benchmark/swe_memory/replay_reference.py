"""Fixed real SWE trace; local timed model responses, real VM tools.

Private engineering comparison. Does not change runtime or policy code.
"""

import copy
import hashlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def read_lines(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line]


def commands(messages):
    return [
        json.loads(t["function"]["arguments"])["command"]
        for m in messages
        if m["role"] == "assistant"
        for t in (m.get("tool_calls") or [])
    ]


def make_plan(trajectory, events):
    messages = trajectory["messages"]
    assistants = [m for m in messages if m["role"] == "assistant"]
    spans, start = [], None
    for e in events:
        if e["event"] == "model_request_start":
            assert start is None
            start = e["host_monotonic_ns"]
        elif e["event"] == "model_request_error":
            raise ValueError("Select an error-free trajectory for this first replay")
        elif e["event"] == "model_request_end":
            assert start is not None
            spans.append((e["host_monotonic_ns"] - start) / 1e9)
            start = None
    assert start is None and len(spans) == len(assistants) and spans
    assert trajectory["info"]["exit_status"] == "Submitted"
    plan = []
    for i, (message, wait) in enumerate(zip(assistants, spans)):
        response = copy.deepcopy(message["extra"]["response"])
        assert response["object"] == "chat.completion"
        assert commands([response["choices"][0]["message"]]) == commands([message])
        # Source client has a 120-second timeout; preserve successful 115-second NodeBB wait.
        assert 0 <= wait < 120
        plan.append(dict(index=i + 1, wait_s=wait, response=response, commands=commands([message])))
    assert len(plan[-1]["commands"]) == 1
    assert plan[-1]["commands"][0] == "echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT"
    return plan


class ReplayState:
    def __init__(self, plan, log):
        self.plan, self.log = plan, Path(log)
        self.next = 0
        self.pending = False
        self.errors = []
        self.lock = threading.Lock()

    def append(self, row):
        with self.log.open("a") as f:
            f.write(json.dumps(row) + "\n")

    def reserve(self, body):
        with self.lock:
            if self.pending or self.next >= len(self.plan):
                raise ValueError("concurrent, duplicate or extra model request")
            prior = [c for row in self.plan[: self.next] for c in row["commands"]]
            if commands(body["messages"]) != prior:
                raise ValueError("tool command history differs from fixed trace")
            if body.get("stream", False):
                raise ValueError("source trace is non-streaming")
            if body["model"] != self.plan[self.next]["response"]["model"]:
                raise ValueError("unexpected model routing")
            self.pending = True
            return self.plan[self.next]


def make_server(state):
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            received = time.monotonic_ns()
            record = dict(received_monotonic_ns=received, path=self.path)
            try:
                if self.path != "/v1/chat/completions":
                    raise ValueError("unexpected path")
                size = int(self.headers["Content-Length"])
                if size > 32 * 1024 * 1024:
                    raise ValueError("request exceeds trace fixture limit")
                body = json.loads(self.rfile.read(size))
                step = state.reserve(body)
                record.update(index=step["index"], target_wait_s=step["wait_s"])
                raw = json.dumps(step["response"]).encode()
                record["wait_start_monotonic_ns"] = time.monotonic_ns()
                deadline = record["wait_start_monotonic_ns"] + round(step["wait_s"] * 1e9)
                while True:
                    remaining = (deadline - time.monotonic_ns()) / 1e9
                    if remaining <= 0:
                        break
                    time.sleep(remaining)
                record["body_ready_monotonic_ns"] = time.monotonic_ns()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
                self.wfile.flush()
                record["write_done_monotonic_ns"] = time.monotonic_ns()
                record["status"] = "delivered"
                with state.lock:
                    state.next += 1
                    state.pending = False
            except Exception as exc:
                record.update(status="failed", error_type=type(exc).__name__, error=str(exc))
                state.errors.append(record)
                try:
                    raw = json.dumps(
                        {"error": {"message": str(exc), "type": "replay_mismatch"}}
                    ).encode()
                    self.send_response(409)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except OSError:
                    pass
            finally:
                state.append(record)

        def log_message(self, *args):
            pass

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)


def audit_trial(trial, original, plan, state):
    trajectory = json.loads((trial / "agent/mini-swe-agent.trajectory.json").read_text())
    original_trajectory = json.loads(
        (original / "agent/mini-swe-agent.trajectory.json").read_text()
    )
    actual = commands(trajectory["messages"])
    expected = [c for step in plan for c in step["commands"]]

    def tool_codes(t):
        return [m["extra"]["returncode"] for m in t["messages"] if m["role"] == "tool"]

    result = json.loads((trial / "result.json").read_text())
    check = dict(
        model_calls=state.next,
        fixture_errors=state.errors,
        commands_match=actual == expected,
        tool_return_codes_match=tool_codes(trajectory) == tool_codes(original_trajectory),
        original_tool_return_codes=tool_codes(original_trajectory),
        replay_tool_return_codes=tool_codes(trajectory),
        patch_sha256=sha(trial / "agent.patch"),
        original_patch_sha256=sha(original / "agent.patch"),
        agent_exit_status=trajectory["info"]["exit_status"],
        reward=(result.get("verifier_result") or {}).get("rewards"),
        exception=result.get("exception_info"),
    )
    check["patch_matches"] = check["patch_sha256"] == check["original_patch_sha256"]
    check["passed"] = (
        state.next == len(plan)
        and not state.errors
        and check["commands_match"]
        and check["tool_return_codes_match"]
        and check["patch_matches"]
        and check["agent_exit_status"] == "Submitted"
        and check["exception"] is None
        and check["reward"] == {"reward": 1}
    )
    return check
