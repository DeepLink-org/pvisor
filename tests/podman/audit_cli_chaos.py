#!/usr/bin/env python3
"""Read-only audit of CLI chaos receipts; never starts a CLI or a VM."""

import argparse
import hashlib
import json
from pathlib import Path

if not __debug__:
    raise RuntimeError(
        "CLI chaos audit requires assertions enabled; do not use -O or PYTHONOPTIMIZE"
    )


def audit(root):
    manifest = json.loads((root / "manifest.json").read_text())
    outcome = json.loads((root / "outcome.json").read_text())
    assert outcome["passed"] and not outcome["failure"] and not outcome["cleanup_errors"], outcome
    assert sum(outcome["counts"].values()) == outcome["rounds"], outcome
    assert not json.loads((root / "cleanup.json").read_text())["errors"]
    limits = json.loads((root / "limits.json").read_text())
    assert limits["memory.max"] == str(1536 * 1024**2) and limits["memory.swap.max"] == "0", limits
    assert limits["pids.max"] == "384"
    quota, period = map(int, limits["cpu.max"].split())
    assert quota * 2 == period * 3
    assert (
        hashlib.sha256((root / "runner.py").read_bytes()).hexdigest() == manifest["driver_sha256"]
    )
    receipts = sorted(root.glob("cmd-*"))
    assert len(receipts) == outcome["commands"], (len(receipts), outcome)
    checked_outputs = 0
    checked_inputs = 0
    error_causes = 0
    roundtrips = 0
    working_directories = set()
    daemon_starts = {}
    pool_capability = None
    lost_pool_counts = []
    for directory in receipts:
        command = json.loads((directory / "command.json").read_text())
        if command.get("cwd") is not None:
            working_directories.add(command["cwd"])
        result = json.loads((directory / "exit.json").read_text())
        expected = command["expected_code"]
        expected = expected if isinstance(expected, list) else [expected]
        assert not result["timed_out"] and result["code"] in expected, directory
        for stream in ["stdout", "stderr"]:
            data = (directory / stream).read_bytes()
            digest = command.get("expected_" + stream + "_sha256")
            if digest is not None:
                assert hashlib.sha256(data).hexdigest() == digest, (directory, stream)
                checked_outputs += 1
        if (directory / "stdin").exists():
            data = (directory / "stdin").read_bytes()
            assert (
                len(data) == command["stdin_bytes"]
                and hashlib.sha256(data).hexdigest() == command["stdin_sha256"]
            ), directory
            checked_inputs += 1
        if command["argv"] == ["ps", "-q"]:
            live = len((directory / "stdout").read_bytes().splitlines())
            if command["step"] == "pool_loss":
                assert 0 <= live <= 2, directory
                lost_pool_counts.append(live)
            else:
                assert live == 2, directory
        arguments = command["argv"]
        if arguments[:2] == ["daemon", "start"] and result["code"] == 0:
            status = json.loads((directory / "stdout").read_text())
            key = (command["step"], command["round"])
            if key in daemon_starts:
                assert daemon_starts[key] == status, directory
            daemon_starts[key] = status
            if pool_capability is None:
                pool_capability = status["memory_pool"]
            assert status["memory_pool"] == pool_capability, directory
        script = arguments[-1] if len(arguments) > 2 and arguments[-2] == "-c" else ""
        stream_copy = arguments[:2] == ["exec", "-i"] and script.startswith(
            "cat; printf stream-error >&2; exit "
        )
        run_copy = arguments[:2] == ["run", "--rm"] and (
            script.startswith("cat; exit ") or script == "cat; kill -TERM $$"
        )
        if stream_copy or run_copy:
            data = (directory / "stdout").read_bytes()
            assert (
                len(data) == command["stdin_bytes"]
                and hashlib.sha256(data).hexdigest() == command["stdin_sha256"]
            ), directory
            if stream_copy:
                assert (directory / "stderr").read_bytes() == b"stream-error", directory
            roundtrips += 1
        for prefix, reason in [
            (["run", "-M", "--memory-pool"], b"cannot be used"),
            (["run", "--unknown-cli-chaos"], b"unexpected argument"),
            (["run", "-m", "invalid"], b"invalid memory size"),
            (["run", "--cpus", "0"], b"invalid VM CPU/memory budget"),
        ]:
            if arguments[: len(prefix)] == prefix:
                assert (
                    not (directory / "stdout").read_bytes()
                    and reason in (directory / "stderr").read_bytes()
                ), directory
                error_causes += 1
    if outcome["counts"].get("pool_loss", 0):
        fault = json.loads((root / "pool-loss.json").read_text())
        assert (
            fault["verified"]
            and fault["dependent_vms_stopped"]
            and fault["implicit_replacement_refused"]
        ), fault
        assert (
            lost_pool_counts
            and lost_pool_counts[-1] == 0
            and lost_pool_counts == sorted(lost_pool_counts, reverse=True)
        ), lost_pool_counts
        assert fault["after_socket_inode"] == fault["retained_socket_inode"]
    samples = [json.loads(line) for line in (root / "resources.jsonl").read_text().splitlines()]
    assert samples
    for sample in samples:
        group = sample["cgroup"]
        assert (
            int(group["memory.peak"]) <= 1536 * 1024**2 and int(group["memory.swap.current"]) == 0
        ), sample
        events = dict(line.split() for line in group["memory.events"].splitlines())
        assert all(int(events[key]) == 0 for key in ["max", "oom", "oom_kill"]), sample
    return {
        "passed": True,
        "rounds": outcome["rounds"],
        "commands": len(receipts),
        "elapsed": outcome["elapsed"],
        "paused_samples": outcome["paused_samples"],
        "output_hashes_rechecked": checked_outputs,
        "input_hashes_rechecked": checked_inputs,
        "invalid_error_causes_rechecked": error_causes,
        "cat_roundtrip_hashes_rechecked": roundtrips,
        "working_directories_seen": sorted(working_directories),
        "daemon_start_groups_rechecked": len(daemon_starts),
        "owned_pool_loss_verified": bool(outcome["counts"].get("pool_loss", 0)),
        "peak_cgroup_mib": max(int(sample["cgroup"]["memory.peak"]) for sample in samples)
        / 1024**2,
        "minimum_host_available_gib": min(sample["available"] for sample in samples) / 1024**3,
        "maximum_host_load": max(sample["load1"] for sample in samples),
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    print(json.dumps(audit(args.evidence), indent=2))
