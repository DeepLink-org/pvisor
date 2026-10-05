#!/usr/bin/env python3
"""Paired macOS VM/Docker host-bind tool workloads with independent worker timing."""

import argparse
import json
import math
import os
from pathlib import Path
import platform
import random
import shlex
import shutil
import subprocess
import uuid

import macos_migration as bench

WORKER = """const {spawnSync}=require("child_process");
const started=process.hrtime.bigint();
const result=spawnSync("/bin/sh",["-ec",process.argv[1]],{encoding:"utf8",maxBuffer:16*1024*1024});
const elapsed=Number(process.hrtime.bigint()-started)/1e6;
if(result.stdout)process.stdout.write(result.stdout);
if(result.stderr)process.stderr.write(result.stderr);
if(result.error)throw result.error;
if(result.status!==0)process.exit(result.status===null?125:result.status);
console.log("\\nPVISOR_TOOL_WORKER "+JSON.stringify({case:process.argv[2],worker_ms:elapsed}));"""
TOOL_CASES = ("rg-2048", "rg-deep-2048", "git-status-2048", "npm-offline-32")


def worker_result(stdout, case):
    records = [
        json.loads(line.removeprefix("PVISOR_TOOL_WORKER "))
        for line in stdout.decode().splitlines()
        if line.startswith("PVISOR_TOOL_WORKER ")
    ]
    if len(records) != 1 or records[0].get("case") != case:
        raise ValueError("expected exactly one matching worker record")
    value = records[0]["worker_ms"]
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or not math.isfinite(value)
        or value < 0
    ):
        raise ValueError("invalid worker duration")
    return value


def docker_trial(args, case, round_id):
    work = args.output / "trials" / f"b0-{round_id}-{case}-docker"
    work.mkdir(parents=True)
    source = (
        "fixture-git"
        if case == "git-status-2048"
        else "fixture-npm"
        if case == "npm-offline-32"
        else "fixture-deep"
        if "-deep-" in case
        else "fixture"
    )
    shutil.copytree(args.output / source, work / "fixture")
    cpus, memory, _, payload = bench.CASES[case]
    name = "pvisor-fsbench-" + uuid.uuid4().hex
    command = [
        "docker",
        "run",
        "--rm",
        "--pull=never",
        "--name",
        name,
        "--network",
        "none",
        "--cpus",
        str(cpus),
        "--memory",
        f"{memory}m",
        "--memory-swap",
        f"{memory}m",
        "--mount",
        f"type=bind,src={work},dst=/work",
        "--workdir",
        "/work",
        args.image,
        "/bin/sh",
        "-ec",
        payload + "; printf 'PVISOR_BENCH_READY\\n'",
    ]
    try:
        ready, completion, stdout, _, exit_code = bench.measure_process(
            command, cwd=work, env=os.environ.copy(), work=work
        )
        worker = worker_result(b"".join(stdout), case)
    finally:
        # A killed client does not necessarily stop its daemon-owned container.
        subprocess.run(
            ["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
    shutil.rmtree(work / "fixture")
    return dict(
        batch=0,
        round=round_id,
        variant="docker",
        case=case,
        ready_ms=ready,
        completion_ms=completion,
        worker_ms=worker,
        observed_isolation="docker_container",
        exit=exit_code,
        work=str(work),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("vm", "rootfs", "firmware", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--seed", type=int, default=20261012)
    parser.add_argument("--cases", default=",".join(TOOL_CASES))
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("requires Apple Silicon/HVF and local ARM64 Docker")
    selected = args.cases.split(",")
    if (
        args.samples < 1
        or args.warmups < 0
        or len(set(selected)) != len(selected)
        or any(c not in TOOL_CASES for c in selected)
    ):
        parser.error("invalid samples, warmups or tool cases")
    for name in ("vm", "rootfs", "firmware", "output"):
        setattr(args, name, getattr(args, name).resolve())
    args.image = subprocess.check_output(
        ["docker", "image", "inspect", args.image, "--format", "{{.Id}}"], text=True
    ).strip()
    args.output.mkdir(exist_ok=False)
    bench.create_fixture(args.output / "fixture", depth=1)
    bench.create_fixture(args.output / "fixture-deep", depth=8)
    bench.create_git_fixture(args.output / "fixture-git")
    bench.create_npm_fixture(args.output / "fixture-npm")
    originals = {case: bench.CASES[case] for case in selected}
    for case, (cpus, memory, compressed, payload) in originals.items():
        wrapper = (
            "node -e "
            + shlex.quote(WORKER)
            + " -- "
            + shlex.quote(payload)
            + " "
            + shlex.quote(case)
        )
        bench.CASES[case] = (cpus, memory, compressed, wrapper)
    metadata = dict(
        schema="pvisor-macos-docker-tools/v1",
        platform=platform.platform(),
        image=args.image,
        docker=subprocess.check_output(
            [
                "docker",
                "version",
                "--format",
                "{{.Server.Version}} {{.Server.Os}} {{.Server.Arch}}",
            ],
            text=True,
        ).strip(),
        protocol=dict(
            samples=args.samples,
            warmups=args.warmups,
            seed=args.seed,
            cases=originals,
            workspace="macOS host bind: Docker /work; pVisor staged virtio-fs",
            cache="warm host; fresh copied fixture and npm cache per trial",
            worker="Node hrtime around synchronous shell+tool+result validation; excludes observer startup",
            diagnostics=False,
        ),
        load_before=os.getloadavg(),
        sha256={
            str(p): bench.sha(p)
            for p in [
                args.vm,
                args.firmware / "libkrunfw.5.dylib",
                Path(__file__),
                Path(bench.__file__),
            ]
        },
        guest_tools={
            name: bench.sha(args.rootfs / f"usr/bin/{name}")
            for name in ("rg", "git", "node", "npm")
        },
    )
    docker_hashes = subprocess.check_output(
        [
            "docker",
            "run",
            "--rm",
            "--pull=never",
            "--network",
            "none",
            args.image,
            "sha256sum",
            "/usr/bin/rg",
            "/usr/bin/git",
            "/usr/bin/node",
            "/usr/bin/npm",
        ],
        text=True,
    )
    metadata["docker_tools"] = {
        Path(path).name: digest
        for digest, path in (line.split() for line in docker_hashes.splitlines())
    }
    if metadata["docker_tools"] != metadata["guest_tools"]:
        raise ValueError("Docker and guest tool binaries differ")
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rows = []
    rng = random.Random(args.seed)
    with (args.output / "samples.jsonl").open("w") as log:
        for round_id in range(-args.warmups, args.samples):
            cases = selected.copy()
            rng.shuffle(cases)
            for case in cases:
                variants = ["vm", "docker"]
                rng.shuffle(variants)
                for variant in variants:
                    if variant == "docker":
                        row = docker_trial(args, case, round_id)
                    else:
                        row = bench.trial(args, "vm", case, 0, round_id)
                        row["worker_ms"] = worker_result(
                            (Path(row["work"]) / "stdout.log").read_bytes(), case
                        )
                    if round_id >= 0:
                        rows.append(row)
                        log.write(json.dumps(row) + "\n")
                        log.flush()
            print(f"round {round_id + 1}/{args.samples}", flush=True)
    # Reuse paired distribution logic with Docker as baseline; ratio, not a
    # migration pass/fail gate. Keep end-to-end and worker scopes separate.
    normalized = [
        row | {"variant": "baseline" if row["variant"] == "docker" else "candidate"} for row in rows
    ]
    summary = bench.summarize(normalized, 15)
    worker_summary = bench.summarize(
        [
            row | {"ready_ms": row["worker_ms"], "completion_ms": row["worker_ms"]}
            for row in normalized
        ],
        15,
    )
    for case in summary:
        summary[case]["worker_ms"] = worker_summary[case]["ready_ms"]
        for value in summary[case].values():
            value["vm_over_docker_p50"] = (
                value["distributions"]["candidate"]["p50"]
                / value["distributions"]["baseline"]["p50"]
            )
            value.pop("regression", None)
    report = metadata | dict(samples=rows, summary=summary, load_after=os.getloadavg())
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                case: {metric: value["vm_over_docker_p50"] for metric, value in metrics.items()}
                for case, metrics in summary.items()
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
