#!/usr/bin/env python3
"""Prepare one B-STARTUP event row and validate an untimed preflight.

Benchmark: B-STARTUP (benchmark/REGISTRY.md#b-startup), role user-facing.
Motivation: choose an executor for prepared disposable environments.
Conclusion sought: same-launch Ready and normal Exit levels.
Design: fresh inputs; checked output and Run Bundles; warm caches; 2 vCPU/128 MiB.
"""

import argparse
import importlib.util
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPO = ROOT.parent
sys.path.insert(0, str(REPO / "benchmark/support"))
_spec = importlib.util.spec_from_file_location("benchmark_common", ROOT / "common.py")
COMMON = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(COMMON)
from firecracker_kernels import verify_kernel_receipt  # noqa: E402
from hooks import cleanup_container, prepare  # noqa: E402
from reference_inputs import verify_reference_inputs  # noqa: E402
from subjects import LABELS, NAMES, subject  # noqa: E402

BACKENDS = NAMES
BOOT = "console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/bench/init quiet pvbench.mode=ready pvbench.scratch=executor"


def preserve_listener(binary):
    """Refuse to launch a different executable against a live Host listener."""
    generation = Path("/tmp") / f"pvisor-host-{os.geteuid()}" / "generation"
    if not generation.exists():
        return
    state = json.loads(generation.read_text())
    pid = state.get("pid")
    if not isinstance(pid, int) or pid <= 0:
        raise ValueError("existing Host listener cannot be verified")
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return
    try:
        matches = COMMON.digest(Path("/proc") / str(pid) / "exe") == COMMON.digest(binary)
    except OSError as error:
        if not shutil.which("b3sum") or not state.get("digest"):
            raise ValueError("cannot verify existing Host; b3sum is required") from error
        selected = subprocess.check_output(["b3sum", "--no-names", str(binary)], text=True).strip()
        matches = selected == state["digest"]
    if not matches:
        raise ValueError(
            "selected binary differs from existing Host listener; use its paired binary or a private namespace"
        )


def prepare_template(
    template, output=None, assets=None, binary=None, receipt=None, cpu_affinity="0,1", row=None
):
    template = Path(template).resolve()
    table = COMMON.table_template(template)
    if row is None and len(table) != 1:
        raise ValueError("select one startup row during preparation")
    row = row or next(iter(table))
    backend = next((key for key, label in LABELS.items() if label == row), None)
    if backend is None or row not in table:
        raise ValueError("one registered startup condition is required")
    source = template.with_name(backend.replace("-", "_") + "_timing.md")
    document = COMMON.markdown_inventory(source)
    if len(document["experiments"]) != 1 or document["experiments"][0]["id"] != backend:
        raise ValueError("one matching startup plan is required")
    plan = document["experiments"][0]["plan"]
    if (
        plan["prepare"]
        or plan["conclude"]
        or plan["cleanup"]
        or plan["expected_stdout"] is not None
    ):
        raise ValueError("startup correctness uses the Ready/Exit adapter, not process hooks")
    if table[row] != f"$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend {backend}":
        raise ValueError("startup row differs from its fixed timing script")
    import timing as ready

    batch = COMMON.create_batch(template, output)
    original_affinity = os.sched_getaffinity(0)
    try:
        selected = {int(cpu) for cpu in cpu_affinity.split(",")}
        available = os.sched_getaffinity(0)
        if not selected or not selected <= available:
            raise ValueError("requested CPU affinity is unavailable")
        os.sched_setaffinity(0, selected)
        asset_path = assets or os.environ.get("PVISOR_BENCHMARK_ASSETS")
        if backend != "native" and asset_path is None:
            raise ValueError("prepared inputs required: use --assets or PVISOR_BENCHMARK_ASSETS")
        assets = None if backend == "native" else Path(asset_path).resolve()
        before = verify_reference_inputs(assets) if assets else None
        artifacts = [
            source,
            Path(os.environ.get("SEMSPEC_BIN", REPO / "target/semaspec/debug/semaspec")),
        ]
        if assets:
            artifacts += [assets / "assets.json", assets / "input-manifest.json"]
        binary = Path(binary or REPO / "target/release/pvisor").resolve()
        if backend.startswith("pvisor-"):
            preserve_listener(binary)
            artifacts.append(binary)
        kernel = None
        if backend in ("fc-system", "qemu", "qemu-microvm"):
            receipt_path = receipt or os.environ.get("PVISOR_BENCHMARK_KERNEL_RECEIPT")
            if receipt_path is None:
                raise ValueError(
                    "stock kernel receipt required: use --kernel-receipt or PVISOR_BENCHMARK_KERNEL_RECEIPT"
                )
            receipt = Path(receipt_path).resolve()
            kernel = verify_kernel_receipt(receipt, "fc-system")
            if backend.startswith("qemu") and "initrd" not in kernel["paths"]:
                raise ValueError(
                    "these QEMU templates require the prepared stock initrd; edit the template for an initrd-free condition"
                )
            artifacts += [receipt, *map(Path, kernel["paths"].values())]
        image = None
        if backend == "docker":
            image = os.environ.get("PVISOR_BENCHMARK_DOCKER_IMAGE") or json.loads(
                (assets / "assets.json").read_text()
            ).get("docker_image")
            if not isinstance(image, str) or not image.startswith("sha256:"):
                raise ValueError("Docker requires an immutable pre-prepared image ID")
            actual = subprocess.check_output(
                ["docker", "inspect", "--format", "{{.Id}}", image], text=True
            ).strip()
            if image != actual:
                raise ValueError("prepared Docker image identity differs")
        executable = (
            "firecracker"
            if backend == "fc-system"
            else "qemu-system-x86_64"
            if backend.startswith("qemu")
            else "docker"
            if backend == "docker"
            else None
        )
        if executable:
            resolved = shutil.which(executable)
            if not resolved:
                raise ValueError("missing executable: " + executable)
            artifacts.append(Path(resolved))
        protocol = dict(
            benchmark_id="B-STARTUP",
            batch=batch.name,
            backends=[backend],
            assets=str(assets) if assets else None,
            kernel=kernel,
            kernel_receipt=str(receipt) if kernel else None,
            input_before=before,
            boot_args=BOOT if backend == "fc-system" else None,
            device_transport="virtio-mmio" if backend == "fc-system" else None,
            host_isolation="rootless_process",
            staged_isolation="rootless_process",
            cpu_affinity=cpu_affinity,
            guest_cpus=2,
            guest_memory_mib=128,
            cache="warm; no host cache eviction",
            docker_command=["docker"],
            container_name="pvisor-startup-" + batch.name[-18:],
            docker_image=image,
            completion="normal zero exit; controlled Firecracker termination is never completion",
            scope=COMMON.READY_EXIT_SCOPE,
            measurement_engine="python-perf_counter_ns",
            ready="exact REFERENCE_READY line received",
            plan=plan,
            specification_sha256=document["source_sha256"],
            host=dict(machine=platform.machine(), processor=platform.processor() or "unavailable"),
            inherited_environment={
                key: os.environ[key]
                for key in ("PATH", "LANG", "LC_ALL", "TZ")
                if key in os.environ
            },
        )
        COMMON.save(batch / "protocol.json", protocol)
        artifacts.append(batch / "protocol.json")
        helpers = [
            Path(__file__),
            template.with_name("hooks.py"),
            template.with_name("audit.py"),
            REPO / "benchmark/run.py",
            REPO / "benchmark/support/reference_inputs.py",
            REPO / "benchmark/support/firecracker_kernels.py",
        ]
        helpers += [template.with_name(name) for name in ("timing.py", "subjects.py")]
        if source != template:
            helpers.append(source)
        variables = dict(
            BINARY=binary,
            ASSETS=assets,
            IMAGE=image,
            CPU=cpu_affinity,
            CONTAINER=protocol["container_name"],
            BATCH_NAME=batch.name,
            VMLINUX=kernel["paths"]["vmlinuz"] if kernel else "",
            INITRD=kernel["paths"].get("initrd", "") if kernel else "",
        )
        expanded = variables | {"REPO": REPO, "BATCH": batch}
        protocol["subject"] = subject(plan, expanded)
        executable = shutil.which(protocol["subject"][0])
        if executable is None:
            raise ValueError("missing subject executable: " + protocol["subject"][0])
        executable = Path(executable).resolve(strict=True)
        protocol["subject_executable"] = str(executable)
        artifacts.append(executable)
        protocol["environment"] = {
            key: subject({"argv": [value]}, expanded)[0] for key, value in plan["env"].items()
        }
        protocol["cwd"] = subject({"argv": [plan["cwd"] or "@BATCH@/workspace"]}, expanded)[0]
        COMMON.save(batch / "protocol.json", protocol)
        if COMMON.digest(source) != document["source_sha256"]:
            raise ValueError("startup specification changed during preparation")
        prepare(batch, backend)
        COMMON.materialize(
            template,
            batch,
            variables,
            artifacts,
            batch / "workspace",
            helpers,
            row_config=dict(rows={row: backend}, runs=plan["runs"], warmup=plan["warmup"]),
        )
        ready.run_trial(batch, "preflight", 0, reset=False)
        return batch
    except BaseException as error:
        COMMON.save(batch / "failure.json", dict(error=str(error), backend=backend))
        if (batch / "manifest.json").exists():
            manifest = json.loads((batch / "manifest.json").read_text())
            manifest["status"], manifest["error"] = "failed", str(error)
            COMMON.save(batch / "manifest.json", manifest)
        if (batch / "protocol.json").exists():
            cleanup_container(batch)
        raise
    finally:
        os.sched_setaffinity(0, original_affinity)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("template", type=Path)
    p.add_argument("--output", type=Path)
    p.add_argument("--row")
    p.add_argument("--assets", type=Path)
    p.add_argument("--binary", type=Path)
    p.add_argument("--kernel-receipt", type=Path)
    p.add_argument("--cpu-affinity", default="0,1")
    args = p.parse_args()
    try:
        print(
            prepare_template(
                args.template,
                args.output,
                args.assets,
                args.binary,
                args.kernel_receipt,
                args.cpu_affinity,
                row=args.row,
            )
        )
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        p.exit(1, f"{error}\n")
