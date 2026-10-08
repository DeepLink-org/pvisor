#!/usr/bin/env python3
"""Same-host experimental cold-pager comparison; run without other benchmarks.

Benchmark: B-VM-MEMORY (benchmark/README.md#b-vm-memory), role user-facing.
Motivation: idle Agent VMs hold memory; users need to know what reclaim
saves and what the next access costs.
Conclusion sought: reduction of host physical (or cgroup) memory for an idle
VM, and the added latency and CPU on first access after restore.
Design: fresh VM per trial, physical memory as the primary metric, footprint
only as a proxy, repeated and random data separated, integrity verified.

One private pool and fresh VM per trial. Guest timings exclude the explicit idle
window. Integrity, successful VM Bundles and actual reclaim/restore are required.
Small trial counts are diagnostic, not a tight tail-latency acceptance gate.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

from startup import percentile, validate_bundle

PAYLOAD = """import hashlib, json, time
data = bytearray(hashlib.shake_256(b'pvisor-migration-cold-data').digest(32*1024*1024))
expected = hashlib.sha256(data).hexdigest()
def read():
    start = time.perf_counter_ns()
    assert hashlib.sha256(data).hexdigest() == expected
    return (time.perf_counter_ns() - start) / 1e6
warm = read()
time.sleep(IDLE_SECONDS)
cold = read()
print(json.dumps(dict(warm_read_ms=warm, cold_read_ms=cold, digest=expected, integrity='passed')), flush=True)
time.sleep(1)
"""


def trial(args, variant, round_id):
    work = args.output / f"{round_id}-{variant}"
    work.mkdir()
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "probe.py").write_text(PAYLOAD.replace("IDLE_SECONDS", str(args.idle_seconds)))
    with tempfile.TemporaryDirectory(prefix="pv-cold-", dir="/private/tmp") as pool_dir:
        socket = Path(pool_dir) / "pool.sock"
        with (work / "pool.log").open("w") as pool_log:
            pool = subprocess.Popen(
                [str(args.pool), str(socket), "--max-bytes", str(128 * 1024 * 1024)],
                stdout=pool_log,
                stderr=subprocess.STDOUT,
            )
            try:
                deadline = time.monotonic() + 10
                while not socket.exists():
                    if pool.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f"pool startup failed; see {work}")
                    time.sleep(0.02)
                env = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR") if k in os.environ}
                env.update(
                    PVISOR_RUN_HOME=str(work / "runs"),
                    XDG_CONFIG_HOME=str(work / "config"),
                    PVISOR_STARTUP_TIMING="0",
                    PVISOR_EXPERIMENTAL_MEMORY_POOL=str(socket),
                    PVISOR_EXPERIMENTAL_MEMORY_METRICS="1",
                )
                command = [
                    str(getattr(args, variant)),
                    "run",
                    "--no-agent-defaults",
                    "--overlaynet",
                    "off",
                    "--stdio",
                    "inherit",
                    "--timeout",
                    "120s",
                    "--vm",
                    "--rootfs",
                    str(args.rootfs),
                    "--vm-library-dir",
                    str(args.firmware),
                    "--cpu",
                    "2",
                    "--memory",
                    "256MiB",
                    "--",
                    "/usr/bin/python3",
                    "probe.py",
                ]
                process = subprocess.Popen(
                    command,
                    cwd=workspace,
                    env=env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    start_new_session=True,
                )
                try:
                    stdout, stderr = process.communicate(timeout=135)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    stdout, stderr = process.communicate()
                    raise RuntimeError(f"VM timed out; see {work}")
                finally:
                    (work / "stdout.log").write_bytes(stdout)
                    (work / "stderr.log").write_bytes(stderr)
                result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
            finally:
                pool.terminate()
                try:
                    pool.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pool.kill()
                    pool.wait()
    (work / "stdout.log").write_bytes(result.stdout)
    (work / "stderr.log").write_bytes(result.stderr)
    if result.returncode != 0:
        raise RuntimeError(f"VM failed, exit={result.returncode}; see {work}")
    readings = [
        json.loads(line)
        for line in result.stdout.decode().splitlines()
        if line.startswith('{"warm_read_ms"')
    ]
    if len(readings) != 1 or readings[0]["integrity"] != "passed":
        raise RuntimeError(f"missing guest integrity result; see {work}")
    samples = [
        dict((key, int(value)) for key, value in (item.split("=", 1) for item in line.split()[1:]))
        for line in result.stderr.decode().splitlines()
        if line.startswith("pvisor-cold-sample ")
    ]
    if not samples:
        raise RuntimeError(f"pager was not exercised; see {work}")
    evidence = {
        key: max(s[key] for s in samples)
        for key in (
            "cold_bytes",
            "restored_bytes",
            "restore_count",
            "restore_total_us",
            "restore_max_us",
            "pool_rejections",
        )
    }
    if (
        evidence["cold_bytes"] < 32 * 1024 * 1024
        or evidence["restored_bytes"] < 32 * 1024 * 1024
        or evidence["pool_rejections"]
    ):
        raise RuntimeError(
            f"insufficient reclaim/restore or pool capacity rejection: {evidence}; see {work}"
        )
    bundles = list((work / "runs").glob("*/run-bundle.json"))
    if len(bundles) != 1:
        raise RuntimeError(f"expected one Bundle; see {work}")
    validate_bundle(bundles[0].parent, "vm")
    shutil.copy2(bundles[0], work / "run-bundle.json")
    shutil.rmtree(work / "runs")
    return dict(
        round=round_id,
        variant=variant,
        **readings[0],
        evidence=evidence,
        restore_average_us=evidence["restore_total_us"] / evidence["restore_count"],
        work=str(work),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "pool", "rootfs", "firmware", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--idle-seconds", type=int, default=40)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("requires Apple Silicon/HVF")
    if args.samples < 1 or args.idle_seconds < 35:
        parser.error("positive samples and idle >=35s required")
    for name in ("baseline", "candidate", "pool", "rootfs", "firmware", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(exist_ok=False)
    rows, rng = [], random.Random(20261004)
    metadata = dict(
        schema="pvisor-macos-cold-migration/v1",
        platform=platform.platform(),
        samples_per_variant=args.samples,
        idle_seconds=args.idle_seconds,
        pool_max_bytes=128 * 1024 * 1024,
        guest_data_bytes=32 * 1024 * 1024,
        metrics_enabled=True,
        host_cache="warm; no eviction",
        load_before=os.getloadavg(),
        sha256={
            str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (
                args.baseline,
                args.candidate,
                args.pool,
                Path(__file__),
                args.firmware / "libkrunfw.5.dylib",
                args.rootfs / "usr/bin/python3",
            )
        },
    )
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    with (args.output / "samples.jsonl").open("w") as log:
        for round_id in range(args.samples):
            variants = ["baseline", "candidate"]
            rng.shuffle(variants)
            for variant in variants:
                row = trial(args, variant, round_id)
                rows.append(row)
                log.write(json.dumps(row) + "\n")
                log.flush()
                print(json.dumps(row), flush=True)
    summary = {
        variant: {
            metric: {
                f"p{p}": percentile([r[metric] for r in rows if r["variant"] == variant], p)
                for p in (50, 95)
            }
            for metric in ("warm_read_ms", "cold_read_ms", "restore_average_us")
        }
        for variant in ("baseline", "candidate")
    }
    (args.output / "report.json").write_text(
        json.dumps(metadata | dict(rows=rows, summary=summary), indent=2) + "\n"
    )


if __name__ == "__main__":
    main()
