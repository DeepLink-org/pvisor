#!/usr/bin/env python3
"""Reconcile saved bounded SDK chaos evidence without starting any VM.

This checks provenance, deterministic payloads, operation accounting and resource
receipts. The frozen live driver performs SDK assertions; these records do not
replace a capture of every HTTP response or prove arbitrary linearizability.
"""

import argparse
import collections
import hashlib
import json
import random
from pathlib import Path


def load(path):
    return json.loads(path.read_text())


def sha(data):
    return hashlib.sha256(data).hexdigest()


def audit(root):
    cfg = load(root / "config.json")
    inputs = load(root / "inputs.json")
    summary = load(root / "summary.json")
    assert summary["passed"] and not summary["background_errors"]
    assert not summary["cleanup_errors"] and not load(root / "cleanup.json")["errors"]
    assert not any(
        (root / name).exists()
        for name in (
            "failure.json",
            "corruption.json",
            "background-corruption.json",
            "background-error.json",
            "unexpected-api-exit.json",
        )
    )
    assert sha(Path(cfg["binary"]).read_bytes()) == inputs["binary_sha256"]
    assert sha((root / "daemon_chaos.py").read_bytes()) == inputs["driver_sha256"]
    assert (
        sha((root / "images/native-ubuntu.json").read_bytes()) == inputs["derived_manifest_sha256"]
    )
    schedule = load(root / "schedule.json")
    progress = load(root / "progress.json")
    assert len(schedule) == cfg["rounds"] == progress["round"] == progress["total"]
    assert summary["seed"] == cfg["seed"]
    counts = collections.Counter()
    expected_exits = root / "expected-api-exits.jsonl"
    if expected_exits.exists():
        expected_exit_pids = collections.Counter(
            json.loads(line)["pid"] for line in expected_exits.open()
        )
        observed = collections.Counter()
        for line in (root / "api-exits.jsonl").open():
            event = json.loads(line)
            if not event["stopping"]:
                assert event["code"] == -9
                observed[event["pid"]] += 1
        assert observed == expected_exit_pids
    identities, tokens, deleted, active = set(), {}, set(), set()
    pending, contents, heartbeats = {}, {}, {}
    maximum_live = 0
    for line in (root / "operations.jsonl").open():
        event = json.loads(line)
        kind, identity = event["kind"], event.get("id")
        counts[kind] += 1
        assert 0 <= event["seconds"] <= summary["seconds"]
        if identity is not None and kind != "create":
            assert identity in identities, event
        if kind == "create":
            assert identity not in identities
            identities.add(identity)
            active.add(identity)
            if "token_sha256" in event:
                tokens[identity] = event["token_sha256"]
            maximum_live = max(maximum_live, len(active))
        elif kind == "delete":
            assert identity in active
            active.remove(identity)
            deleted.add(identity)
        elif kind == "file_start":
            key = identity, event["slot"]
            assert key not in pending
            rng = random.Random(event["seed"])
            payload = rng.randbytes(rng.choice([0, 1, 31, 4095, 4096, 8193, 32769, 65537]))
            assert event["bytes"] == len(payload) and event["sha256"] == sha(payload)
            pending[key] = event
            contents[identity, f"/tmp/chaos/slot {event['slot']} ✓"] = sha(payload), len(payload)
        elif kind == "file_roundtrip":
            start = pending.pop((identity, event["slot"]))
            assert event["seed"] == start["seed"] and event["bytes"] == start["bytes"]
            assert event["mode"] in {0, 400, 600, 640, 644}
        elif kind == "read":
            if event["path"] == "/tmp/chaos/heartbeat":
                if identity in tokens:
                    assert event["sha256"] == tokens[identity]
                previous = heartbeats.setdefault(identity, (event["sha256"], event["bytes"]))
                assert previous == (event["sha256"], event["bytes"])
            else:
                assert contents[identity, event["path"]] == (event["sha256"], event["bytes"])
        elif kind == "background_read" and "sha256" in event:
            assert event["sha256"] == tokens[identity]
        elif kind == "command":
            assert event["exit_code"] == random.Random(event["seed"]).choice([0, 3, 7, 23, 125])
        elif kind == "append":
            rng = random.Random(event["seed"])
            expected = {str(65 + index): rng.randint(1, 8193) for index in range(8)}
            assert event["byte_counts"] == expected and event["bytes"] == sum(expected.values())
        elif kind == "background_commit":
            expected = f"commit-{event['seed']:x}".encode()
            assert event["sha256"] == sha(expected) and event["bytes"] == len(expected)
        elif kind in {"expected_disconnect", "expected_revoked_token"}:
            if "epoch_at_start" in event:
                assert (
                    event["faulting_at_start"]
                    or event["faulting_at_error"]
                    or event["epoch_at_start"] != event["epoch_at_error"]
                )
    assert not pending and counts == summary["counts"]
    if expected_exits.exists():
        assert (
            sum(expected_exit_pids.values()) == counts["restart_api"] + counts["background_crash"]
        )
    # Older drivers did not record deletion acknowledgements; newer evidence
    # must account for every completed delete/recreate and exact revoked token.
    if counts["delete"]:
        assert counts["delete"] == counts["recreate"]
        assert len(active) == 4 and maximum_live == 4
        for line in (root / "operations.jsonl").open():
            event = json.loads(line)
            if event["kind"] == "expected_revoked_token":
                assert event["id"] in deleted
    for kind in ("pause", "restart_api", "recreate", "cancel", "background_crash"):
        assert counts[kind] == schedule.count(kind)
    assert counts["file_roundtrip"] == 8 * schedule.count("files")
    assert counts["command"] == 12 * schedule.count("commands")
    assert counts["background_command"] == 4 * schedule.count("jobs")
    assert counts["append"] == 4 * schedule.count("append")
    assert counts["background_commit"] == schedule.count("background_crash")
    assert counts["create"] == 4 + counts["recreate"]
    if cfg.get("profile", "chaos") == "steady":
        assert counts["create"] == 4 and not any(
            counts[k] for k in ("recreate", "restart_api", "background_crash")
        )
    resource_samples, maximum_fd, maximum_peak, observed_max_live = 0, 0, 0, 0
    for line in (root / "resources.jsonl").open():
        sample = json.loads(line)
        resource_samples += 1
        live = [state for state in sample["guests"].values() if state["pids"]]
        assert len(live) <= 4
        observed_max_live = max(observed_max_live, len(live))
        for state in live:
            assert state["limits"] == {
                "memory.max": str(256 * 1024**2),
                "memory.swap.max": "0",
                "cpu.max": "100000 100000",
                "pids.max": "512",
            }
            assert state["peak"] <= 256 * 1024**2
            if "pids_current" in state:
                assert state["pids_current"] <= 512 and state["pids_events"].get("max", 0) == 0
            maximum_peak = max(maximum_peak, state["peak"])
            for process in state["processes"].values():
                assert process["nofile"] == ["1024", "524288"]
                assert process["fds"] < 1024
                maximum_fd = max(maximum_fd, process["fds"])
    assert resource_samples and observed_max_live == 4
    result = {
        "passed": True,
        "seed": cfg["seed"],
        "profile": cfg.get("profile", "chaos"),
        "rounds": len(schedule),
        "counts": counts,
        "seconds": summary["seconds"],
        "binary_sha256": inputs["binary_sha256"],
        "driver_sha256": inputs["driver_sha256"],
        "sampled_max_live_vms": observed_max_live,
        "resource_samples": resource_samples,
        "max_sampled_process_fds": maximum_fd,
        "max_sampled_guest_cgroup_peak_mib": maximum_peak / 2**20,
        "scope": "saved evidence reconciliation; live assertions are in the frozen driver",
    }
    (root / "audit.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directories", type=Path, nargs="+")
    args = parser.parse_args()
    print(json.dumps([audit(path.resolve()) for path in args.directories], indent=2))


if __name__ == "__main__":
    main()
