#!/usr/bin/env python3
"""Benchmark: B-FS-ENG (REGISTRY.md#b-fs-eng), engineering A/B.

Question: do PR #100's opt-in caches improve the published Python library
workloads, and does the default product regress against its base revision?
Design: reuse local_reads' exact Python/mmap scripts and OCI oracle, prepared
V2 store, three warmups and 30 randomized triplets. Fresh instance per trial,
unchanged 4-CPU/4-GiB/swap0 product budget and 2-vCPU/512-MiB guest.
"""

import argparse
import json
import os
import random
import shutil
import sys
from pathlib import Path

REPO = next(
    p
    for p in Path(__file__).resolve().parents
    if (p / "benchmark/storage/native_docker.py").is_file()
)
sys.path.insert(0, str(REPO / "benchmark/storage"))
import local_reads as reads  # noqa: E402
import native_docker as native  # noqa: E402

FLAGS = (
    "DIRECTORY_NAMES_CACHE",
    "PREPARED_COPY_UP",
    "LOGICAL_DIRECTORY_CACHE",
    "REUSE_LAZY_NODES",
    "STREAM_LAZY_READS",
    "LOGICAL_DATA_CACHE",
    "DIRECT_LAZY_COPY",
    "SCOPED_NAMES",
    "OBJECT_READ_BARRIERS",
)
ENABLED = {
    "DIRECTORY_NAMES_CACHE",
    "PREPARED_COPY_UP",
    "LOGICAL_DIRECTORY_CACHE",
    "REUSE_LAZY_NODES",
    "LOGICAL_DATA_CACHE",
    "DIRECT_LAZY_COPY",
}


def prepare(args):
    root = args.prepare.resolve()
    if ".data" not in root.parts:
        raise ValueError("evidence must be under .data")
    root.mkdir(parents=True, exist_ok=False)
    inputs = json.loads((args.original / "inputs.json").read_text())
    native.RUNTIME.save(
        root / "selected-inputs.json",
        [i for i in inputs if i["name"] == "python-3.12-slim-bookworm"],
    )
    spec = {
        "main": str(args.baseline.resolve()),
        "default": str(args.candidate.resolve()),
        "opt-in": str(args.candidate.resolve()),
    }
    binaries = {}
    for name, source in spec.items():
        directory = root / "artifacts" / name
        directory.mkdir(parents=True)
        shutil.copy2(source, directory / "pvisor")
        shutil.copy2(Path(source).parent / "libkrunfw.so.5", directory / "libkrunfw.so.5")
        binaries[name] = str(directory / "pvisor")
    store = args.original / "prepared-pvisor/pvisor"
    shutil.copytree(store / "images/pool-v2", root / "prepared-store/images/pool-v2", symlinks=True)
    shutil.copy2(store / "images.json", root / "prepared-store/images.json")
    # Relocate the copied image catalog; never write the historical source store.
    catalog = json.loads((root / "prepared-store/images.json").read_text())
    for image in catalog:
        if "rootfs" in image:
            assert image.pop("rootfs") == "", "only cache-backed images are supported"
        image["cache"]["location"] = str(root / "prepared-store/images/pool-v2")
    native.RUNTIME.save(root / "prepared-store/images.json", catalog)
    native.RUNTIME.save(
        root / "plan.json",
        {
            "benchmark_id": "B-FS-ENG",
            "role": "engineering A/B",
            "original": str(args.original.resolve()),
            "runs": args.runs,
            "warmups": 3,
            "seed": 20261103,
            "binaries": binaries,
            "binary_sha256": {k: native.sha(v) for k, v in binaries.items()},
            "baseline_revision": "1a4a8dfb8c0e09ff14586ce9325d49c80e17d445",
            "candidate_revision": "93f3274023bfb55e401fd3fc4ba1c5bac65c5ddd",
            "exclusions": "none; any oracle/resource/exit failure invalidates batch",
        },
    )
    shutil.copy2(__file__, root / "docs_retest.py")
    native.RUNTIME.save(
        root / "in.json",
        {
            "Python documentation workloads": "$ "
            + __import__("shlex").join(
                [
                    sys.executable,
                    str(root / "docs_retest.py"),
                    "--measure",
                    str(root),
                ]
            ),
        },
    )
    print(root)


def measure(root):
    plan = json.loads((root / "plan.json").read_text())
    for name, path in plan["binaries"].items():
        assert native.sha(path) == plan["binary_sha256"][name]
    args = argparse.Namespace(
        output=root / "observations",
        binary=Path(plan["binaries"]["default"]),
        inputs=root / "selected-inputs.json",
        phase="local-reads",
    )
    e = native.Evidence(args)
    shutil.copy2(
        Path(plan["binaries"]["default"]).parent / "libkrunfw.so.5", e.root / "libkrunfw.so.5"
    )
    e.save(benchmark_id="B-FS-ENG", role="engineering A/B", plan=plan)
    launcher = native.REPO / "benchmark/storage/enter_group.rs"
    e.command(["rustc", "--edition=2024", "-O", launcher, "-o", e.root / "enter-group"])
    image = e.inputs[0]
    oracle = reads.lazy.tree_oracle(image)
    payloads = {"python": reads.lazy.PYTHON_PROBE, "mmap-scan": reads.scan_probe(oracle)}
    native.RUNTIME.save(e.root / "workloads.json", payloads)
    native.RUNTIME.save(e.root / "mmap-oci-oracle.json", oracle)
    products = {}
    try:
        for name, binary in plan["binaries"].items():
            directory = e.root / ("prepared-" + name)
            directory.mkdir()
            product = native.Product(e, directory, "pvisor")
            products[name] = product
            product.prefix = [*product.launch, binary]
            shutil.copytree(root / "prepared-store", product.store)
            product.store.chmod(0o700)
            (product.store / "images").chmod(0o700)
            catalog = json.loads((product.store / "images.json").read_text())
            for entry in catalog:
                entry["cache"]["location"] = str(product.store / "images/pool-v2")
            native.RUNTIME.save(product.store / "images.json", catalog)
            product.env.update(
                {
                    "PVISOR_VM_FS_" + flag: str(int(name == "opt-in" and flag in ENABLED))
                    for flag in FLAGS
                }
            )
            product.env.update(
                PVISOR_VM_FS_WRITEBACK="0", PVISOR_VM_FS_HOST_WRITEBACK="0", PVISOR_FS_PROFILE="0"
            )
            native.RUNTIME.save(
                directory / "configuration.json",
                {
                    "binary": binary,
                    "env": product.env,
                    "budget": product.budget.read(),
                },
            )
        rng = random.Random(plan["seed"])
        records = {}
        for workload, script in payloads.items():

            def trial(phase, index):
                pair = {}
                for name in rng.sample(list(products), len(products)):
                    product = products[name]
                    directory = e.root / f"{workload}-{phase}-{index:03}-{name}"
                    directory.mkdir()
                    command = [
                        str(v)
                        for v in [
                            *product.prefix,
                            "run",
                            "--rm",
                            "--network=none",
                            "--cpus=2",
                            "--memory=512m",
                            image["pinned"],
                            "/usr/bin/taskset",
                            "-c",
                            "0,1",
                            "/bin/sh",
                            "-ec",
                            "test ! -e /tmp/pv-private; printf check >/tmp/pv-private; "
                            'test "$(cat /tmp/pv-private)" = check; exec "$@"',
                            "probe",
                            "/usr/local/bin/python",
                            "-B",
                            "-u",
                            "-c",
                            script,
                        ]
                    ]
                    env = {k: v for k, v in os.environ.items() if not k.startswith("PVISOR_")}
                    result = native.RUNTIME.capture(
                        command,
                        native.REPO,
                        env | product.env,
                        directory / "stdout",
                        timeout=120,
                    )
                    text = (directory / "stdout").read_text()
                    assert text.splitlines().count("REFERENCE_READY") == 1
                    assert not product.command(["ps", "-aq"]).strip()
                    budget = product.budget.read()
                    assert not any(budget["memory_events"].values()), budget
                    assert (product.group / "memory.swap.current").read_text().strip() == "0"
                    native.RUNTIME.save(directory / "resources.json", budget)
                    metrics = {
                        "ready_ns": result["ready_duration_ns"],
                        "exit_ns": result["exit_duration_ns"],
                    }
                    if workload == "mmap-scan":
                        rows = [
                            json.loads(s.removeprefix("READ_PHASES "))
                            for s in text.splitlines()
                            if s.startswith("READ_PHASES ")
                        ]
                        assert len(rows) == 1 and set(rows[0]) == {"first_ns", "repeat_ns"}
                        assert all(type(v) is int and v > 0 for v in rows[0].values())
                        metrics.update(rows[0])
                    pair[name] = metrics
                    native.RUNTIME.save(directory / "metrics.json", metrics)
                native.RUNTIME.save(e.root / f"{workload}-{phase}-{index:03}.json", pair)
                print(workload, phase, index, pair, file=sys.stderr, flush=True)
                return pair

            records[workload] = native.RUNTIME.repeat(plan["warmups"], plan["runs"], trial)
            native.RUNTIME.save(e.root / "pairs.json", records)
        e.save(status="complete")
        print(json.dumps({"N triplets": plan["runs"]}))
    except BaseException as error:
        e.save(status="failed", error=repr(error))
        raise
    finally:
        for product in products.values():
            product.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path)
    parser.add_argument("--measure", type=Path)
    parser.add_argument("--original", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--runs", type=int, default=30)
    args = parser.parse_args()
    if args.prepare:
        prepare(args)
    else:
        measure(args.measure.resolve())
