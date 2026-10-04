#!/usr/bin/env python3
"""Identical complete development environment tasks for the reference matrix."""

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

ROOT = Path(os.environ.get("PVISOR_REFERENCE_TOOL_ROOT", Path(__file__).resolve().parents[1]))
TOOLCHAIN = Path(os.environ.get("PVISOR_REFERENCE_TOOLCHAIN", ROOT / "opt/toolchain"))
WORKLOAD = Path(__file__).resolve()
HARNESS = Path(os.environ.get("PVISOR_REFERENCE_HARNESS", WORKLOAD.parent / "harness"))
sys.path.insert(0, str(HARNESS))
from v1 import agent as fixture_model  # noqa: E402 - also loaded inside the guest rootfs


def checked(argv, cwd=None, env=None):
    result = subprocess.run(
        argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=90
    )
    if result.returncode:
        raise RuntimeError(
            f"{argv}: exit={result.returncode}\n{result.stdout[-1000:]}\n{result.stderr[-3000:]}"
        )
    return result.stdout


def tools_env():
    return os.environ | {
        "PATH": f"{TOOLCHAIN}/bin:{ROOT}/usr/local/bin:{ROOT}/usr/bin:/bin",
        "RUSTC": str(TOOLCHAIN / "bin/rustc"),
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "safe.directory",
        "GIT_CONFIG_VALUE_0": "*",
        "CARGO_HOME": "/tmp/reference-cargo",
        "HOME": "/tmp/reference-home",
        "TMPDIR": "/tmp",
        "CARGO_TARGET_DIR": str(Path.cwd() / "rust/target"),
    }


def probe():
    env = tools_env()
    versions = {
        name: checked([str(path), "--version"], env=env).splitlines()[0]
        for name, path in {
            "python": ROOT / "usr/bin/python3",
            "node": ROOT / "usr/bin/node",
            "git": ROOT / "usr/bin/git",
            "cargo": TOOLCHAIN / "bin/cargo",
            "rustc": TOOLCHAIN / "bin/rustc",
            "claude": ROOT / "usr/local/bin/claude",
            "codex": ROOT / "usr/local/bin/codex",
        }.items()
    }
    versions["kernel"] = os.uname().release
    print("REFERENCE_ENV_READY " + json.dumps(versions), flush=True)
    return versions


def pipeline(repair=True):
    env = tools_env()
    phases = {}
    cmds = [
        ("inspect", ["git", "status", "--porcelain"]),
        ("search", ["rg", "return a - b", "python"]),
    ]
    for name, argv in cmds:
        start = time.perf_counter_ns()
        out = checked(argv, env=env)
        phases[name] = (time.perf_counter_ns() - start) / 1e6
        if name == "search":
            assert "return a - b" in out
    if repair:
        Path("python/adder.py").write_text("def add(a, b):\n    return a + b\n")
    for name, argv in [
        (
            "python-tests",
            [str(ROOT / "usr/bin/python3"), "-m", "unittest", "discover", "-s", "python"],
        ),
        (
            "rust-tests",
            ["cargo", "test", "--offline", "--manifest-path", "rust/Cargo.toml", "-j", "2"],
        ),
        (
            "node-install",
            [
                "npm",
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
                "--prefix",
                "node",
            ],
        ),
        ("node-tests", ["npm", "test", "--prefix", "node"]),
        ("diff", ["git", "diff", "--exit-code", "--", "node", "rust"]),
    ]:
        start = time.perf_counter_ns()
        checked(argv, env=env)
        phases[name] = (time.perf_counter_ns() - start) / 1e6
    diff = checked(["git", "diff", "--", "python/adder.py"], env=env)
    assert "+    return a + b" in diff
    assert Path("python/adder.py").read_text() == "def add(a, b):\n    return a + b\n"
    return phases


def grade_returned(requests):
    """Only accept actual tool results, never a marker in the prompt or schema."""
    for request in requests:
        body = request["body"]
        if "messages" in body:
            for message in body["messages"]:
                content = message.get("content")
                if message.get("role") == "user" and isinstance(content, list):
                    for block in content:
                        if block.get("type") == "tool_result" and not block.get("is_error"):
                            if "REFERENCE_GRADE_PASS" in json.dumps(block.get("content")):
                                return True
        for item in body.get("input", []):
            if isinstance(item, dict) and item.get("type") == "function_call_output":
                value = item.get("output", "")
                if "REFERENCE_GRADE_PASS" in value and "Process exited with code 0" in value:
                    return True
    return False


class RecordingModel(fixture_model.Model):
    def do_POST(self):
        try:
            super().do_POST()
        finally:
            with self.server.lock:
                Path("_model-requests.json").write_text(json.dumps(self.server.requests, indent=2))


def agent_loop(name):
    env = tools_env()
    home = Path.cwd() / "_agent-home"
    home.mkdir()
    (home / "codex").mkdir()
    env.update(
        HOME=str(home),
        CODEX_HOME=str(home / "codex"),
        XDG_CONFIG_HOME=str(home / "config"),
        ANTHROPIC_API_KEY="reference-fake",
        BENCH_API_KEY="reference-fake",
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
    )
    server = ThreadingHTTPServer(("127.0.0.1", 0), RecordingModel)
    server.lock = threading.Lock()
    server.requests = []
    fixture_model.COMMAND = (
        f"{ROOT}/usr/bin/python3 {WORKLOAD} --mode tool-action"
    )
    thread = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True
    )
    thread.start()
    url = f"http://127.0.0.1:{server.server_port}"
    env["ANTHROPIC_BASE_URL"] = url
    try:
        if name == "claude":
            argv = [
                str(ROOT / "usr/local/bin/claude"),
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
                "Fix the Python adder and run the Python, Rust and Node project tests.",
            ]
        else:
            argv = [
                str(ROOT / "usr/local/bin/codex"),
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--json",
                "-s",
                "danger-full-access",
                "-m",
                "reference-fixture",
                "-c",
                'model_provider="reference"',
                "-c",
                f'model_providers.reference={{name="reference",base_url="{url}/v1",env_key="BENCH_API_KEY",wire_api="responses"}}',
                "Fix the Python adder and run the Python, Rust and Node project tests.",
            ]
        output = checked(argv, env=env)
        Path("_cli-output.json").write_text(output)
        assert "BENCH_COMPLETE" in output
        assert grade_returned(server.requests), "graded tool result did not reach CLI model loop"
        assert Path("python/adder.py").read_text() == "def add(a, b):\n    return a + b\n"
        Path("_model-requests.json").write_text(json.dumps(server.requests, indent=2))
        return {"model_requests": len(server.requests), "real_inference": False, "grade": "passed"}
    finally:
        Path("_model-requests.json").write_text(json.dumps(server.requests, indent=2))
        server.shutdown()
        server.server_close()
        thread.join()


def filesystem():
    configuration = Path("_fs/fixture.json")
    value = json.loads(configuration.read_text())
    value["toolchain"] = str(TOOLCHAIN)
    configuration.write_text(json.dumps(value))
    results = {}
    for mode in ("metadata", "read", "write", "git", "rg", "cargo", "npm"):
        output = checked(
            [str(ROOT / "usr/bin/python3"), str(HARNESS / "v1/workload.py"), mode],
            cwd="_fs",
            env=tools_env(),
        )
        record = json.loads(output.strip().splitlines()[-1])
        assert record["workload"] == mode
        results[mode] = record
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", required=True)
    args = parser.parse_args()
    if args.mode == "tool-action":
        pipeline()
        print("REFERENCE_GRADE_PASS", flush=True)
        return
    started = time.perf_counter_ns()
    if args.mode == "filesystem":
        print("REFERENCE_READY", flush=True)
        result = {"filesystem": filesystem()}
    elif args.mode == "env":
        result = {"versions": probe()}
    elif args.mode == "tools":
        print("REFERENCE_READY", flush=True)
        result = {"phases_ms": pipeline()}
    elif args.mode in ("claude", "codex"):
        print("REFERENCE_READY", flush=True)
        result = agent_loop(args.mode)
    else:
        raise ValueError(args.mode)
    result.update(
        mode=args.mode, worker_ms=(time.perf_counter_ns() - started) / 1e6, correctness="passed"
    )
    print("REFERENCE_RESULT " + json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
