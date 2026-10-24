#!/usr/bin/env python3
"""B-STARTUP: observe paired Ready and normal Exit for a prepared backend.

Motivation: waiting for a usable prepared VM determines interaction latency.
Conclusion sought: launch-to-first-valid-output latency at 2 vCPU / 128 MiB.
Design: 3 warmups, 60 samples, fresh stage/workspace, warm host caches; output,
normal exit and any pVisor Run Bundle must pass after timing stops. Interpreter
startup, input preparation and teardown are excluded; Popen and pipe observation
are included. Exit uses the same start and includes work through normal exit.
Measurements use the Python perf_counter_ns clock.
"""

import argparse
import importlib.util
import json
import os
import subprocess
from pathlib import Path

from hooks import archive, cleanup_container, prepare, validate

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("ready_common", ROOT / "common.py")
COMMON = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(COMMON)
RUNS = 60
WARMUP = 3
TIMEOUT = 30
READY_FIELDS = COMMON.RUNTIME.READY_FIELDS
EXIT_FIELDS = COMMON.RUNTIME.EXIT_FIELDS
FIELDS = (*READY_FIELDS, *EXIT_FIELDS[1:])
summarize = COMMON.RUNTIME.summarize_ready
summarize_pairs = COMMON.RUNTIME.summarize_pairs
capture = COMMON.RUNTIME.capture


def run_trial(batch, phase, index, reset=True):
    protocol = json.loads((batch / "protocol.json").read_text())
    backend = protocol["backends"][0]
    if reset:
        prepare(batch, backend)
    env = os.environ.copy() | {
        "PVISOR_RUN_HOME": str(batch / "current/runs"),
        "PVISOR_STARTUP_TIMING": "0",
        "PVISOR_FS_PROFILE": "0",
    }
    record = capture(protocol["subject"], batch / "workspace", env, batch / "current/stdout")
    record.update(phase=phase, index=index)
    COMMON.save(batch / "current/timing.json", record)
    (batch / "current/stdout.timing.json").unlink()
    validate(batch, backend)
    archive(batch)
    return {"ready_ns": record["ready_duration_ns"], "exit_ns": record["exit_duration_ns"]}


def measure(batch):
    manifest = json.loads((batch / "manifest.json").read_text())
    if manifest["status"] != "prepared" or manifest["metric_scope"] != COMMON.READY_EXIT_SCOPE:
        raise ValueError("expected a freshly prepared Ready / Exit batch")
    if len(list((batch / "trials").glob("*/timing.json"))) != 1:
        raise ValueError("Ready batch must have only its preflight; do not reuse a measured batch")
    for path, sha in manifest["artifacts"].items():
        if COMMON.digest(path) != sha:
            raise ValueError("prepared artifact changed: " + path)
    samples = COMMON.RUNTIME.repeat(
        WARMUP, RUNS, lambda phase, index: run_trial(batch, phase, index)
    )
    result = summarize_pairs(samples)
    COMMON.save(
        batch / "timing.samples.json",
        {
            "engine": "python-perf_counter_ns",
            "unit": "nanosecond",
            "samples": samples,
            "result": result,
        },
    )
    return result


def main(backend):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("batch", type=Path)
    args = parser.parse_args()
    try:
        batch = args.batch.resolve()
        protocol = json.loads((batch / "protocol.json").read_text())
        if protocol["backends"] != [backend]:
            raise ValueError("script backend differs from prepared protocol")
        print(json.dumps(measure(batch)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        if (args.batch / "manifest.json").exists():
            manifest = json.loads((args.batch / "manifest.json").read_text())
            if manifest.get("status") == "prepared" and manifest.get("conditions") == [backend]:
                manifest.update(status="failed", error=str(error))
                COMMON.save(args.batch / "manifest.json", manifest)
        if (args.batch / "protocol.json").exists():
            cleanup_container(args.batch)
        parser.exit(1, f"{error}\n")
