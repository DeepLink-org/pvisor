"""Strict coarse-phase and storage-placement checks; no BIO data."""

import json
from pathlib import Path

from arrival_common import SSD

REQUIRED = {
    "seal.journal_sync",
    "seal.upper_sync",
    "seal.parent_sync",
    "seal.publish",
    "runtime.teardown",
    "runtime.overlay_stage",
    "runtime.overlay_finalize",
    "runtime.remaining_teardown",
    "runtime.commit",
    "driver.handle_wait",
    "driver.identity_join",
    "driver.log_drain",
    "driver.event_snapshot",
    "driver.summary_total",
    "summary.events_serialize",
    "summary.events_write",
    "summary.result_write",
    "log.pump_join",
    "log.writer_join",
}


def phases(lines):
    pending = {}
    done = {}
    pid = None
    for line in lines:
        if not line.startswith("PVISOR_SHUTDOWN "):
            continue
        e = json.loads(line.split(" ", 1)[1])
        name = e["phase"]
        if pid is None:
            pid = e["pid"]
        assert e["pid"] == pid, "Unexpected phase PID"
        assert name in REQUIRED, "Unexpected phase"
        assert isinstance(e["unix_ns"], int) and e["unix_ns"] > 0
        if e["edge"] == "begin":
            assert name not in pending and name not in done, "Duplicate phase"
            pending[name] = e
        else:
            assert e["edge"] == "end" and name in pending, "Unmatched phase end"
            a = pending.pop(name)
            assert e["success"] is True, "Unsuccessful phase"
            assert e["elapsed_ns"] >= 0 and e["unix_ns"] >= a["unix_ns"]
            done[name] = dict(
                seconds=e["elapsed_ns"] / 1e9, begin_unix_ns=a["unix_ns"], end_unix_ns=e["unix_ns"]
            )
    assert not pending, "Incomplete phase"
    assert set(done) == REQUIRED, "Missing phase"
    order = ["seal.journal_sync", "seal.upper_sync", "seal.parent_sync", "seal.publish"]
    for a, b in zip(order, order[1:]):
        assert done[a]["end_unix_ns"] <= done[b]["begin_unix_ns"], "Seal order changed"
    assert done["runtime.overlay_stage"]["end_unix_ns"] <= done["driver.log_drain"]["begin_unix_ns"]
    assert done["driver.log_drain"]["end_unix_ns"] <= done["driver.summary_total"]["begin_unix_ns"]
    return done


def audit_job(job, backing_device=None):
    def load(p):
        return json.loads(p.read_text())

    meta = load(job / "started.json")
    owned = load(job / "owned-unit.json")
    vm = job / "runs" / meta["task"] / "vm"
    stage = Path(owned["stage_dir"])
    expected = SSD / job.name / "stage"
    assert stage == expected and stage.resolve() == expected
    assert (
        stage.stat().st_dev
        == (Path(owned["backing"]).stat().st_dev if backing_device is None else backing_device)
        != vm.stat().st_dev
    )
    for name in ["upper", "work", "preimages"]:
        assert (stage / name).resolve() == stage / name
        assert (stage / name).stat().st_dev == stage.stat().st_dev
    assert (stage / "preimages/sealed-v1").read_bytes() == b"pvisor.stage.sealed/1\n"
    assert (stage / "preimages/complete-v1").is_file()
    overlay = load(stage / "overlay.json")
    assert overlay["state"] == "staged"
    assert Path(overlay["upper"]["upper_dir"]) == stage / "upper"
    assert Path(overlay["upper"]["work_dir"]) == stage / "work"
    run = load(stage / "run.json")
    assert run["state"] == "completed" and run["finished_at_unix_ms"] is not None
    result = load(vm / "run-result.json")
    assert result["state"] == "completed" and result["exit_code"] == 0
    assert load(vm / "driver-shutdown.json")["signal"] is None, "Forced shutdown is not success"
    p = phases((vm / "driver.log").read_text().splitlines())
    events = [json.loads(line) for line in (vm / "events.jsonl").read_text().splitlines()]
    assert not any(e["event"] == "shutdown.wait_error" for e in events), (
        "Original shutdown timeout/error fired"
    )
    names = [
        "shutdown.stop_request_begin",
        "shutdown.stop_ack",
        "shutdown.process_wait_begin",
        "shutdown.process_wait_end",
        "vm_stop",
    ]
    selected = {n: [e for e in events if e["event"] == n] for n in names}
    assert all(len(v) == 1 for v in selected.values())
    ev = {k: v[0] for k, v in selected.items()}
    for a, b in zip(names, names[1:]):
        assert ev[a]["host_monotonic_ns"] <= ev[b]["host_monotonic_ns"]
    assert ev["vm_stop"]["return_code"] == ev["shutdown.process_wait_end"]["return_code"] == 0
    verify = next(e["host_monotonic_ns"] for e in events if e["event"] == "trial.verification_end")

    def seconds(a, b):
        return (ev[b]["host_monotonic_ns"] - ev[a]["host_monotonic_ns"]) / 1e9

    assert seconds("shutdown.process_wait_begin", "shutdown.process_wait_end") < 300
    return dict(
        passed=True,
        stage_dir=str(stage),
        phases=p,
        post_verifier_s=(ev["vm_stop"]["host_monotonic_ns"] - verify) / 1e9,
        stop_ack_s=seconds("shutdown.stop_request_begin", "shutdown.stop_ack"),
        process_wait_s=seconds("shutdown.process_wait_begin", "shutdown.process_wait_end"),
        policy=meta["policy"],
        job=job.name,
    )
