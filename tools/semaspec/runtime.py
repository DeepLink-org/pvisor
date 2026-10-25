"""Semaspec's stdlib experiment runtime; clocks and evidence are shared by adapters.

Measurements use perf_counter_ns around direct argv execution, never shell parsing.
Preparation/conclusion/cleanup and aggregation are outside that interval. Ready
observation has its own explicit clock boundary. No approval or publication occurs.
"""

import json
import os
import selectors
import shlex
import signal
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

ENGINE = "python-perf_counter_ns"
FIELDS = ("N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms")
READY_FIELDS = (
    "N",
    "Ready median ms",
    "Ready mean ms",
    "Ready SD ms",
    "Ready min ms",
    "Ready max ms",
)
EXIT_FIELDS = tuple(field.replace("Ready", "Exit") for field in READY_FIELDS)


def save(path, value):
    """Atomically publish JSON evidence, including on a failed trial."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=path.parent, delete=False
    ) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    temporary.replace(path)


def repeat(warmup, runs, trial):
    """Validate the plan before any side effects; never discard slow samples."""
    if type(warmup) is not int or warmup < 0 or type(runs) is not int or runs < 2:
        raise ValueError("warmup must be nonnegative and runs must be at least two")
    for index in range(warmup):
        trial("warmup", index)
    return [trial("sample", index) for index in range(runs)]


def summarize(samples):
    if len(samples) < 2 or any(type(n) is not int or n <= 0 for n in samples):
        raise ValueError("at least two positive integer durations required")
    values = [n / 1_000_000 for n in samples]
    return dict(
        zip(
            FIELDS,
            (
                len(values),
                statistics.median(values),
                statistics.mean(values),
                statistics.stdev(values),
                min(values),
                max(values),
            ),
        )
    )


def command_trial(path, condition, phase, index, config, validate=None):
    record = dict(
        condition=condition,
        phase=phase,
        index=index,
        command=config["argv"],
        environment=config.get("env", {}),
        hooks=[],
        exit_code=None,
    )
    env = os.environ.copy() | config.get("env", {})
    try:
        for action in ("prepare", "subject", "conclude"):
            argv = config["argv"] if action == "subject" else config.get(action)
            if not argv:
                continue
            start = time.perf_counter_ns()
            result = subprocess.run(
                argv,
                cwd=config["cwd"],
                env=env,
                capture_output=True,
                text=True,
                timeout=config["timeout"],
            )
            end = time.perf_counter_ns()
            outcome = dict(
                command=argv,
                exit_code=result.returncode,
                stdout=result.stdout,
                stderr=result.stderr,
            )
            if action == "subject":
                record.update(outcome, start_ns=start, end_ns=end, duration_ns=end - start)
            else:
                record["hooks"].append(dict(action=action, **outcome))
            if result.returncode:
                raise ValueError(f"{action} failed with exit {result.returncode}")
        if "expected_stdout" in config and record["stdout"] != config["expected_stdout"]:
            raise ValueError("stdout differs from expected text")
        if validate is not None:
            validate(record)
        return record["duration_ns"]
    except BaseException as error:
        record["error"] = str(error)
        if isinstance(error, subprocess.TimeoutExpired):
            record["stdout"] = (error.stdout or b"").decode(errors="replace")
            record["stderr"] = (error.stderr or b"").decode(errors="replace")
        raise
    finally:
        if config.get("cleanup"):
            try:
                result = subprocess.run(
                    config["cleanup"],
                    cwd=config["cwd"],
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=config["timeout"],
                )
                record["hooks"].append(
                    dict(
                        action="cleanup",
                        command=config["cleanup"],
                        exit_code=result.returncode,
                        stdout=result.stdout,
                        stderr=result.stderr,
                    )
                )
                if result.returncode:
                    raise ValueError(f"cleanup failed with exit {result.returncode}")
            except BaseException as error:
                record["cleanup_error"] = str(error)
                save(path, record)
                if "error" not in record:
                    raise
        save(path, record)


def audit_command_row(batch, manifest, condition, record, resolved):
    config = json.loads((batch / "commands.json").read_text())[condition]
    phases = [("warmup", i) for i in range(manifest["warmup"])] + [
        ("sample", i) for i in range(manifest["runs"])
    ]
    expected_paths = [batch / "command-trials" / condition / f"{p}-{i:04d}.json" for p, i in phases]
    if set((batch / "command-trials" / condition).glob("*.json")) != set(expected_paths):
        raise ValueError("missing or extra command trials")
    samples = []
    for path, (phase, index) in zip(expected_paths, phases):
        item = json.loads(path.read_text())
        if (
            item.get("error")
            or item.get("cleanup_error")
            or item["condition"] != condition
            or item["phase"] != phase
            or item["index"] != index
            or item["command"] != config["argv"]
            or item["environment"] != config.get("env", {})
            or item["exit_code"] != 0
            or type(item["start_ns"]) is not int
            or type(item["end_ns"]) is not int
            or item["end_ns"] - item["start_ns"] != item["duration_ns"]
            or item["duration_ns"] <= 0
        ):
            raise ValueError("failed or altered command trial")
        hooks = [
            (action, config[action])
            for action in ("prepare", "conclude", "cleanup")
            if config.get(action)
        ]
        if [(h["action"], h["command"]) for h in item["hooks"]] != hooks or any(
            h["exit_code"] != 0 for h in item["hooks"]
        ):
            raise ValueError("missing or failed correctness hook")
        if "expected_stdout" in config and item["stdout"] != config["expected_stdout"]:
            raise ValueError("stdout differs from expected text")
        if phase == "sample":
            samples.append(item["duration_ns"])
    expected = summarize(samples)
    raw = json.loads((batch / f"{condition}.samples.json").read_text())
    if (
        raw
        != dict(
            engine="python-perf_counter_ns", unit="nanosecond", samples=samples, result=expected
        )
        or list(resolved) != list(FIELDS)
        or resolved != expected
        or record["raw"] != expected
        or record["value"] != expected
    ):
        raise ValueError("resolved row or samples differ from command trials")
    return dict(command=shlex.join(config["argv"]), statistics=expected)


def summarize_ready(samples, label="Ready"):
    values = summarize(samples)
    return dict(
        zip(tuple(field.replace("Ready", label) for field in READY_FIELDS), values.values())
    )


def summarize_pairs(samples):
    if any(
        set(s) != {"ready_ns", "exit_ns"}
        or type(s["ready_ns"]) is not int
        or type(s["exit_ns"]) is not int
        or not 0 < s["ready_ns"] <= s["exit_ns"]
        for s in samples
    ):
        raise ValueError("Ready and Exit must share one valid trial")
    return summarize_ready([s["ready_ns"] for s in samples]) | summarize_ready(
        [s["exit_ns"] for s in samples], "Exit"
    )


def capture(argv, cwd, env, log, timeout=30):
    """Start the clock immediately before Popen; stop at the exact line receipt.

    Drain output and wait for normal exit afterwards. Timeout/failure kills only
    this trial's owned process group, preserving output and diagnostic timing.
    """
    record = {
        "command": argv,
        "start_ns": None,
        "ready_ns": None,
        "end_ns": None,
        "exit_code": None,
    }
    process = None
    try:
        with log.open("ab", buffering=0) as output, selectors.DefaultSelector() as selector:
            start = time.perf_counter_ns()
            record["start_ns"] = start
            process = subprocess.Popen(
                argv,
                cwd=cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                bufsize=0,
                start_new_session=True,
            )
            selector.register(process.stdout, selectors.EVENT_READ)
            pending = b""
            deadline = start / 1e9 + timeout
            while selector.get_map():
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    raise TimeoutError("Ready trial did not finish normally within timeout")
                for key, _ in selector.select(min(remaining, 0.1)):
                    block = os.read(key.fd, 65536)
                    received = time.perf_counter_ns()
                    if not block:
                        selector.unregister(key.fileobj)
                        continue
                    output.write(block)
                    pending += block
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        if line.rstrip(b"\r") == b"REFERENCE_READY":
                            if record["ready_ns"] is not None:
                                raise ValueError("duplicate Ready marker")
                            record["ready_ns"] = received
            record["exit_code"] = process.wait(timeout=max(0.001, deadline - time.perf_counter()))
            record["end_ns"] = time.perf_counter_ns()
            if record["exit_code"] != 0 or record["ready_ns"] is None:
                raise ValueError("trial failed or did not emit an exact Ready line")
            record["ready_duration_ns"] = record["ready_ns"] - record["start_ns"]
            record["exit_duration_ns"] = record["end_ns"] - record["start_ns"]
            return record
    except BaseException as error:
        record["error"] = str(error)
        raise
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            process.stdout.close()
        save(log.with_suffix(".timing.json"), record)


def measure_plan(config, directory):
    """Execute a validated direct-argv plan; retain each warmup and formal trial.

    This protocol is also embedded in the standalone Rust CLI. Artifact lifetime
    belongs to its caller; no user-maintained result file is required.
    """
    directory = Path(directory)
    samples = []
    outcome = dict(engine=ENGINE, unit="nanosecond", status="FAIL", samples=samples, result=None)

    def trial(phase, index):
        duration = command_trial(
            directory / f"{phase}-{index:04d}.json", config["id"], phase, index, config
        )
        if phase == "sample":
            samples.append(duration)
        return duration

    try:
        repeat(config["warmup"], config["runs"], trial)
        outcome.update(status="PASS", result=summarize(samples))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        outcome["error"] = str(error)
    outcome["trials"] = [json.loads(path.read_text()) for path in sorted(directory.glob("*.json"))]
    return outcome


if __name__ == "__main__":
    import sys

    print(json.dumps(measure_plan(json.load(sys.stdin), sys.argv[1])))
