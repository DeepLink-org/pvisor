#!/usr/bin/env python3
"""Interleaved, same-host CLI/VM migration comparison on Apple Silicon.

Benchmark: B-MACOS-ENG (benchmark/README.md#b-macos-eng), role engineering A/B.
Motivation: developers need to detect regressions in a macOS CLI migration.
Conclusion sought: paired median differences between frozen CLI versions, with confidence intervals.
Design: interleaved runs, prepared rootfs, identical firmware, warm caches.

Prepared Alpine rootfs, identical firmware and warm host caches. Timings include
CLI setup and shutdown; successful output and completed VM Bundles are required.
Raw evidence stays in a local .data/ directory.
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
    "metadata-deep-2048": (2, 256, False, 'test "$(find fixture -type f | wc -l)" -eq 2048'),
    "search-deep-2048": (2, 256, False, 'test "$(grep -rl needle fixture | wc -l)" -eq 32'),
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
DEFAULT_CASES = tuple(CASES)
CASES.update(
    {
        "rg-2048": (2, 256, False, 'test "$(rg --no-config -l needle fixture | wc -l)" -eq 32'),
        "read-parallel-4-64mib": (
            4,
            256,
            False,
            "pids=''; for worker in 0 1 2 3; do "
            '(test "$(stat -c %s fixture/data$worker)" -eq 16777216; '
            "dd if=fixture/data$worker of=/dev/null bs=65536 2>/dev/null) & "
            'pids="$pids $!"; done; for pid in $pids; do wait "$pid"; done',
        ),
        "rg-parallel-4": (
            4,
            256,
            False,
            "pids=''; for worker in 0 1 2 3; do "
            '(test "$(rg --threads 1 --no-config -l needle fixture/q$worker | wc -l)" -eq 8) & '
            'pids="$pids $!"; done; for pid in $pids; do wait "$pid"; done',
        ),
        "rg-deep-2048": (
            2,
            256,
            False,
            'test "$(rg --no-config -l needle fixture | wc -l)" -eq 32',
        ),
        "rg-deep-partial-upper-2048": (
            2,
            256,
            False,
            'for d in fixture/d*; do mkdir -p "$d/l1/new-stage"; done; '
            'test "$(find fixture -type f | wc -l)" -eq 2048; '
            'test "$(rg --no-config -l needle fixture | wc -l)" -eq 32',
        ),
        "git-status-2048": (
            2,
            256,
            False,
            'result="$(git -c safe.directory=\'*\' -C fixture status --porcelain --untracked-files=all)"; test -z "$result"',
        ),
        "npm-offline-32": (
            2,
            256,
            False,
            'cd fixture; npm install --offline --ignore-scripts --no-audit --no-fund --package-lock=false --cache /tmp/pvisor-npm-cache; node -e \'const fs=require("fs"); if(fs.readdirSync("node_modules").filter(n=>!n.startsWith(".")).length!==32)process.exit(1); for(let i=0;i<32;i++){if(require("p"+i)!==i)process.exit(2)}\'',
        ),
    }
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def create_fixture(root, *, depth, files=2048):
    """One match every 64 files; directory depth does not change contents."""
    root.mkdir()
    for i in range(files):
        directory = root / f"d{i // 64:02}"
        for level in range(1, depth):
            directory /= f"l{level}"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"f{i:04}.txt").write_text(
            ("needle" if i % 64 == 0 else "ordinary") + " payload\n"
        )


def create_parallel_fixture(root):
    """Four disjoint 512-file quarters: equal total work, no shared guest pages."""
    root.mkdir()
    for quarter in range(4):
        create_fixture(root / f"q{quarter}", depth=1, files=512)


def create_git_fixture(root):
    create_fixture(root, depth=1)
    env = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR") if k in os.environ}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    prefix = ["git", "-c", "core.hooksPath=/dev/null"]
    for args in (
        ["init", "-q", "--template="],
        ["add", "."],
        [
            "-c",
            "user.name=Benchmark",
            "-c",
            "user.email=benchmark@invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ):
        subprocess.run(prefix + args, cwd=root, env=env, check=True, stdout=subprocess.DEVNULL)


def create_npm_fixture(root):
    root.mkdir()
    dependencies = {}
    for i in range(32):
        package = root / "local" / f"p{i}"
        package.mkdir(parents=True)
        (package / "package.json").write_text(
            json.dumps(dict(name=f"p{i}", version="1.0.0", main="m0.js"))
        )
        for j in range(16):
            (package / f"m{j}.js").write_text(f"module.exports = {i + j};\n")
        dependencies[f"p{i}"] = f"file:local/p{i}"
    (root / "package.json").write_text(
        json.dumps(dict(name="fixture", version="1.0.0", dependencies=dependencies))
    )


def measure_process(command, *, cwd, env, work):
    ready, ended, stdout, stderr = [], [], [], []
    done = threading.Event()
    started = time.monotonic_ns()
    process = subprocess.Popen(
        command,
        cwd=cwd,
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
        raise RuntimeError(f"failed process, exit={process.returncode}; see {work}")
    return (
        (ready[0] - started) / 1e6,
        (ended[0] - started) / 1e6,
        stdout,
        stderr,
        process.returncode,
    )


def trial(args, variant, case, batch, round_id):
    cpus, memory, compressed, payload = CASES[case]
    work = args.output / "trials" / f"b{batch}-{round_id}-{case}-{variant}"
    work.mkdir(parents=True)
    (work / "config-home").mkdir()
    if case in (
        "metadata-2048",
        "search-2048",
        "metadata-deep-2048",
        "search-deep-2048",
        "rg-2048",
        "rg-parallel-4",
        "read-parallel-4-64mib",
        "rg-deep-2048",
        "rg-deep-partial-upper-2048",
        "git-status-2048",
        "npm-offline-32",
    ):
        fixture_name = (
            "fixture-read"
            if case == "read-parallel-4-64mib"
            else "fixture-parallel"
            if case == "rg-parallel-4"
            else "fixture-git"
            if case == "git-status-2048"
            else "fixture-npm"
            if case == "npm-offline-32"
            else "fixture-deep"
            if "-deep-" in case
            else "fixture"
        )
        shutil.copytree(args.output / fixture_name, work / "fixture")
    env = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR") if k in os.environ}
    run_home = args.output / "run-storage" / work.name
    diagnostic_timing = getattr(args, "diagnostic_timing", False)
    env.update(
        PVISOR_RUN_HOME=str(run_home),
        XDG_CONFIG_HOME=str(work / "config-home"),
        PVISOR_STARTUP_TIMING="1" if diagnostic_timing else "0",
        PVISOR_PERSISTENCE_TIMING="1" if diagnostic_timing else "0",
        PVISOR_FS_PROFILE="1" if getattr(args, "filesystem_profile", False) else "0",
    )
    workers = getattr(args, f"{variant}_fs_workers", None)
    if workers is not None:
        env["PVISOR_VM_FS_WORKERS"] = str(workers)
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
    ready_ms, completion_ms, stdout, stderr, exit_code = measure_process(
        command, cwd=work, env=env, work=work
    )
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
        ready_ms=ready_ms,
        completion_ms=completion_ms,
        observed_isolation=isolation,
        exit=exit_code,
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
    for variant in ("baseline", "candidate"):
        parser.add_argument(f"--{variant}-fs-workers", type=int, choices=range(1, 9))
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--batches", type=int, default=3)
    parser.add_argument("--cases", default=",".join(DEFAULT_CASES))
    parser.add_argument("--regression-threshold", type=float, default=15)
    parser.add_argument("--seed", type=int, default=20261004)
    parser.add_argument(
        "--diagnostic-timing",
        action="store_true",
        help="retain CLI startup/persistence stage timings; report as an instrumented diagnostic",
    )
    parser.add_argument(
        "--filesystem-profile",
        action="store_true",
        help="retain aggregate filesystem checkpoints; instrumented runs are not performance acceptance",
    )
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
    create_fixture(args.output / "fixture", depth=1)
    if any("-deep-" in case for case in selected):
        create_fixture(args.output / "fixture-deep", depth=8)
    if "read-parallel-4-64mib" in selected:
        root = args.output / "fixture-read"
        root.mkdir()
        for worker in range(4):
            with (root / f"data{worker}").open("wb") as file:
                file.truncate(16 * 1024 * 1024)
    if "rg-parallel-4" in selected:
        create_parallel_fixture(args.output / "fixture-parallel")
    if "git-status-2048" in selected:
        create_git_fixture(args.output / "fixture-git")
    if "npm-offline-32" in selected:
        create_npm_fixture(args.output / "fixture-npm")
    metadata = dict(
        schema="pvisor-macos-migration/v1",
        platform=platform.platform(),
        protocol=dict(
            samples=args.samples,
            warmups=args.warmups,
            batches=args.batches,
            threshold_percent=args.regression_threshold,
            seed=args.seed,
            diagnostic_timing=args.diagnostic_timing,
            filesystem_profile=args.filesystem_profile,
            fs_workers={
                variant: getattr(args, f"{variant}_fs_workers")
                for variant in ("baseline", "candidate")
            },
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
    tools = {"rg": "usr/bin/rg", "git": "usr/bin/git", "node": "usr/bin/node", "npm": "usr/bin/npm"}
    metadata["guest_tools_sha256"] = {
        name: sha(args.rootfs / relative)
        for name, relative in tools.items()
        if (args.rootfs / relative).is_file()
    }
    packages = args.rootfs / "lib/apk/db/installed"
    if packages.is_file():
        metadata["rootfs_packages_sha256"] = sha(packages)
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rng, rows = random.Random(args.seed), []
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
