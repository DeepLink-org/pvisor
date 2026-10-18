#!/usr/bin/env python3
"""Benchmark: B-FS-TOOLS (benchmark/README.md#b-fs-tools), user-facing.

Question: how much time do complete repository/build/publish tasks take in each
user execution mode, and which I/O shapes and resource conditions explain it?
Design: identical frozen static payload, fixture and toolroot; N3 fresh case
environments/configuration, three dependent rounds, randomized mode order.
Ready excludes isolation-harness preparation and exit; work plus checked final
sync is task completion. No interference gate or deletion of valid slow values.
This is a new user cohort; engineering writeback A/B observations are not inputs.
"""

import argparse
import json
import os
import platform
import random
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
import user_workloads_backends as BACKENDS  # noqa: E402 - import after module-loading setup
import user_workloads_vm as VM  # noqa: E402 - import after module-loading setup
import workloads_v2_run as SHARED  # noqa: E402 - import after module-loading setup

LABELS = {
    "native": "Native",
    "host": "pVisor host",
    "staged": "pVisor staged",
    "vm": "pVisor VM",
    "vm-writeback": "pVisor VM + writeback",
    "docker": "Docker",
    "fc-system": "Firecracker (stock kernel)",
    "qemu": "QEMU q35",
    "qemu-microvm": "QEMU microvm",
}
HARNESS = (
    "user_workloads.py",
    "user_workloads_backends.py",
    "user_workloads_vm.py",
    "user_workloads_analysis.py",
    "workloads_v2.py",
    "workloads_v2_run.py",
    "curves_diagnostics.py",
    "user_workloads_docker.py",
    "user_workloads_docker_launch.rs",
    "user_workloads_reference_vm.py",
)
save, sha, require = SHARED.save, SHARED.sha, SHARED.require


def resource_state(protocol):
    """Read the enclosing budget, including this cohort's private daemon."""
    state = SHARED.cgroup_state()
    docker = protocol.get("reference_configuration", {}).get("docker", {})
    if not docker:
        return state
    root = Path(docker["budget_cgroup"]).resolve(strict=True)
    require(
        root.is_relative_to(Path("/sys/fs/cgroup")) and Path(state["cgroup"]).is_relative_to(root),
        "campaign escaped reference parent budget",
    )
    result = dict(
        cgroup=str(root),
        affinity=state["affinity"],
        observer_cgroup=state["cgroup"],
        scope="parent slice including private Docker daemon",
    )
    for name in (
        "cpu.max",
        "cpu.stat",
        "memory.max",
        "memory.swap.max",
        "memory.current",
        "memory.peak",
        "memory.events",
        "memory.stat",
        "io.stat",
    ):
        if (root / name).exists():
            result[name] = (root / name).read_text().strip()
    return result


def prepare(args):
    batch = args.prepare.resolve()
    require(".data" in batch.parts, "raw evidence belongs under .data")
    batch.mkdir(parents=True, exist_ok=False)
    (batch / "harness").mkdir()
    (batch / "bin").mkdir()
    source = args.payload_batch.resolve()
    for name in HARNESS:
        shutil.copy2(HERE / name, batch / "harness" / name)
    for name in ("payload", "libkrunfw.so"):
        shutil.copy2(source / "bin" / name, batch / "bin" / name)
    for name in ("workloads_v2_guest.rs", "workloads_v2_scenarios.rs"):
        shutil.copy2(source / "harness" / name, batch / "harness" / name)
    shutil.copy2(source / "firmware-source/SOURCE.json", batch / "firmware-source.json")
    shutil.copytree(source / "fixture-layer", batch / "fixture-layer", symlinks=True)
    shutil.copy2(source / "fixture-manifest.json", batch / "fixture-manifest.json")
    shutil.copy2(args.artifacts / "build-receipt.json", batch / "cli-build-receipt.json")
    for locale in ("zh", "en"):
        shutil.copy2(
            REPO / "docs/src" / locale / "benchmarks/filesystem.md",
            batch / ("historical-user-page-" + locale + ".md"),
        )
    prepared = BACKENDS.prepare_rootfs(
        batch, args.rootfs.resolve(), source, args.artifacts.resolve()
    )
    source_protocol = json.loads((source / "protocol.json").read_text())
    catalog = source_protocol["catalog"]
    for case in catalog:
        case.pop("comparison_applicability", None)
        case["input_location"] = (
            "same frozen fixture read through the selected execution mode workspace/input"
        )
    cases = [c["id"] for c in catalog]
    if args.cases:
        selected = args.cases.split(",")
        require(
            len(set(selected)) == len(selected) and set(selected) <= set(cases), "invalid cases"
        )
        cases = selected
    modes = args.backends.split(",") if args.backends else list(LABELS)
    require(len(set(modes)) == len(modes) and set(modes) <= set(LABELS), "invalid backends")
    references = {}
    if "docker" in modes:
        require(args.docker_setup is not None, "private Docker budget setup required")
        import user_workloads_docker as DOCKER

        references["docker"] = DOCKER.prepare_assets(batch, args.docker_setup.resolve())
    if set(modes) & {"fc-system", "qemu", "qemu-microvm"}:
        require(args.stock_kernel_receipt is not None, "stock kernel receipt required")
        import user_workloads_reference_vm as REFERENCE_VM

        references.update(REFERENCE_VM.prepare_assets(batch, args.stock_kernel_receipt.resolve()))
    rng = random.Random(args.seed)
    orders = []
    for pair in range(1, args.samples + 1):
        sequence = list(cases)
        rng.shuffle(sequence)
        for case in sequence:
            order = list(modes)
            rng.shuffle(order)
            orders.append(dict(case_id=case, pair=pair, backends=order))
    artifacts = {
        str(p.relative_to(batch)): sha(p)
        for p in list((batch / "harness").glob("*"))
        + list((batch / "bin").glob("*"))
        + [
            batch / name
            for name in ("toolroot-receipt.json", "cli-build-receipt.json", "firmware-source.json")
        ]
        if p.is_file()
    }
    for directory in ("reference-assets", "docker-assets"):
        if (batch / directory).exists():
            artifacts.update(
                {
                    str(p.relative_to(batch)): sha(p)
                    for p in (batch / directory).rglob("*")
                    if p.is_file()
                }
            )
    protocol = dict(
        schema=1,
        benchmark_id="B-FS-TOOLS",
        role="user-facing",
        samples=args.samples,
        sample_index_base=1,
        rounds=3,
        ram_mib=512,
        seed=args.seed,
        cases=cases,
        catalog=catalog,
        backends=[dict(id=m, label=LABELS[m]) for m in modes],
        orders=orders,
        vm_configuration={m: VM.vm_configuration(m) for m in modes if m.startswith("vm")},
        reference_configuration=references,
        toolroot=str(prepared),
        prepared_toolroot_receipt="toolroot-receipt.json",
        prepared_tools={
            name: sha(prepared / name)
            for name in (
                "usr/bin/cc",
                "usr/bin/git",
                "usr/bin/python3",
                "usr/libexec/gcc/x86_64-redhat-linux/16/cc1",
            )
        },
        fixture=str(batch / "fixture-layer/fs-v2-fixture"),
        artifacts=artifacts,
        fixture_manifest_sha256=sha(batch / "fixture-manifest.json"),
        artifact_origin=str(args.artifacts.resolve()),
        payload_origin=str(source),
        rootfs_origin=str(args.rootfs.resolve()),
        trial_timeout_seconds=args.timeout,
        budget=dict(
            cpus=[0, 1],
            quota="200000 100000",
            memory_bytes=2 << 30,
            swap_bytes=0,
            guest_mib=512,
            guest_vcpus=2,
            cpu_placement="inherited affinity; host user cgroup has no cpuset delegation",
        ),
        resource_scale="128/640 MiB WSS and 1 GiB logical writes at fixed 512 MiB reference; native/host have parent 2 GiB, not 512 MiB guest limit",
        timing="Ready from common pre-exec launch marker; isolation harness preparation and exit excluded; completed=work+final_sync, setup/validation separate",
        cache="fresh case environment; three dependent rounds reuse state and verification-warmed pages; shared prepared host inputs; no cold-disk claim",
        quiet_gate=False,
        interference_gate=False,
        inventory_gate=False,
        skip_remaining_on_case_failure=True,
        failures="all failures retained without replacement; after first backend/case failure remaining planned observations for that cell are explicitly skipped; incomplete cells get no median",
        limitations=[
            "N3 exploration, no reliable tails or population-wide ranking",
            "reference runtime storage stacks differ; not pure VMM overhead",
            "resource WSS changes retained bytes and final sync volume",
            "four resource anchors are not a dense capacity curve or established pressure knee",
        ],
        host=dict(
            platform=platform.platform(),
            uname=list(platform.uname()),
            filesystem=subprocess.check_output(
                ["findmnt", "-T", str(batch), "-o", "TARGET,SOURCE,FSTYPE,OPTIONS", "--json"],
                text=True,
            ),
        ),
    )
    save(batch / "protocol.json", protocol)
    save(
        batch / "in.json",
        {
            "filesystem-user-layered": "$ "
            + sys.executable
            + " "
            + str(batch / "harness/user_workloads.py")
            + " --measure "
            + str(batch)
        },
    )
    print(batch, flush=True)


def one_trial(batch, protocol, case, pair, backend, *, preflight=False):
    folder = batch / ("preflight" if preflight else "trials") / f"{case}-{pair:03}-{backend}"
    folder.mkdir(parents=True, exist_ok=False)
    record = dict(
        case_id=case,
        pair=pair,
        backend=backend,
        rows=[],
        valid=False,
        host_verified=False,
        folder=str(folder.relative_to(batch)),
    )
    process = None
    started = time.monotonic_ns()
    launch = ready = done = None
    before = resource_state(protocol)
    try:
        argv, env = BACKENDS.build_command(batch, protocol, folder, case, backend)
        record["command"] = argv
        with (folder / "stdout.log").open("w") as out, (folder / "stderr.log").open("w") as err:
            process = subprocess.Popen(
                argv,
                stdout=subprocess.PIPE,
                stderr=err,
                stdin=subprocess.DEVNULL,
                text=True,
                env=env,
                start_new_session=True,
            )
            timed_out = threading.Event()

            def timeout():
                timed_out.set()
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

            timer = threading.Timer(protocol["trial_timeout_seconds"], timeout)
            timer.start()
            try:
                for line in process.stdout:
                    now = time.monotonic_ns()
                    out.write(line)
                    out.flush()
                    if line.startswith("USER_WORKLOADS_LAUNCH "):
                        require(launch is None, "duplicate executor launch")
                        launch = json.loads(line.split(" ", 1)[1])["monotonic_ns"]
                    elif line.startswith("WORKLOADS_V2_READY "):
                        require(ready is None, "duplicate Ready")
                        ready = now
                        record["ready_record"] = json.loads(line.split(" ", 1)[1])
                    elif line.startswith("WORKLOADS_V2_RESULT "):
                        record["rows"].append(json.loads(line.split(" ", 1)[1]))
                    elif line.startswith("WORKLOADS_V2_DONE "):
                        require(done is None, "duplicate DONE")
                        done = now
                rc = process.wait()
            finally:
                timer.cancel()
        ended = time.monotonic_ns()
        after = resource_state(protocol)
        record.update(
            returncode=rc,
            timed_out=timed_out.is_set(),
            ready_ns=ready - launch if ready is not None and launch is not None else None,
            wrapper_prepare_ns=launch - started if launch is not None else None,
            job_ns=ended - started,
            exit_after_done_ns=ended - done if done else None,
            resource=dict(
                before=before,
                after=after,
                cpu_usage_usec_delta=SHARED.usage(after) - SHARED.usage(before),
                scope="whole trial including namespace setup, workload preparation, verification and exit",
            ),
        )
        require(
            rc == 0
            and not timed_out.is_set()
            and launch is not None
            and ready is not None
            and done is not None,
            "executor/Ready/DONE failed",
        )
        require(started <= launch <= ready <= done <= ended, "external clock ordering failed")
        rows = record["rows"]
        require(
            len(rows) == 3
            and {(r["id"], r["round"]) for r in rows} == {(case, n) for n in (1, 2, 3)},
            "exact three rounds missing",
        )
        for row in rows:
            SHARED.validate_row(row, case)
        require(
            "pvisor-fs-profile " not in (folder / "stderr.log").read_text(),
            "diagnostics present in timing",
        )
        if backend in ("staged", "vm", "vm-writeback"):
            require(
                {p.name for p in (folder / "work").iterdir()} == {"input", "output"}
                and not any((folder / "work/output").iterdir()),
                "staged execution modified the original workspace",
            )
            record["original_workspace_unchanged"] = True
        work, overlay = BACKENDS.finalize_output(batch, protocol, folder, case, backend)
        view = folder / "host-view"
        if overlay:
            record["physical_overlay_markers"] = SHARED.materialize_upper_view(work, view, case)
        else:
            view = work
        verified = subprocess.run(
            [
                str(batch / "bin/payload"),
                "--verify",
                case,
                str(view),
                protocol["fixture"],
                "3",
                "512",
            ],
            text=True,
            capture_output=True,
            timeout=180,
        )
        (folder / "host-verification.log").write_text(verified.stdout + verified.stderr)
        receipt = [
            json.loads(s.split(" ", 1)[1])
            for s in verified.stdout.splitlines()
            if s.startswith("WORKLOADS_V2_VERIFIED ")
        ]
        require(
            verified.returncode == 0 and len(receipt) == 1 and receipt[0].get("verified") is True,
            "independent final host verification failed",
        )
        record.update(host_verified=True, host_verification=receipt[0], valid=True)
    except Exception as error:
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        record["error"] = repr(error)
    finally:
        try:
            BACKENDS.cleanup_trial(batch, protocol, folder, case, backend)
        except Exception as error:
            record["valid"] = False
            record["cleanup_error"] = repr(error)
    save(folder / "result.json", record)
    if record["valid"]:
        # Retain logs/receipts; discard only this trial's generated multi-GiB data.
        for name in (
            "host-view",
            "work",
            "stage",
            "runs",
            "namespace-root",
            "exported-work",
            "reference-output",
            "docker-output",
        ):
            path = folder / name
            if path.exists():
                shutil.rmtree(path)
        for name in ("work.ext4", "root.ext4"):
            (folder / name).unlink(missing_ok=True)
    return record


def verify_inputs(batch, protocol):
    SHARED.verify_artifacts(batch, protocol)
    SHARED.verify_fixture(batch, protocol)
    receipt = json.loads((batch / "toolroot-receipt.json").read_text())
    for artifact in receipt["installed"] + receipt["runtime_dependencies"]:
        require(
            sha(Path(protocol["toolroot"]) / artifact["path"]) == artifact["sha256"],
            "executed artifact changed: " + artifact["path"],
        )
    for relative, digest in protocol["prepared_tools"].items():
        require(
            sha(Path(protocol["toolroot"]) / relative) == digest,
            "prepared tool changed: " + relative,
        )
    # Fixture files are shared read-only by private namespaces; retain sentinels
    # at campaign boundaries without scanning the product filesystem per trial.
    expected = json.loads((batch / "fixture-manifest.json").read_text())
    for relative, digest in expected.items():
        require(
            sha(Path(protocol["toolroot"]) / relative) == digest,
            "shared toolroot fixture changed: " + relative,
        )


def campaign(batch):
    protocol = json.loads((batch / "protocol.json").read_text())
    verify_inputs(batch, protocol)
    state = resource_state(protocol)
    require(
        state["cpu.max"] == "200000 100000"
        and state["memory.max"] == str(2 << 30)
        and state["memory.swap.max"] == "0"
        and state["affinity"] == [0, 1],
        "resource budget missing",
    )
    save(batch / "budget-before.json", state)
    records = []
    failed_cells, skipped = set(), []
    for order in protocol["orders"]:
        for backend in order["backends"]:
            cell = (order["case_id"], backend)
            if protocol.get("skip_remaining_on_case_failure") and cell in failed_cells:
                skipped.append(
                    dict(
                        case_id=order["case_id"],
                        pair=order["pair"],
                        backend=backend,
                        reason="first attempt for this backend/case failed; not executed",
                    )
                )
                save(batch / "skipped.json", skipped)
                continue
            row = one_trial(batch, protocol, order["case_id"], order["pair"], backend)
            records.append(row)
            if not row["valid"]:
                failed_cells.add(cell)
            save(batch / "records.partial.json", records)
            print(
                json.dumps(
                    {key: row.get(key) for key in ("case_id", "pair", "backend", "valid", "error")}
                ),
                file=sys.stderr,
                flush=True,
            )
    verify_inputs(batch, protocol)
    save(batch / "records.json", records)
    save(batch / "budget-after.json", resource_state(protocol))
    failed = sum(not r["valid"] for r in records)
    save(
        batch / "status.json",
        dict(
            benchmark_id="B-FS-TOOLS",
            role="user-facing",
            trials=len(records),
            failed=failed,
            skipped=len(skipped),
            excluded=0,
            complete=failed == 0 and not skipped,
            measurements=sum(len(r["rows"]) for r in records if r["valid"]),
        ),
    )
    print(json.dumps(dict(trials=len(records), failed=failed)))


def measure(batch):
    require(not (batch / "records.partial.json").exists(), "refuse to reuse measured batch")
    unit = "pvisor-fs-user-" + uuid.uuid4().hex[:12]
    protocol = json.loads((batch / "protocol.json").read_text())
    docker = protocol.get("reference_configuration", {}).get("docker", {})
    command = ["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect", "--unit", unit]
    if docker:
        command.append("--slice=" + docker["budget_slice_unit"])
    command += [
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
        str(batch / "harness/user_workloads.py"),
        "--campaign",
        str(batch),
    ]
    save(batch / "campaign-command.json", command)
    with (
        (batch / "campaign.stdout.log").open("w") as out,
        (batch / "campaign.stderr.log").open("w") as err,
    ):
        process = subprocess.run(command, stdout=out, stderr=err, timeout=14400)
    require(process.returncode == 0, "campaign failed; retained logs have details")
    print((batch / "campaign.stdout.log").read_text(), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--prepare", type=Path)
    group.add_argument("--measure", type=Path)
    group.add_argument("--campaign", type=Path)
    group.add_argument("--preflight", type=Path)
    parser.add_argument("--payload-batch", type=Path)
    parser.add_argument("--rootfs", type=Path)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--docker-setup", type=Path)
    parser.add_argument("--stock-kernel-receipt", type=Path)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--cases")
    parser.add_argument("--backends")
    parser.add_argument("--seed", type=int, default=20261019)
    parser.add_argument("--timeout", type=int, default=90)
    args = parser.parse_args()
    if args.prepare:
        require(
            all((args.payload_batch, args.rootfs, args.artifacts)),
            "prepared payload/rootfs/artifacts required",
        )
        require(args.samples >= 1 and args.timeout > 0, "invalid samples/timeout")
        prepare(args)
    elif args.measure:
        measure(args.measure.resolve())
    elif args.campaign:
        campaign(args.campaign.resolve())
    else:
        batch = args.preflight.resolve()
        protocol = json.loads((batch / "protocol.json").read_text())
        results = [
            one_trial(batch, protocol, args.cases or "M01", 0, mode["id"], preflight=True)
            for mode in protocol["backends"]
        ]
        save(batch / "preflight-results.json", results)
        print(json.dumps([{k: r.get(k) for k in ("backend", "valid", "error")} for r in results]))


if __name__ == "__main__":
    main()
