#!/usr/bin/env python3
"""B-FS-ENG immutable-lower cache curves; B-FS-DIAG is a separate campaign.

Question: how does reopen reuse retain guest data pages as WSS crosses RAM?
Same physical owned fixture and static native/guest payload; read + full byte
comparison are timed. Five consecutive passes, three dependent resident rounds.
Shared host input pages may be charged outside the enclosing measurement group.
"""

import argparse
import hashlib
import json
import os
import platform
import random
import shutil
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

from curves import cgroup_state, save, sha, verify_artifacts
from curves_analysis import csv_file, describe, seed_for
from curves_diagnostics import aggregate, collect_phases, collect_profiles

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
MIB = 1 << 20
SIZES = (1, 16, 64, 256, 384, 448, 512, 768)
CONDITIONS = {
    "native": {},
    "off": {"PVISOR_VM_CACHE_EXPERIMENT": "off"},
    "on": {"PVISOR_VM_CACHE_EXPERIMENT": "root-data"},
}
HELPERS = ("curves.py", "curves_analysis.py", "curves_diagnostics.py")


def cells():
    return [
        dict(id=f"read-{size}m", size_mib=size, bytes=size * MIB, file=f"file-{size}m.bin")
        for size in SIZES
    ]


def make_fixture(root, sizes=SIZES):
    """Generate once, outside measured groups; bounded-memory full content hash."""
    files = root / "cache-input/files"
    files.mkdir(parents=True, exist_ok=False)
    pattern = random.Random(20261017).randbytes(MIB)
    (root / "cache-input/pattern.bin").write_bytes(pattern)
    for size in sizes:
        with (files / f"file-{size}m.bin").open("xb") as stream:
            for _ in range(size):
                stream.write(pattern)
            stream.flush()
            os.fsync(stream.fileno())
    inventory = {}
    for path in root.rglob("*"):
        if path.is_file():
            path.chmod(0o444)
            inventory[str(path.relative_to(root))] = dict(
                bytes=path.stat().st_size, sha256=sha(path)
            )
    for path in sorted((p for p in root.rglob("*") if p.is_dir()), reverse=True):
        path.chmod(0o555)
    root.chmod(0o555)
    return dict(
        schema=1,
        pattern="deterministic_random_1m_repeated",
        block_bytes=MIB,
        seed=20261017,
        compression="high entropy within 1MiB; repeated across offsets/files; no KSM requested",
        files=inventory,
    )


def verify_fixture(root, manifest):
    observed = {str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()}
    if observed != manifest["files"].keys():
        raise ValueError("immutable fixture file inventory changed")
    for name, receipt in manifest["files"].items():
        path = root / name
        if (
            path.is_symlink()
            or path.stat().st_size != receipt["bytes"]
            or sha(path) != receipt["sha256"]
        ):
            raise ValueError("immutable fixture changed: " + name)


def build(batch, receipt, reuse=None):
    source_hashes = receipt["source_hashes"]
    for name, digest in source_hashes.items():
        if sha(REPO / name) != digest:
            raise ValueError("current product source differs from supplied receipt: " + name)
    if reuse:
        prior = json.loads((reuse / "protocol.json").read_text())
        verify_artifacts(reuse, prior)
        if prior["source_hashes"] != source_hashes:
            raise ValueError("reused product source differs")
        for name in ("read_cache_driver.rs", "read_cache_guest.rs"):
            if sha(batch / "harness" / name) != sha(reuse / "harness" / name):
                raise ValueError("reused driver/payload source differs")
        if sha(batch / "bin/libkrunfw.so") != sha(reuse / "bin/libkrunfw.so"):
            raise ValueError("reused firmware differs")
        for name in ("subject", "payload"):
            shutil.copy2(reuse / "bin" / name, batch / "bin" / name)
        save(
            batch / "build-reuse.json",
            dict(batch=str(reuse), protocol_sha256=sha(reuse / "protocol.json")),
        )
        return
    for name in source_hashes:
        target = batch / "source" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / name, target)
    profiles = (
        '[profile.release]\nopt-level=3\nlto="thin"\nstrip="symbols"\ncodegen-units=8\npanic="abort"\n'
        '[profile.release.build-override]\nstrip="none"\n[profile.performance]\ninherits="release"\n'
        '[profile.performance.build-override]\nstrip="none"\n'
    )
    commands = []
    for kind in ("guest", "driver"):
        project = batch / kind
        (project / "src").mkdir(parents=True)
        shutil.copy2(batch / "harness" / f"read_cache_{kind}.rs", project / "src/main.rs")
        dependencies = 'serde_json="1"\n'
        if kind == "driver":
            dependencies += "pvisor-vm={path=" + json.dumps(str(REPO / "crates/pvisor-vm")) + "}\n"
            dependencies += (
                "pvisor-overlay-core={path="
                + json.dumps(str(REPO / "crates/pvisor-overlay-core"))
                + "}\n"
            )
        manifest = (
            '[workspace]\n[package]\nname="pvisor-read-cache-'
            + kind
            + '"\nversion="0.0.0"\nedition="2024"\n'
            "[dependencies]\n"
            + dependencies
            + profiles
            + "[patch.crates-io]\nfuser={path="
            + json.dumps(str(REPO / "vendor/fuser"))
            + "}\n"
        )
        (project / "Cargo.toml").write_text(manifest)
        shutil.copy2(REPO / "Cargo.lock", project / "Cargo.lock")
        command = [
            "cargo",
            "build",
            "--offline",
            "--profile",
            "performance",
            "--manifest-path",
            str(project / "Cargo.toml"),
            "--target-dir",
            str(REPO / "target"),
        ]
        if kind == "guest":
            command += ["--target", "x86_64-unknown-linux-musl"]
        commands.append(command)
    with (batch / "build.log").open("w") as log:
        for command in commands:
            log.write(json.dumps(command) + "\n")
            log.flush()
            subprocess.run(command, cwd=REPO, stdout=log, stderr=subprocess.STDOUT, check=True)
    shutil.copy2(
        REPO / "target/x86_64-unknown-linux-musl/performance/pvisor-read-cache-guest",
        batch / "bin/payload",
    )
    shutil.copy2(REPO / "target/performance/pvisor-read-cache-driver", batch / "bin/subject")
    for name, digest in source_hashes.items():
        if sha(REPO / name) != digest:
            raise ValueError("product source changed during build: " + name)


def prepare(args):
    batch = args.prepare.resolve()
    if ".data" not in batch.parts:
        raise ValueError("evidence must be under .data")
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    (batch / "bin").mkdir()
    for name in ("read_cache_curves.py", "read_cache_driver.rs", "read_cache_guest.rs", *HELPERS):
        shutil.copy2(HERE / name, batch / "harness" / name)
    shutil.copy2(args.firmware.resolve(), batch / "bin/libkrunfw.so")
    receipt = json.loads(args.source_receipt.read_text())
    shutil.copy2(args.source_receipt, batch / "product-build-receipt.json")
    build(batch, receipt, args.reuse_build.resolve() if args.reuse_build else None)
    if args.fixture_batch:
        prior = json.loads((args.fixture_batch / "protocol.json").read_text())
        fixture = Path(prior["fixture"])
        fixture_manifest = prior["fixture_manifest"]
        verify_fixture(fixture, fixture_manifest)
    else:
        fixture = batch / "fixture"
        fixture_manifest = make_fixture(fixture)
        verify_fixture(fixture, fixture_manifest)
    orders = []
    rng = random.Random(args.seed)
    for pair in range(args.samples):
        arms, plan = list(CONDITIONS), cells()
        rng.shuffle(arms)
        rng.shuffle(plan)
        save(batch / f"plan-{pair:03}.json", dict(schema=1, passes=5, cells=plan))
        orders.append(dict(pair=pair, conditions=arms, cell_order=[c["id"] for c in plan]))
    artifacts = {
        str(p.relative_to(batch)): sha(p)
        for folder in (batch / "bin", batch / "harness")
        for p in folder.iterdir()
        if p.is_file()
    }
    artifacts.update({p.name: sha(p) for p in batch.glob("plan-*.json")})
    artifacts["product-build-receipt.json"] = sha(batch / "product-build-receipt.json")
    protocol = dict(
        schema=1,
        benchmark_id="B-FS-DIAG" if args.diagnostic else "B-FS-ENG",
        role="diagnostic" if args.diagnostic else "engineering A/B",
        question=__doc__,
        cells=cells(),
        conditions=CONDITIONS,
        samples=args.samples,
        resident_rounds=3,
        passes=5,
        orders=orders,
        seed=args.seed,
        profiling=args.diagnostic,
        rootfs=str(args.rootfs.resolve()),
        rootfs_provenance="external prepared CLI input; path/receipt provenance, not rehashed by this runner",
        fixture=str(fixture),
        fixture_manifest=fixture_manifest,
        source_hashes=receipt["source_hashes"],
        product_receipt_sha256=sha(args.source_receipt),
        artifacts=artifacts,
        timing="open + streaming read + inline full memcmp + checked close; each pass and full five-pass cell; preparation/host postverify/exit separate",
        memory="2MiB payload buffers; guest512MiB; shared precharged host fixture pages may belong outside measured group; not whole-machine memory savings",
        cache="fresh guest; host input prepared and fully read before campaign; no cold-disk claim; repeated1MiB high entropy block",
        budget=dict(
            cpus=[0, 1], quota="200000 100000", memory_bytes=2 << 30, swap_bytes=0, guest_mib=512
        ),
        host=dict(
            platform=platform.platform(),
            affinity=sorted(os.sched_getaffinity(0)),
            filesystem=subprocess.check_output(
                ["findmnt", "-T", str(fixture), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS", "--json"],
                text=True,
            ),
        ),
        failures="retain all failures and valid slow samples; incomplete campaign cannot be complete evidence",
    )
    save(batch / "protocol.json", protocol)
    save(
        batch / "in.json",
        {
            "immutable-lower-read-cache": "$ "
            + sys.executable
            + " "
            + str(batch / "harness/read_cache_curves.py")
            + " --measure "
            + str(batch)
        },
    )
    print(batch, flush=True)


def validate_rows(protocol, rows, ready, done):
    expected = {(c["id"], r) for c in protocol["cells"] for r in range(1, 4)}
    observed = [(r.get("id"), r.get("round")) for r in rows]
    if (
        len(observed) != len(expected)
        or set(observed) != expected
        or ready
        != dict(
            schema=1,
            cells=len(protocol["cells"]),
            rounds=3,
            passes=5,
            buffer_bytes=MIB,
            verification="inline_memcmp_repeated_1m",
        )
        or done != dict(schema=1, rows=len(expected), passes=len(expected) * 5, verified=True)
    ):
        raise ValueError("Ready/DONE/cell-round inventory mismatch")
    by_id = {c["id"]: c for c in protocol["cells"]}
    for row in rows:
        passes = row.get("passes", [])
        if (
            row.get("schema") != 1
            or row.get("correctness") is not True
            or row.get("bytes") != by_id[row["id"]]["bytes"]
            or [p.get("pass") for p in passes] != [1, 2, 3, 4, 5]
        ):
            raise ValueError("invalid cell metadata/pass inventory")
        for p in passes:
            if (
                p.get("correctness") is not True
                or p.get("bytes") != row["bytes"]
                or any(
                    type(p.get(k)) is not int or p[k] < 0
                    for k in ("open_ns", "read_check_ns", "close_ns", "total_ns")
                )
                or p["total_ns"] <= 0
                or p["total_ns"] != p["open_ns"] + p["read_check_ns"] + p["close_ns"]
            ):
                raise ValueError("invalid pass bytes/clock/correctness")
        if (
            row.get("pass_ns_sum") != sum(p["total_ns"] for p in passes)
            or type(row.get("total_ns")) is not int
            or row["total_ns"] < row["pass_ns_sum"]
        ):
            raise ValueError("full-cell clock is inconsistent")


def one_trial(batch, protocol, pair, condition):
    folder = batch / "trials" / f"{pair:03}-{condition}"
    folder.mkdir(parents=True, exist_ok=False)
    stage = folder / "stage"
    for name in ("upper", "work"):
        (stage / name).mkdir(parents=True)
    plan = batch / f"plan-{pair:03}.json"
    fixture = Path(protocol["fixture"])
    native = condition == "native"
    command = (
        [str(batch / "bin/payload"), str(plan), str(fixture / "cache-input/files"), "3"]
        if native
        else [
            str(batch / "bin/subject"),
            protocol["rootfs"],
            str(fixture),
            str(batch / "bin/libkrunfw.so"),
            str(batch / "bin/payload"),
            str(plan),
            str(stage),
            "3",
        ]
    )
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PVISOR_", "READ_CACHE_"))}
    env.update(
        PVISOR_VM_FS_WRITEBACK="0",
        PVISOR_VM_FS_HOST_WRITEBACK="0",
        PVISOR_VM_GUEST_CACHE="1",
        PVISOR_FS_PROFILE="1" if protocol["profiling"] and not native else "0",
        READ_CACHE_PROFILE="1" if protocol["profiling"] and not native else "0",
        RUST_LOG="off",
    )
    env.update(protocol["conditions"][condition])
    result = dict(
        pair=pair,
        condition=condition,
        command=command,
        rows=[],
        resource_before=cgroup_state(),
        environment={k: v for k, v in env.items() if k.startswith(("PVISOR_", "READ_CACHE_"))},
    )
    started = time.monotonic_ns()
    ready = done = None
    process = None
    try:
        with (folder / "stdout.log").open("w") as out, (folder / "stderr.log").open("w") as err:
            process = subprocess.Popen(
                command,
                stdout=subprocess.PIPE,
                stderr=err,
                text=True,
                env=env,
                stdin=subprocess.DEVNULL,
            )
            timer = threading.Timer(600, process.kill)
            timer.start()
            try:
                for line in process.stdout:
                    out.write(line)
                    out.flush()
                    if line.startswith("READ_CACHE_READY "):
                        if ready is not None:
                            raise ValueError("duplicate Ready")
                        ready = json.loads(line.split(" ", 1)[1])
                        result["ready_ms"] = (time.monotonic_ns() - started) / 1e6
                    elif line.startswith("READ_CACHE_RESULT "):
                        result["rows"].append(json.loads(line.split(" ", 1)[1]))
                    elif line.startswith("READ_CACHE_DONE "):
                        if done is not None:
                            raise ValueError("duplicate DONE")
                        done = json.loads(line.split(" ", 1)[1])
                result["returncode"] = process.wait()
            finally:
                timer.cancel()
        result["job_ms"] = (time.monotonic_ns() - started) / 1e6
        if result["returncode"] != 0:
            raise ValueError("nonzero subject exit")
        validate_rows(protocol, result["rows"], ready, done)
        lines = (folder / "stderr.log").read_text().splitlines()
        if protocol["profiling"]:
            final = collect_profiles(lines, {} if native else None)
            expanded = [
                f"{id_}::p{p}"
                for id_ in next(o for o in protocol["orders"] if o["pair"] == pair)["cell_order"]
                for p in range(1, 6)
            ]
            phases = collect_phases(lines, final, expanded, 3)
            if not native and len(phases) != len(expanded) * 3 * 4:
                raise ValueError("missing per-pass phase inventory")
            save(
                folder / "diagnostics.json",
                dict(final_profiles=final, phases=phases, totals=aggregate(final)),
            )
        elif any("pvisor-fs-profile" in line for line in lines):
            raise ValueError("profile output in timing sample")
        result["valid"] = True
    except BaseException as error:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait()
        result.update(valid=False, error=repr(error))
    finally:
        result["resource"] = cgroup_state()
        save(folder / "result.json", result)
    if result["valid"]:
        shutil.rmtree(stage)
    return result


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_artifacts(batch, protocol)
    state = cgroup_state()
    if (
        state.get("cpu.max") != "200000 100000"
        or state.get("memory.max") != str(2 << 30)
        or state.get("memory.swap.max") != "0"
        or state["affinity"] != [0, 1]
    ):
        raise ValueError("enclosing CPU/memory budget was not installed")
    save(batch / "budget-before.json", state)
    records = []
    for order in protocol["orders"]:
        for condition in order["conditions"]:
            record = one_trial(batch, protocol, order["pair"], condition)
            records.append(record)
            save(batch / "records.partial.json", records)
            print(
                json.dumps(dict(pair=order["pair"], condition=condition, valid=record["valid"])),
                flush=True,
            )
    verify_artifacts(batch, protocol)
    verification = time.monotonic_ns()
    verify_fixture(Path(protocol["fixture"]), protocol["fixture_manifest"])
    save(
        batch / "host-postverification.json",
        dict(verified=True, full_sha256=True, ms=(time.monotonic_ns() - verification) / 1e6),
    )
    save(batch / "records.json", records)
    save(batch / "budget-after.json", cgroup_state())
    failed = sum(not r["valid"] for r in records)
    save(
        batch / "status.json",
        dict(
            valid=failed == 0, trials=len(records), failed=failed, excluded=0, fixture_verified=True
        ),
    )
    if failed:
        raise ValueError("failed trials retained; campaign incomplete")


def measure(batch):
    if (batch / "records.partial.json").exists() or (batch / "records.json").exists():
        raise ValueError("refusing batch reuse")
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_artifacts(batch, protocol)
    verify_fixture(Path(protocol["fixture"]), protocol["fixture_manifest"])
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--collect",
        "--unit",
        "pvisor-read-cache-" + uuid.uuid4().hex[:12],
        "-p",
        "CPUQuota=200%",
        "-p",
        "AllowedCPUs=0,1",
        "-p",
        "MemoryMax=2G",
        "-p",
        "MemorySwapMax=0",
        "-p",
        "LimitNOFILE=65536",
        "taskset",
        "-c",
        "0,1",
        sys.executable,
        str(batch / "harness/read_cache_curves.py"),
        "--campaign",
        str(batch),
    ]
    save(batch / "campaign-command.json", command)
    with (
        (batch / "campaign.stdout.log").open("w") as out,
        (batch / "campaign.stderr.log").open("w") as err,
    ):
        result = subprocess.run(command, stdout=out, stderr=err, timeout=14400, check=False)
    if result.returncode:
        raise ValueError("campaign failed; evidence retained")
    status = json.loads((batch / "status.json").read_text())
    expected_trials = sum(len(order["conditions"]) for order in protocol["orders"])
    postverify = json.loads((batch / "host-postverification.json").read_text())
    if (
        status.get("valid") is not True
        or status.get("failed") != 0
        or status.get("trials") != expected_trials
        or status.get("fixture_verified") is not True
        or postverify.get("verified") is not True
        or postverify.get("full_sha256") is not True
    ):
        raise ValueError("campaign completion receipt is incomplete")
    # semspec consumes exactly one JSON value and EOF, rather than the internal
    # campaign's progress lines (which remain in campaign.stdout.log).
    print(json.dumps(dict(schema=1, benchmark_id=protocol["benchmark_id"], **status)), flush=True)


def analyze(batch, plots=True):
    import inspect

    protocol_path, records_path = batch / "protocol.json", batch / "records.json"
    protocol = json.loads(protocol_path.read_text())
    records = json.loads(records_path.read_text())
    output = batch / "analysis"
    output.mkdir(exist_ok=True)
    expected = {
        (order["pair"], condition)
        for order in protocol["orders"]
        for condition in order["conditions"]
    }
    observed = [(record["pair"], record["condition"]) for record in records]
    if set(observed) != expected or len(observed) != len(expected):
        raise ValueError("environment inventory mismatch")
    if not json.loads((batch / "host-postverification.json").read_text()).get("verified"):
        raise ValueError("host postverify missing")
    diagnostic_mode = protocol["profiling"] is True
    conditions = list(protocol["conditions"])
    rounds = protocol["resident_rounds"]
    raw, points, diagnostic, count_points = [], [], [], []
    round_points, round_ratios, round_paired_summary = [], [], []
    count_round_points, count_round_summary = [], []
    for record in records:
        if record.get("valid") is not True:
            raise ValueError("failed environment, not complete evidence")
        validate_rows(
            protocol,
            record["rows"],
            dict(
                schema=1,
                cells=len(protocol["cells"]),
                rounds=3,
                passes=5,
                buffer_bytes=MIB,
                verification="inline_memcmp_repeated_1m",
            ),
            dict(
                schema=1,
                rows=len(protocol["cells"]) * 3,
                passes=len(protocol["cells"]) * 15,
                verified=True,
            ),
        )
        if diagnostic_mode:
            folder = batch / "trials" / f"{record['pair']:03}-{record['condition']}"
            for phase in json.loads((folder / "diagnostics.json").read_text())["phases"]:
                if phase["component"] == "virtio-fs-protocol":
                    id_, pass_ = phase["id"].split("::p")
                    diagnostic.append(
                        dict(
                            condition=record["condition"],
                            pair=record["pair"],
                            id=id_,
                            round=phase["round"],
                            pass_=int(pass_),
                            read_requests=phase["measurements"].get("READ", {}).get("calls", 0),
                        )
                    )
            continue
        for row in record["rows"]:
            for pass_ in row["passes"]:
                raw.append(
                    dict(
                        pair=record["pair"],
                        condition=record["condition"],
                        id=row["id"],
                        round=row["round"],
                        cell_total_ns=row["total_ns"],
                        **pass_,
                    )
                )
        for cell in protocol["cells"]:
            rows = [row for row in record["rows"] if row["id"] == cell["id"]]
            for group, indices in (("first", (1,)), ("repeat", (2, 3, 4, 5))):
                selected = [p for row in rows for p in row["passes"] if p["pass"] in indices]
                byte_sum, time_sum = (
                    sum(p["bytes"] for p in selected),
                    sum(p["total_ns"] for p in selected),
                )
                points.append(
                    dict(
                        pair=record["pair"],
                        condition=record["condition"],
                        id=cell["id"],
                        size_mib=cell["size_mib"],
                        reuse=group,
                        rounds=rounds,
                        pass_count=len(selected),
                        bytes_sum=byte_sum,
                        total_ns_sum=time_sum,
                        throughput_mib_s=byte_sum * 1e9 / MIB / time_sum,
                    )
                )
            for row in rows:
                selected = [p for p in row["passes"] if p["pass"] in (2, 3, 4, 5)]
                byte_sum = sum(p["bytes"] for p in selected)
                time_sum = sum(p["total_ns"] for p in selected)
                round_points.append(
                    dict(
                        pair=record["pair"],
                        condition=record["condition"],
                        id=cell["id"],
                        size_mib=cell["size_mib"],
                        round=row["round"],
                        pass_count=4,
                        bytes_sum=byte_sum,
                        total_ns_sum=time_sum,
                        throughput_mib_s=byte_sum * 1e9 / MIB / time_sum,
                    )
                )

    summary, ratios, paired_summary = [], [], []
    count_summary = []
    for cell in protocol["cells"]:
        for group in ("first", "repeat"):
            for condition in conditions:
                if diagnostic_mode:
                    if condition == "native":
                        continue  # Native has no FUSE connection; unavailable is not zero.
                    for pair in sorted(
                        {record["pair"] for record in records if record["condition"] == condition}
                    ):
                        selected = [
                            row
                            for row in diagnostic
                            if row["id"] == cell["id"]
                            and row["condition"] == condition
                            and row["pair"] == pair
                            and ((row["pass_"] == 1) == (group == "first"))
                        ]
                        expected_passes = rounds * (1 if group == "first" else 4)
                        if len(selected) != expected_passes:
                            raise ValueError("incomplete diagnostic per-pass request inventory")
                        count_points.append(
                            dict(
                                id=cell["id"],
                                size_mib=cell["size_mib"],
                                reuse=group,
                                condition=condition,
                                pair=pair,
                                read_requests_per_pass=sum(row["read_requests"] for row in selected)
                                / expected_passes,
                            )
                        )
                    values = [
                        point["read_requests_per_pass"]
                        for point in count_points
                        if point["id"] == cell["id"]
                        and point["reuse"] == group
                        and point["condition"] == condition
                    ]
                    count_summary.append(
                        dict(
                            id=cell["id"],
                            size_mib=cell["size_mib"],
                            reuse=group,
                            condition=condition,
                            **describe(values),
                        )
                    )
                    continue
                values = [
                    point["throughput_mib_s"]
                    for point in points
                    if point["id"] == cell["id"]
                    and point["reuse"] == group
                    and point["condition"] == condition
                ]
                summary.append(
                    dict(
                        id=cell["id"],
                        size_mib=cell["size_mib"],
                        reuse=group,
                        condition=condition,
                        **describe(
                            values,
                            bootstrap=True,
                            seed=seed_for(protocol.get("seed"), cell["id"], group, condition),
                        ),
                    )
                )
            if diagnostic_mode:
                continue
            off = {
                p["pair"]: p["throughput_mib_s"]
                for p in points
                if p["id"] == cell["id"] and p["reuse"] == group and p["condition"] == "off"
            }
            on = {
                p["pair"]: p["throughput_mib_s"]
                for p in points
                if p["id"] == cell["id"] and p["reuse"] == group and p["condition"] == "on"
            }
            if set(off) != set(on):
                raise ValueError("incomplete independent off/on pairs")
            pair_ids = sorted(off)
            values = [on[pair] / off[pair] for pair in pair_ids]
            for pair, value in zip(pair_ids, values):
                ratios.append(
                    dict(
                        id=cell["id"],
                        size_mib=cell["size_mib"],
                        reuse=group,
                        pair=pair,
                        condition="on",
                        baseline="off",
                        on_over_off=value,
                        on_mib_s=on[pair],
                        off_mib_s=off[pair],
                    )
                )
            paired_summary.append(
                dict(
                    id=cell["id"],
                    size_mib=cell["size_mib"],
                    reuse=group,
                    condition="on",
                    baseline="off",
                    pairs=pair_ids,
                    statistic="median of independent-pair on/off throughput ratios",
                    **describe(
                        values,
                        bootstrap=True,
                        seed=seed_for(protocol.get("seed"), cell["id"], group, "paired"),
                    ),
                )
            )

        for round_ in range(1, rounds + 1):
            if diagnostic_mode:
                for condition in conditions:
                    if condition == "native":
                        continue
                    pairs = sorted(
                        {record["pair"] for record in records if record["condition"] == condition}
                    )
                    values = []
                    for pair in pairs:
                        selected = [
                            row
                            for row in diagnostic
                            if row["id"] == cell["id"]
                            and row["condition"] == condition
                            and row["pair"] == pair
                            and row["round"] == round_
                            and row["pass_"] in (2, 3, 4, 5)
                        ]
                        if len(selected) != 4:
                            raise ValueError("incomplete per-round reopen diagnostic inventory")
                        value = sum(row["read_requests"] for row in selected) / 4
                        values.append(value)
                        count_round_points.append(
                            dict(
                                id=cell["id"],
                                size_mib=cell["size_mib"],
                                condition=condition,
                                round=round_,
                                pair=pair,
                                read_requests_per_pass=value,
                            )
                        )
                    count_round_summary.append(
                        dict(
                            id=cell["id"],
                            size_mib=cell["size_mib"],
                            condition=condition,
                            round=round_,
                            **describe(values),
                        )
                    )
                continue
            off = {
                point["pair"]: point["throughput_mib_s"]
                for point in round_points
                if point["id"] == cell["id"]
                and point["round"] == round_
                and point["condition"] == "off"
            }
            on = {
                point["pair"]: point["throughput_mib_s"]
                for point in round_points
                if point["id"] == cell["id"]
                and point["round"] == round_
                and point["condition"] == "on"
            }
            if set(off) != set(on):
                raise ValueError("incomplete per-round independent off/on pairs")
            pairs = sorted(off)
            values = [on[pair] / off[pair] for pair in pairs]
            for pair, value in zip(pairs, values):
                round_ratios.append(
                    dict(
                        id=cell["id"],
                        size_mib=cell["size_mib"],
                        round=round_,
                        pair=pair,
                        condition="on",
                        baseline="off",
                        on_over_off=value,
                        on_mib_s=on[pair],
                        off_mib_s=off[pair],
                    )
                )
            round_paired_summary.append(
                dict(
                    id=cell["id"],
                    size_mib=cell["size_mib"],
                    round=round_,
                    condition="on",
                    baseline="off",
                    pairs=pairs,
                    statistic="median independent-pair ratio within one resident round; reopen passes 2-5",
                    **describe(
                        values,
                        bootstrap=True,
                        seed=seed_for(protocol.get("seed"), cell["id"], round_, "history"),
                    ),
                )
            )

    tables = (
        {
            "read_requests.csv": diagnostic,
            "read_request_environment_points.csv": count_points,
            "read_request_summary.csv": count_summary,
            "read_request_rounds.csv": count_round_points,
            "read_request_round_summary.csv": count_round_summary,
        }
        if diagnostic_mode
        else {
            "raw_passes.csv": raw,
            "environment_points.csv": points,
            "curve_summary.csv": summary,
            "paired_ratios.csv": ratios,
            "paired_ratio_summary.csv": paired_summary,
            "cache_rounds.csv": round_points,
            "cache_round_ratios.csv": round_ratios,
            "cache_round_ratio_summary.csv": round_paired_summary,
        }
    )
    for name, rows in tables.items():
        csv_file(output / name, rows)
    report = dict(
        schema=2,
        benchmark_id=protocol["benchmark_id"],
        performance_samples=not diagnostic_mode,
        analysis_source=dict(
            version="read-cache-analysis-v4",
            script=str(Path(__file__).resolve()),
            script_sha256=sha(Path(__file__)),
            analyze_sha256=hashlib.sha256(inspect.getsource(analyze).encode()).hexdigest(),
            helper_sha256={name: sha(HERE / name) for name in HELPERS},
        ),
        input_sha256=dict(
            protocol=sha(protocol_path),
            records=sha(records_path),
            host_postverification=sha(batch / "host-postverification.json"),
        ),
        independent_unit="fresh paired environment; resident rounds and reopen passes are dependent",
        bootstrap=5000,
        confidence="95% percentile bootstrap; independent environments or pairs; unadjusted across cells",
        evidence="N5 exploratory" if protocol["samples"] < 30 else "independent confirmation batch",
        reuse_labels=dict(
            first="per-cell first pass across three resident rounds; not cold first-ever access",
            repeat="per-cell reopen passes 2-5 across three resident rounds",
        ),
        aggregation="sum bytes / sum pass wall times within an environment, then median across environments",
        ratio="median of independent-pair on/off throughput ratios; native shown only as an absolute reference",
        fixture_contract="one shared physical fixture declared Immutable; rootfs remains Mutable",
        timing=protocol["timing"],
        memory=protocol["memory"],
        valid_environments=len(records),
        failed_environments=0,
        summary=summary,
        paired_ratio_summary=paired_summary,
        round_paired_ratio_summary=round_paired_summary,
        history_unit="same independent pairs within each resident round; never pool rounds into N15",
        request_summary=count_summary,
        request_round_summary=count_round_summary,
        output_files=list(tables),
    )
    if plots:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        plt.rcParams.update(
            {
                "font.size": 10,
                "axes.spines.top": False,
                "axes.spines.right": False,
                "pdf.fonttype": 42,
                "svg.fonttype": "none",
            }
        )
        fig, axes = plt.subplots(
            1,
            2 if diagnostic_mode else 3,
            figsize=(10 if diagnostic_mode else 16.5, 4.4),
            layout="constrained",
        )
        colors = {"native": "#666666", "off": "#D55E00", "on": "#0072B2"}

        def series(ax, rows, color, label, individual, metric):
            rows = sorted(rows, key=lambda row: row["size_mib"])
            if not rows:
                return
            ax.plot(
                [row["size_mib"] for row in rows],
                [row["median"] for row in rows],
                marker="o",
                color=color,
                label=label,
                linewidth=1.5,
            )
            with_ci = [row for row in rows if row.get("ci_low") is not None]
            if with_ci:
                ax.fill_between(
                    [row["size_mib"] for row in with_ci],
                    [row["ci_low"] for row in with_ci],
                    [row["ci_high"] for row in with_ci],
                    color=color,
                    alpha=0.14,
                )
                ax.errorbar(
                    [row["size_mib"] for row in with_ci],
                    [row["median"] for row in with_ci],
                    yerr=[
                        [row["median"] - row["ci_low"] for row in with_ci],
                        [row["ci_high"] - row["median"] for row in with_ci],
                    ],
                    fmt="none",
                    ecolor=color,
                    alpha=0.65,
                    capsize=3,
                    linewidth=0.8,
                )
            ax.scatter(
                [point["size_mib"] for point in individual],
                [point[metric] for point in individual],
                color=color,
                marker="x",
                s=22,
                alpha=0.6,
                zorder=4,
            )

        for ax, group in zip(axes[:2], ("first", "repeat")):
            for condition in conditions:
                rows = [
                    row
                    for row in (count_summary if diagnostic_mode else summary)
                    if row["reuse"] == group and row["condition"] == condition
                ]
                individuals = [
                    row
                    for row in (count_points if diagnostic_mode else points)
                    if row["reuse"] == group and row["condition"] == condition
                ]
                series(
                    ax,
                    rows,
                    colors[condition],
                    condition,
                    individuals,
                    "read_requests_per_pass" if diagnostic_mode else "throughput_mib_s",
                )
            ax.set(
                ylabel="READ requests / pass"
                if diagnostic_mode
                else "Read + check throughput (MiB/s)",
                title="Per-cell first pass across 3 rounds\n(not cold first-ever access)"
                if group == "first"
                else "Reopen passes 2-5 across 3 rounds",
            )
        if not diagnostic_mode:
            for group, color in (("first", "#009E73"), ("repeat", "#CC79A7")):
                series(
                    axes[2],
                    [row for row in paired_summary if row["reuse"] == group],
                    color,
                    group,
                    [row for row in ratios if row["reuse"] == group],
                    "on_over_off",
                )
            axes[2].axhline(1, color="#666666", linestyle="--", linewidth=0.8)
            axes[2].set(
                ylabel="Paired throughput: on / off",
                title="Independent paired ratios\n(native excluded)",
            )
        guest_mib = protocol.get("budget", {}).get("guest_mib", 512)
        ticks = sorted({cell["size_mib"] for cell in protocol["cells"]} | {guest_mib})
        for ax in axes:
            ax.axvline(
                guest_mib,
                color="#333333",
                linestyle=":",
                linewidth=1,
                label=f"guest RAM {guest_mib} MiB",
            )
            ax.set(xscale="log", xlabel="Working set (MiB)", ylim=(0, None))
            ax.set_xticks(ticks, [str(x) for x in ticks])
            plt.setp(ax.get_xticklabels(), rotation=65, ha="right", fontsize=8)
            ax.grid(alpha=0.25)
            ax.legend(fontsize=8, frameon=False)
        fig.suptitle(
            "Independent diagnostic READ counts; instrumented durations excluded"
            if diagnostic_mode
            else f"N={protocol['samples']} independent environments/arm; all points + 95% bootstrap intervals; "
            + ("exploratory" if protocol["samples"] < 30 else "independent confirmation"),
            fontsize=10,
        )
        for extension in ("svg", "pdf"):
            name = ("read-requests." if diagnostic_mode else "read-cache-throughput.") + extension
            fig.savefig(output / name, bbox_inches="tight")
            report["output_files"].append(name)
        plt.close(fig)
        if diagnostic_mode:
            vm_conditions = [condition for condition in conditions if condition != "native"]
            fig, axes = plt.subplots(
                1, len(vm_conditions), figsize=(12, 4), layout="constrained", squeeze=False
            )
            cells_by_size = sorted(protocol["cells"], key=lambda cell: cell["size_mib"])
            maximum = max(point["read_requests_per_pass"] for point in count_round_points)
            norm = matplotlib.colors.SymLogNorm(linthresh=1, vmin=0, vmax=max(maximum, 1))
            for ax, condition in zip(axes[0], vm_conditions):
                matrix = [
                    [
                        next(
                            row["median"]
                            for row in count_round_summary
                            if row["condition"] == condition
                            and row["round"] == round_
                            and row["id"] == cell["id"]
                        )
                        for cell in cells_by_size
                    ]
                    for round_ in range(1, rounds + 1)
                ]
                image = ax.imshow(matrix, aspect="auto", cmap="YlGnBu", norm=norm)
                for i, values in enumerate(matrix):
                    for j, value in enumerate(values):
                        ax.text(
                            j,
                            i,
                            f"{value:g}",
                            ha="center",
                            va="center",
                            fontsize=8,
                            color="white" if norm(value) > 0.65 else "black",
                        )
                ax.set_xticks(
                    range(len(cells_by_size)), [str(cell["size_mib"]) for cell in cells_by_size]
                )
                ax.set_yticks(range(rounds), [f"R{round_}" for round_ in range(1, rounds + 1)])
                ax.set(xlabel="Working set (MiB)", ylabel="Resident round", title=condition)
            fig.colorbar(
                image,
                ax=list(axes[0]),
                label="READ requests / reopen pass (symlog color)",
                shrink=0.8,
            )
            fig.suptitle(
                f"Diagnostic N={protocol['samples']} environment/arm: reopen passes 2-5; cache history, not a fixed RAM threshold"
            )
            name = "read-request-round-history"
        else:
            fig, axes = plt.subplots(
                1, rounds, figsize=(16.5, 4.1), layout="constrained", sharey=True, squeeze=False
            )
            for ax, round_ in zip(axes[0], range(1, rounds + 1)):
                series(
                    ax,
                    [row for row in round_paired_summary if row["round"] == round_],
                    "#0072B2",
                    "on / off",
                    [row for row in round_ratios if row["round"] == round_],
                    "on_over_off",
                )
                ax.axhline(1, color="#666666", linestyle="--", linewidth=0.8)
                ax.axvline(
                    guest_mib,
                    color="#333333",
                    linestyle=":",
                    linewidth=1,
                    label=f"guest RAM {guest_mib} MiB (reference)",
                )
                ax.set(xscale="log", xlabel="Working set (MiB)", title=f"Resident round {round_}")
                ax.set_xticks(ticks, [str(x) for x in ticks])
                plt.setp(ax.get_xticklabels(), rotation=65, ha="right", fontsize=8)
                ax.grid(alpha=0.25)
                ax.legend(fontsize=8, frameon=False)
            axes[0][0].set(ylabel="Paired on / off throughput", ylim=(0, None))
            fig.suptitle(
                f"Reopen passes 2-5: N={protocol['samples']} independent pairs per round; 95% bootstrap; rounds are dependent"
            )
            name = "read-cache-round-history"
        for extension in ("svg", "pdf"):
            filename = name + "." + extension
            fig.savefig(output / filename, bbox_inches="tight")
            report["output_files"].append(filename)
        plt.close(fig)
    save(output / "summary.json", report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    for name in ("prepare", "measure", "campaign", "analyze"):
        modes.add_argument("--" + name, type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--firmware", type=Path)
    parser.add_argument(
        "--source-receipt",
        type=Path,
        default=HERE / ".data/curves-cli-artifacts-20261017/build-receipt.json",
    )
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--seed", type=int, default=20261017)
    parser.add_argument("--diagnostic", action="store_true")
    parser.add_argument("--reuse-build", type=Path)
    parser.add_argument("--fixture-batch", type=Path)
    parser.add_argument("--no-plots", action="store_true")
    args = parser.parse_args()
    if args.prepare:
        if not args.rootfs or not args.firmware or args.samples < 1:
            parser.error("prepare needs rootfs, firmware, positive samples")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    elif args.campaign:
        campaign(args.campaign.resolve())
    else:
        analyze(args.analyze.resolve(), not args.no_plots)


if __name__ == "__main__":
    main()
