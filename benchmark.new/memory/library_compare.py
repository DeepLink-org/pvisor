#!/usr/bin/env python3
"""Benchmark: B-MEMORY-SCALE, role engineering A/B.

Question: do successful-file-READ hints accelerate actual cross-VM dlopen page
sharing, with what whole-group cost relative to Firecracker and default KSM?
Seven preregistered paths, common Python userspace/64MiB library/full SHA/COW
oracle, four 256MiB/1vCPU VMs, 4CPU/2GiB/swap0, 60s idle/30s write settling.
Hints-off/on use the same binary; DAX is an explicit attachment reference.
Out-of-group page identity audit runs after timed first readback. No host sysfs writes.
"""

import argparse
import json
import random
import shlex
import shutil
import sys
from pathlib import Path

from library_protocol import VARIANTS
from pool_compare import (
    REPO,
    attempt,
    digest,
    expected,
    host_identity,
    kernel_state,
    require_default_ksm,
    save,
    verify_harness,
)
from prepare_pool import tree_manifest


def verify_inputs(inputs, receipt):
    if any(digest(inputs / name) != checksum for name, checksum in receipt["hashes"].items()):
        raise ValueError("frozen input bytes changed")
    retained = json.loads((inputs / "python-root-manifest.json").read_text())
    if tree_manifest(inputs / "python-root") != retained:
        raise ValueError("Python rootfs content/mode/link manifest changed")
    if (
        digest(inputs / "python-root/libraries/libpayload.so")
        != receipt["hashes"]["libraries/libpayload.so"]
    ):
        raise ValueError("rootfs and DAX library copies differ")
    if digest(inputs / "python-root/library-worker.py") != receipt["hashes"]["library_worker.py"]:
        raise ValueError("block and virtio-fs worker bytes differ")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--docker-inputs", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--preflight", action="store_true")
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--variants", default=",".join(VARIANTS))
    args = parser.parse_args()
    variants = args.variants.split(",")
    if not variants or len(set(variants)) != len(variants) or not set(variants) <= set(VARIANTS):
        parser.error("invalid library variant selection")
    if not args.preflight and (args.samples < 5 or set(variants) != set(VARIANTS)):
        parser.error("formal library cohort requires all variants and at least five rounds")
    inputs, output = args.inputs.resolve(), args.output.resolve()
    docker_inputs = args.docker_inputs.resolve() if args.docker_inputs else None
    if "docker-rootless-library" in variants and docker_inputs is None:
        parser.error("Docker variant requires --docker-inputs")
    if args.prepare:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "--inputs",
            str(inputs),
            "--output",
            str(output),
            "--samples",
            str(args.samples),
            "--variants",
            args.variants,
        ]
        if args.preflight:
            command.append("--preflight")
        if docker_inputs:
            command += ["--docker-inputs", str(docker_inputs)]
        destination = output.with_suffix(".input.json")
        with destination.open("x") as stream:
            json.dump({"library-curves": "$ " + shlex.join(command)}, stream, indent=2)
            stream.write("\n")
        print(destination)
        return 0
    output.mkdir()
    require_default_ksm(kernel_state())
    receipt = json.loads((inputs / "receipt.json").read_text())
    required = (
        "library.ext4",
        "python-root-manifest.json",
        "payload",
        "library_worker.py",
        "libraries/libpayload.so",
        "firecracker",
        "firecracker-filter.bpf",
    )
    if not set(required) <= set(receipt["hashes"]):
        raise ValueError(
            "library cohort requires complete frozen userspace and stock Firecracker inputs"
        )
    verify_inputs(inputs, receipt)
    hashes = {
        f"{n}:{percent}": expected("random-shared", n, percent)
        for n in range(1, 5)
        for percent in (0, 25, 100)
    }
    paths = [
        Path(__file__).with_name(name)
        for name in (
            "library_compare.py",
            "library_protocol.py",
            "library_evidence.py",
            "prepare_pool.py",
            "pool_compare.py",
            "scanner_compare.py",
            "docker_support.py",
        )
    ]
    paths += [
        REPO / "benchmark/pvisor" / name
        for name in ("firecracker_ksm.py", "memory_savings.py", "linux_cold_runtime.py")
    ]
    harness = {str(path.resolve()): digest(path) for path in paths}
    frozen = output / "harness"
    frozen.mkdir()
    for path in paths:
        shutil.copy2(path, frozen / path.name)
    report = dict(
        benchmark_id="B-MEMORY-SCALE",
        role="engineering A/B",
        campaign="library",
        primary_baseline="firecracker-block",
        preflight=args.preflight,
        samples=1 if args.preflight else args.samples,
        arms=variants,
        patterns=["random-shared"],
        receipt=receipt,
        harness_sha256=digest(Path(__file__).with_name("pool_compare.py")),
        harness_files=harness,
        host=host_identity(),
        attempts=[],
        complete=False,
        caveats=[
            "different guest kernels, block/virtio-fs root and guest supervisor",
            "Python userspace identical; source inputs prepared and cache retained",
            "prepared source cache may be charged outside the product cgroup; PSS includes mapped DAX pages, charged memory is not whole-host savings",
            "DAX requires explicit immutable attachment; not automatic normal-rootfs DAX",
            "file READ hints select candidates only; merge authority still uses full live bytes",
            "page identity audit after timed first readback, outside product cgroup; not net saved memory",
            "ksmd global CPU is outside quota, not exactly attributable",
            "scanners may work during sequential startup",
        ],
    )
    if docker_inputs:
        from docker_support import verify_inputs as verify_docker_inputs

        report["docker_receipt"] = json.loads((docker_inputs / "receipt.json").read_text())
        verify_docker_inputs(docker_inputs, report["docker_receipt"])
        if report["docker_receipt"]["pool_receipt_sha256"] != digest(inputs / "receipt.json"):
            raise ValueError("Docker and VM frozen source inputs differ")
        report["caveats"].append(
            "rootless Docker includes a dedicated daemon/runtime and shares the host kernel; same frozen library/userspace, full slice accounting"
        )
    save(output / "report.json", report)
    for round_index in range(report["samples"]):
        order = variants.copy()
        random.Random(20261019 + round_index).shuffle(order)
        for variant in order:
            try:
                if not verify_harness(harness):
                    raise ValueError("measurement source changed between conditions")
                verify_inputs(inputs, receipt)
            except Exception as error:
                report["error"] = str(error)
                save(output / "report.json", report)
                return 1
            arm = VARIANTS[variant][0]
            row = attempt(
                inputs,
                output / str(len(report["attempts"])),
                arm,
                "random-shared",
                hashes,
                harness,
                library_variant=variant,
                docker_inputs=docker_inputs,
            )
            row.update(round=round_index, library_variant=variant)
            report["attempts"].append(row)
            save(output / "report.json", report)
            print(f"round={round_index} {variant}: {row['status']}", file=sys.stderr, flush=True)
            if row["status"] != "successful":
                print(row.get("error"), file=sys.stderr)
                return 1
    verify_inputs(inputs, receipt)
    if docker_inputs:
        verify_docker_inputs(docker_inputs, report["docker_receipt"])
    report["complete"] = verify_harness(harness)
    save(output / "report.json", report)
    print(
        json.dumps(
            dict(
                complete=report["complete"],
                conditions=len(report["attempts"]),
                evidence=str(output),
            )
        )
    )
    return 0 if report["complete"] else 1


if __name__ == "__main__":
    sys.exit(main())
