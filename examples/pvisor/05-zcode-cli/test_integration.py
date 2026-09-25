#!/usr/bin/env python3
"""Exercise the installed CLI, real file effects, SSE capture and process cleanup."""

import hashlib
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

EXAMPLE = Path(__file__).resolve().parent
WORK = Path(os.environ.get("WORK_ROOT", EXAMPLE / ".work")).resolve()
PVISOR = os.environ.get("PVISOR_BIN", str(EXAMPLE.parents[2] / "target/release/pvisor"))


def read_json(path):
    return json.loads(path.read_text())


def verify_normal_command():
    work = Path(tempfile.mkdtemp(prefix="normal-", dir=WORK))
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    subprocess.run([
        sys.executable, str(EXAMPLE / "prepare.py"), str(work),
        f"http://127.0.0.1:{port}/v1", PVISOR,
    ], check=True)
    state = work / "state"
    env = {**os.environ,
        "ZCODE_DATA_BASE_DIR": str(state),
        "ZCODE_STORAGE_DIR": str(state / "storage"),
        "ZCODE_SESSION_DB_PATH": str(state / "storage/sessions.sqlite"),
        "ZCODE_LOG_DIR": str(state / "logs"),
        "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE": str(state / "provider.json"),
        "ZCODE_MODEL_TELEMETRY_ENABLED": "false",
    }
    with (work / "mock.jsonl").open("w") as log:
        server = subprocess.Popen([sys.executable, str(EXAMPLE / "mock_llm.py"), str(port), "write"], stdout=log)
        try:
            for _ in range(100):
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.05)
            output = subprocess.run([
                "zcode", "--prompt", "Create hello.txt, then reply PVISOR_ZCODE_OK.",
                "--mode", "edit", "--no-color",
            ], cwd=work / "base", env=env, capture_output=True, text=True, timeout=60)
        finally:
            server.terminate()
            server.wait(timeout=10)
    (work / "zcode.log").write_text(output.stdout + output.stderr)
    assert output.returncode == 0, output.stderr
    assert output.stdout.strip() == "PVISOR_ZCODE_OK"
    assert (work / "base/hello.txt").read_text() == "hello from zcode\n"
    assert len((work / "mock.jsonl").read_text().splitlines()) == 2


def proc_snapshot():
    result = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdecimal():
            continue
        try:
            stat = (path / "stat").read_text()
            name, fields = stat[stat.index("(") + 1 :].rsplit(")", 1)
            fields = fields.split()
            result[int(path.name)] = (int(fields[1]), fields[19], name)
        except (OSError, ValueError, IndexError):
            continue
    return result


def run_case(scenario, deadline, case=None):
    case = case or scenario
    case_root = WORK / case
    env = {
        **os.environ,
        "WORK_ROOT": str(case_root),
        "PVISOR_BIN": PVISOR,
        "MOCK_SCENARIO": scenario,
        "PVISOR_TIMEOUT": deadline,
    }
    observed = {}
    saw_sleep = False
    log_path = WORK / f"zcode-test-{case}.log"
    with log_path.open("w") as log:
        process = subprocess.Popen(
            ["bash", str(EXAMPLE / "run.sh")],
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        started = time.monotonic()
        try:
            while process.poll() is None:
                snapshot = proc_snapshot()
                descendants = {process.pid}
                while True:
                    added = {
                        pid for pid, (parent, _, _) in snapshot.items() if parent in descendants
                    } - descendants
                    if not added:
                        break
                    descendants.update(added)
                for pid in descendants:
                    if pid not in snapshot:
                        continue
                    _, start, name = snapshot[pid]
                    observed[pid] = {"start": start, "name": name}
                    if name == "sleep":
                        try:
                            args = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
                            saw_sleep |= b"120" in args
                        except OSError:
                            pass
                if time.monotonic() - started > 100:
                    raise TimeoutError(f"Integration test hung; see {log_path}")
                time.sleep(0.15)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=15)
    time.sleep(0.3)
    current = proc_snapshot()
    survivors = [
        pid for pid, info in observed.items() if pid in current and current[pid][1] == info["start"]
    ]
    (WORK / f"zcode-processes-{case}.json").write_text(
        json.dumps(
            {
                "observed": observed,
                "saw_sleep_120": saw_sleep,
                "survivors": survivors,
                "exit_code": process.returncode,
            },
            indent=2,
        )
        + "\n"
    )
    assert not survivors, f"Processes survived {scenario}: {survivors}"
    if scenario == "write":
        assert process.returncode == 0, log_path.read_text()
    else:
        assert process.returncode != 0, "Timeout unexpectedly succeeded"
        assert saw_sleep, f"Bash sleep did not start; see {log_path}"
    return case_root / ("zcode-cli" if scenario == "write" else "zcode-cli-timeout")


def assert_base_unchanged(work):
    base = work / "base"
    actual = {
        str(p.relative_to(base)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in base.rglob("*")
        if p.is_file()
    }
    assert actual == read_json(work / "base-before.json"), "Host workspace changed before apply"


def verify_write(work, decision="apply"):
    base, stage = work / "base", work / "stage"
    bundle = read_json(work / "review.json")
    assert bundle["run"]["state"] == "completed"
    assert bundle["run"]["exit_code"] == 0
    assert bundle["run"]["output"]["stdout"].strip() == "PVISOR_ZCODE_OK"
    assert bundle["filesystem"]["state"] == "staged"
    assert (stage / "upper/hello.txt").read_text() == "hello from zcode\n"
    assert (work / "state/storage/sessions.sqlite").is_file()
    assert bundle["run"]["command"][0] == "zcode"
    assert bundle["safety"]["filesystem_non_bypassable"]
    assert not (base / ".runtime").exists()
    assert_base_unchanged(work)

    requests = [json.loads(line) for line in (work / "mock.jsonl").read_text().splitlines()]
    assert len(requests) == 2
    assert all(r["path"] == "/v1/chat/completions" and r["body"]["stream"] for r in requests)
    tool_results = [m for m in requests[1]["body"]["messages"] if m["role"] == "tool"]
    assert len(tool_results) == 1
    counts = bundle["network"]["intercepted"]
    assert counts["sink_requests"] == counts["requests_seen"] == 2
    assert counts["failures"] == 0
    events = [
        json.loads(line) for line in (stage / ".capture/events.jsonl").read_text().splitlines()
    ]
    llm = [event for event in events if event["kind"].startswith("llm.")]
    assert [event["kind"] for event in llm] == [
        "llm.request",
        "llm.response.stream",
    ] * 2
    assert llm[0]["call_id"] == llm[1]["call_id"]
    assert llm[2]["call_id"] == llm[3]["call_id"] != llm[0]["call_id"]
    assert "Write" in llm[1]["payload"]["assistant_content"]
    assert llm[3]["payload"]["assistant_content"] == "PVISOR_ZCODE_OK"

    if decision == "drop":
        subprocess.run([PVISOR, "drop", str(stage)], check=True)
        assert_base_unchanged(work)
        assert (work / "state/storage/sessions.sqlite").is_file()
        return
    subprocess.run([PVISOR, "apply", str(stage), "--path", "hello.txt"], check=True)
    assert (base / "hello.txt").read_text() == "hello from zcode\n"
    assert not (base / ".zcode-state").exists()
    assert (work / "state/storage/sessions.sqlite").is_file()
    assert (base / "hello.txt").read_text() == "hello from zcode\n"
    # Applying the task output preserves all original fixture inputs.
    for name, digest in read_json(work / "base-before.json").items():
        assert hashlib.sha256((base / name).read_bytes()).hexdigest() == digest
    assert not (base / ".zcode-state").exists()


def main():
    if sys.platform != "linux":
        raise SystemExit("This integration requires Linux rootless isolation and /proc")
    WORK.mkdir(parents=True, exist_ok=True)
    verify_normal_command()
    write = run_case("write", "60s")
    verify_write(write)
    dropped = run_case("write", "60s", case="drop")
    verify_write(dropped, decision="drop")
    timeout = run_case("timeout", "10s")
    bundle = read_json(timeout / "stage/run-bundle.json")
    assert bundle["run"]["state"] == "failed"
    assert bundle["run"]["failure"]["kind"] == "deadline_exceeded"
    assert_base_unchanged(timeout)
    subprocess.run([PVISOR, "drop", str(timeout / "stage")], check=True)
    print(
        "RESULT example=zcode-cli tool_write=1 sse_requests=2 applied=1 dropped=1 "
        "state_persisted=1 normal_command=1 normal_baseline=1 timeout=passed survivors=0"
    )


if __name__ == "__main__":
    main()
