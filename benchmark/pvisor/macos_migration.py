#!/usr/bin/env python3
"""Interleaved, same-host CLI/VM migration comparison on Apple Silicon.

Prepared Alpine rootfs, identical firmware and warm host caches. Timings include
CLI setup and shutdown; successful output and completed VM Bundles are required.
Large evidence stays in target/, outside the source tree.
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
import threading
import time
from pathlib import Path

from startup import percentile, validate_bundle

MARKER = b"PVISOR_BENCH_READY"
CASES = {
    "startup-1cpu-128": (1, 128, False, ":"),
    "startup-2cpu-128": (2, 128, False, ":"),
    "startup-2cpu-2048": (2, 2048, False, ":"),
    "metadata-2048": (2, 256, False, 'test "$(find fixture -type f | wc -l)" -eq 2048'),
    "search-2048": (2, 256, False, 'test "$(grep -rl needle fixture | wc -l)" -eq 32'),
    "write-read-32mib": (
        2,
        256,
        False,
        "dd if=/dev/zero of=data bs=65536 count=512 2>/dev/null; cp data copy; cmp data copy; test $(wc -c < copy) -eq 33554432",
    ),
    "repair-test-diff": (
        2,
        256,
        False,
        "printf 'value=wrong\\n' > config; cp config original; sed -i 's/wrong/correct/' config; . ./config; test \"$value\" = correct; diff original config > changes && exit 1; grep -q correct changes",
    ),
    "compressed-startup": (2, 256, True, ":"),
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def trial(args, variant, case, batch, round_id):
    cpus, memory, compressed, payload = CASES[case]
    work = args.output / "trials" / f"b{batch}-{round_id}-{case}-{variant}"
    work.mkdir(parents=True)
    (work / "config-home").mkdir()
    if case in ("metadata-2048", "search-2048"):
        shutil.copytree(args.output / "fixture", work / "fixture")
    env = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR") if k in os.environ}
    run_home = args.output / "run-storage" / work.name
    env.update(
        PVISOR_RUN_HOME=str(run_home),
        XDG_CONFIG_HOME=str(work / "config-home"),
        PVISOR_STARTUP_TIMING="0",
        PVISOR_PERSISTENCE_TIMING="0",
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
        "30s",
        "--vm",
        "--rootfs",
        str(args.rootfs),
        "--vm-library-dir",
        str(args.firmware),
        "--cpu",
        str(cpus),
        "--memory",
        f"{memory}MiB",
    ]
    if compressed:
        command.append("--vm-ram-compression")
    command += ["--", "/bin/sh", "-ec", payload + "; printf 'PVISOR_BENCH_READY\\n'"]
    ready, ended, stdout, stderr = [], [], [], []
    done = threading.Event()
    started = time.monotonic_ns()
    process = subprocess.Popen(
        command,
        cwd=work,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )

    def read_stdout():
        for line in process.stdout:
            stdout.append(line)
            if line.strip() == MARKER:
                ready.append(time.monotonic_ns())

    def wait():
        # Blocking wait records real exit; wait(timeout=...) polls in Python and
        # can add a 50-ms quantization error to completion latency.
        process.wait()
        ended.append(time.monotonic_ns())
        done.set()

    readers = [
        threading.Thread(target=read_stdout),
        threading.Thread(target=lambda: stderr.append(process.stderr.read())),
        threading.Thread(target=wait),
    ]
    for reader in readers:
        reader.start()
    try:
        if not done.wait(45):
            os.killpg(process.pid, signal.SIGKILL)
            raise TimeoutError(f"trial exceeded 45s; retained at {work}")
    finally:
        for reader in readers:
            reader.join()
        process.stdout.close()
        process.stderr.close()
        (work / "stdout.log").write_bytes(b"".join(stdout))
        (work / "stderr.log").write_bytes(b"".join(stderr))
    if process.returncode != 0 or len(ready) != 1:
        raise RuntimeError(f"failed {case}/{variant}, exit={process.returncode}; see {work}")
    bundles = [
        Path(line.removeprefix("Run Bundle: ")).parent
        for line in b"".join(stderr).decode(errors="replace").splitlines()
        if line.startswith("Run Bundle: ")
    ]
    if len(bundles) != 1:
        raise RuntimeError(f"expected one Bundle; see {work}")
    isolation = validate_bundle(bundles[0], "vm")
    shutil.copy2(next(bundles[0].glob("**/run-bundle.json")), work / "run-bundle.json")
    # Keep logs/Bundle evidence; remove only owned generated payloads after checks.
    for name in ("fixture", "data", "copy"):
        path = work / name
        if path.is_dir():
            shutil.rmtree(path)
        elif path.exists():
            path.unlink()
    if not bundles[0].resolve().is_relative_to(run_home):
        raise RuntimeError(f"unexpected storage outside benchmark-owned directory: {bundles[0]}")
    shutil.rmtree(run_home)
    return dict(
        batch=batch,
        round=round_id,
        variant=variant,
        case=case,
        ready_ms=(ready[0] - started) / 1e6,
        completion_ms=(ended[0] - started) / 1e6,
        observed_isolation=isolation,
        exit=process.returncode,
        work=str(work),
    )


def summarize(rows, threshold):
    results = {}
    rng = random.Random(20261004)
    for case in sorted({row["case"] for row in rows}):
        result = {}
        for metric in ("ready_ms", "completion_ms"):
            groups = {
                v: [r[metric] for r in rows if r["case"] == case and r["variant"] == v]
                for v in ("baseline", "candidate")
            }
            distributions = {
                v: {f"p{p}": percentile(values, p) for p in (50, 95, 99)}
                for v, values in groups.items()
            }
            changes = {
                f"p{p}": (
                    distributions["candidate"][f"p{p}"] / distributions["baseline"][f"p{p}"] - 1
                )
                * 100
                for p in (50, 95, 99)
            }
            paired = [(b, c) for b, c in zip(groups["baseline"], groups["candidate"], strict=True)]
            boots = []
            for _ in range(2000):
                sample = rng.choices(paired, k=len(paired))
                boots.append(
                    (
                        percentile([c for _, c in sample], 50)
                        / percentile([b for b, _ in sample], 50)
                        - 1
                    )
                    * 100
                )
            result[metric] = dict(
                distributions=distributions,
                change_percent=changes,
                paired_bootstrap_p50_change_ci95=[percentile(boots, 2.5), percentile(boots, 97.5)],
                regression=any(changes[k] > threshold for k in ("p50", "p95")),
            )
        results[case] = result
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("baseline", "candidate", "rootfs", "firmware", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--cases", default=",".join(CASES))
    parser.add_argument("--regression-threshold", type=float, default=15)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("requires Apple Silicon/HVF")
    if args.samples < 1 or args.warmups < 0 or args.batches < 1:
        parser.error("invalid samples/warmups/batches")
    selected = args.cases.split(",")
    if len(selected) != len(set(selected)) or any(case not in CASES for case in selected):
        parser.error("cases must be unique names from: " + ",".join(CASES))
    for name in ("baseline", "candidate", "rootfs", "firmware", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.output.mkdir(exist_ok=False)
    fixture = args.output / "fixture"
    fixture.mkdir()
    for i in range(2048):
        directory = fixture / f"d{i // 64:02}"
        directory.mkdir(exist_ok=True)
        (directory / f"f{i:04}.txt").write_text(
            ("needle" if i % 64 == 0 else "ordinary") + " payload\n"
        )
    metadata = dict(
        schema="pvisor-macos-migration/v1",
        platform=platform.platform(),
        protocol=dict(
            samples=args.samples,
            warmups=args.warmups,
            batches=args.batches,
            threshold_percent=args.regression_threshold,
            seed=20261004,
            exit_timer="blocking wait thread; timeout only on completion event",
            host_cache="warm; no eviction",
            cases={k: CASES[k] for k in selected},
        ),
        load_before=os.getloadavg(),
        sha256={
            str(p): sha(p)
            for p in (
                args.baseline,
                args.candidate,
                args.rootfs / "bin/busybox",
                args.firmware / "libkrunfw.5.dylib",
                Path(__file__),
            )
        },
    )
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rng, rows = random.Random(20261004), []
    with (args.output / "samples.jsonl").open("w") as log:
        for batch in range(args.batches):
            for round_id in range(-args.warmups, args.samples):
                cases = selected.copy()
                rng.shuffle(cases)
                for case in cases:
                    variants = ["baseline", "candidate"]
                    rng.shuffle(variants)
                    for variant in variants:
                        row = trial(args, variant, case, batch, round_id)
                        if round_id >= 0:
                            rows.append(row)
                            log.write(json.dumps(row) + "\n")
                            log.flush()
                print(
                    f"batch {batch + 1}/{args.batches}, round {round_id + 1}/{args.samples}",
                    flush=True,
                )
    # Report each batch separately; never pool percentiles across batches.
    summaries = {
        str(b): summarize([r for r in rows if r["batch"] == b], args.regression_threshold)
        for b in range(args.batches)
    }
    report = metadata | dict(batches=summaries, load_after=os.getloadavg(), samples=rows)
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    regressions = [
        (b, case, metric)
        for b, cases in summaries.items()
        for case, metrics in cases.items()
        for metric, values in metrics.items()
        if values["regression"]
    ]
    print(json.dumps(dict(regressions=regressions), indent=2))
    return bool(regressions)


if __name__ == "__main__":
    raise SystemExit(main())
