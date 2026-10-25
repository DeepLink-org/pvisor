#!/usr/bin/env python3
"""Benchmark: B-LAZY-STARTUP (REGISTRY.md#b-lazy-startup), user-facing.

Question: after complete image preparation, what do Python startup and first /
repeated same-instance mmap scans cost with V2 versus native Docker?
Design: same pinned image, native product-group budgets, new instances,
3 warmups/30 randomized pairs; semspec Ready clocks, OCI-derived read oracle.
Guest/container internal read clocks remain separate from host Ready clocks.
"""

import argparse
import datetime
import json
import os
import random
import shutil
from pathlib import Path

import lazy_benefits as lazy
import native_docker as native

SAVE = native.RUNTIME.save


def scan_probe(oracle):
    probe = lazy.MMAP_PROBE.replace("EXPECTED_ORACLE", repr(oracle))
    prefix, body = probe.split("root='/usr/local/lib/python3.12'", 1)
    body = "root='/usr/local/lib/python3.12'" + body
    body = body.removesuffix("print('REFERENCE_READY',flush=True)\n")
    return (
        prefix
        + "import time,json\n"
        + "\n".join(
            [
                "timings={}",
                "for phase in ['first_ns','repeat_ns']:",
                " start=time.perf_counter_ns()",
            ]
            + [" " + line for line in body.splitlines()]
            + [
                " timings[phase]=time.perf_counter_ns()-start",
                "print('READ_PHASES '+json.dumps(timings),flush=True)",
                "print('REFERENCE_READY',flush=True)",
            ]
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=30)
    args = parser.parse_args()
    if args.runs < 2:
        parser.error("at least two pairs are required")
    args.phase = "local-reads"
    e = native.Evidence(args)
    sources = [Path(__file__), Path(lazy.__file__), Path(native.__file__), native.RUNTIME_PATH]
    frozen = {}
    for path in sources:
        shutil.copy2(path, e.root / path.name)
        frozen[str(path)] = native.sha(path)
    e.save(benchmark_id="B-LAZY-STARTUP", phase="local-reads", sources=frozen)
    image = next(i for i in e.inputs if i["name"] == "python-3.12-slim-bookworm")
    oracle = lazy.tree_oracle(image)
    payloads = {"python": lazy.PYTHON_PROBE, "mmap-scan": scan_probe(oracle)}
    SAVE(e.root / "workloads.json", payloads)
    SAVE(e.root / "mmap-oci-oracle.json", oracle)
    launcher = native.REPO / "benchmark/storage/enter_group.rs"
    shutil.copy2(launcher, e.root / launcher.name)
    e.command(["rustc", "--edition=2024", "-O", launcher, "-o", e.root / "enter-group"])
    products = {}
    try:
        for backend in ["docker", "pvisor"]:
            directory = e.root / ("prepared-" + backend)
            directory.mkdir()
            product = native.Product(e, directory, backend)
            products[backend] = product
            native.import_image(e, image, backend, product.prefix, product.env)
            initial = product.budget.read()
            SAVE(directory / "prepared-resources.json", initial)
            assert not any(initial["memory_events"].values()), initial["memory_events"]
        e.save(
            status="measuring",
            runs=args.runs,
            warmups=3,
            budget="4 CPU/4 GiB/swap0 product group; 2 CPU/512 MiB per instance; CPUs0-3 clients/helpers, payload host0,1 or guest0,1",
        )
        rng = random.Random(20261026)
        all_pairs, summaries = {}, {}
        for workload, script in payloads.items():

            def trial(phase, index):
                pair = {}
                for backend in rng.sample(["docker", "pvisor"], 2):
                    product = products[backend]
                    directory = e.root / f"{workload}-{phase}-{index:03}-{backend}"
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
                            'test ! -e /tmp/pv-private; printf check >/tmp/pv-private; test "$(cat /tmp/pv-private)" = check; exec "$@"',
                            "probe",
                            "/usr/local/bin/python",
                            "-B",
                            "-u",
                            "-c",
                            script,
                        ]
                    ]
                    record = native.RUNTIME.capture(
                        command,
                        native.REPO,
                        os.environ | product.env,
                        directory / "stdout",
                        timeout=120,
                    )
                    text = (directory / "stdout").read_text()
                    assert text.splitlines().count("REFERENCE_READY") == 1
                    assert not product.command(["ps", "-aq"]).strip()
                    resources = product.budget.read()
                    SAVE(directory / "resource-observation.json", resources)
                    assert not any(resources["memory_events"].values()), (
                        "resource pressure invalidates trial",
                        resources["memory_events"],
                    )
                    assert (product.group / "memory.swap.current").read_text().strip() == "0"
                    SAVE(
                        directory / "resources.json",
                        {"limits": resources, "membership": product.witness([])},
                    )
                    metrics = dict(
                        ready_ns=record["ready_duration_ns"], exit_ns=record["exit_duration_ns"]
                    )
                    if workload == "mmap-scan":
                        rows = [
                            line.removeprefix("READ_PHASES ")
                            for line in text.splitlines()
                            if line.startswith("READ_PHASES ")
                        ]
                        assert len(rows) == 1
                        values = json.loads(rows[0])
                        assert set(values) == {"first_ns", "repeat_ns"} and all(
                            type(v) is int and v > 0 for v in values.values()
                        )
                        metrics.update(values)
                    SAVE(directory / "metrics.json", metrics)
                    pair[backend] = metrics
                SAVE(e.root / f"{workload}-{phase}-{index:03}.json", pair)
                print(
                    workload,
                    phase,
                    index,
                    {b: {k: round(v / 1e6, 2) for k, v in m.items()} for b, m in pair.items()},
                    flush=True,
                )
                return pair

            pairs = native.RUNTIME.repeat(3, args.runs, trial)
            all_pairs[workload] = pairs
            summaries[workload] = {key: native.paired(pairs, key) for key in pairs[0]["docker"]}
            SAVE(e.root / "pairs.json", all_pairs)
            SAVE(e.root / "summary.json", summaries)
        assert all(native.sha(path) == digest for path, digest in frozen.items())
        e.save(
            status="complete", finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat()
        )
    except BaseException as error:
        e.save(status="failed", error=repr(error))
        raise
    finally:
        for product in products.values():
            product.close()


if __name__ == "__main__":
    main()
