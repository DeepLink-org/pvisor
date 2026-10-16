#!/usr/bin/env python3
"""Benchmark: B-FS-TOOLS, quick Ready and three resident tool rounds.

Question: How soon can a prepared environment execute a command, and what do
three resident rounds and fixed metadata/stream-write workloads cost?

Use prepare with a new assets directory and an owned Docker setup. run(config)
is invoked inside the retained private namespace, using reference_baselines'
original launch/Ready clocks and correctness checks. No interference gate,
warmup, build, image preparation or source-inventory scan runs during sampling.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
from types import SimpleNamespace

from filesystem_rounds_workload import OPERATIONS, STREAM_CALLS, STREAM_CHUNK


CONDITIONS = ("native", "pvisor-host", "pvisor-staged", "pvisor-vm", "pvisor-vm-writeback",
              "docker", "firecracker", "qemu", "qemu-microvm")


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def prepare(args):
    """Reflink the old inputs and install identical new scripts in all images."""
    source = args.source_assets.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    setup = json.loads(args.setup.read_text())
    log = dict(benchmark_id="B-FS-TOOLS", source_assets=str(source),
               setup=str(args.setup.resolve()), commands=[], state="preparing")
    save(output / "preparation.json", log)

    def command(argv):
        log["commands"].append(argv)
        result = subprocess.run(argv, text=True, capture_output=True, check=True, timeout=300)
        number = len(log["commands"])
        (output / f"prepare-{number:02d}.stdout").write_text(result.stdout)
        (output / f"prepare-{number:02d}.stderr").write_text(result.stderr)
        save(output / "preparation.json", log)
        return result.stdout.strip()

    try:
        for name in ("rootfs", "agent-env.ext4", "vmlinux", "bzImage", "kernel.config"):
            if (source / name).exists():
                command(["cp", "-a", "--reflink=always", str(source / name), str(output / name)])
        rootfs = output / "rootfs"
        # Pin just libpython for host executors. The host may have upgraded its
        # Python patch release since the frozen rootfs was prepared; mixing its
        # shared library with the frozen extensions can fail before any work.
        python_libraries = rootfs / "bench/host-python-libs"
        python_libraries.mkdir()
        library = rootfs / "lib64/libpython3.14.so.1.0"
        (python_libraries / library.name).symlink_to(library)
        harness = Path(__file__).resolve().parent
        for name in ("reference_workload.py", "filesystem_rounds_workload.py"):
            shutil.copy2(harness / name, rootfs / "bench" / name)
        fixture = rootfs / "work/_fs/fixture.json"
        values = json.loads(fixture.read_text())
        values["rounds"] = 3
        save(fixture, values)
        installed = ["bench/reference_workload.py", "bench/filesystem_rounds_workload.py",
                     "work/_fs/fixture.json"]
        # debugfs patches only these files in the cloned ext4 template. Verify
        # every replacement by dumping it back; debugfs exit status alone does
        # not reliably indicate command failure.
        commands = []
        for relative in installed:
            path = rootfs / relative
            if any(char in str(path) for char in ('"', "\\", "\n")):
                raise ValueError("unsupported debugfs preparation path")
            if relative != "bench/filesystem_rounds_workload.py":
                commands.append(f"rm /{relative}")
            commands.append(f'write "{path}" /{relative}')
        script = output / "debugfs.txt"
        script.write_text("\n".join(commands) + "\n")
        command(["debugfs", "-w", "-f", str(script), str(output / "agent-env.ext4")])
        for index, relative in enumerate(installed):
            dumped = output / f"verified-{index}.bin"
            command(["debugfs", "-R", f'dump /{relative} "{dumped}"', str(output / "agent-env.ext4")])
            if sha(dumped) != sha(rootfs / relative):
                raise ValueError("ext4 workload differs: " + relative)
        assets = json.loads((source / "assets.json").read_text())
        docker = ["docker", "--host", setup["docker_host"]]
        # Import the existing fixed tar once into this fresh owned daemon, then
        # add only the three verified files. No package or network access.
        base_image = command(docker + ["import", str(source / "agent-env.tar")])
        container = command(docker + ["create", "--entrypoint", "/bin/true", base_image])
        log["owned_preparation_container"] = container
        for relative in installed:
            command(docker + ["cp", str(rootfs / relative), f"{container}:/{relative}"])
        image = command(docker + ["commit", container])
        command(docker + ["rm", container])
        # Verify the committed image too, rather than only its source container.
        verification = command(docker + ["create", "--entrypoint", "/bin/true", image])
        for index, relative in enumerate(installed):
            dumped = output / f"oci-verified-{index}.bin"
            command(docker + ["cp", f"{verification}:/{relative}", str(dumped)])
            if sha(dumped) != sha(rootfs / relative):
                raise ValueError("Docker workload differs: " + relative)
        command(docker + ["rm", verification])
        assets.update(docker_image=image, rootfs=str(rootfs), disk=str(output / "agent-env.ext4"),
                      kernel=str(output / "vmlinux"), preparation_scope="derived resident-round inputs; not a new full-inventory audit")
        save(output / "assets.json", assets)
        log.update(state="prepared", docker_image=image, rootfs=str(rootfs),
                   host_python_sha256=sha(rootfs / "usr/bin/python3"),
                   host_libpython_sha256=sha(library),
                   installed_sha256={relative: sha(rootfs / relative) for relative in installed},
                   source_policy="reuse previously verified tools; verify changed workload bytes in rootfs/ext4/OCI; no full inventory scan")
    except BaseException as error:
        log.update(state="failed", error=str(error))
        raise
    finally:
        save(output / "preparation.json", log)
    print(json.dumps(dict(assets=str(output), state=log["state"], docker_image=image)))


def validate_rounds(result):
    """Check every round, including child cache policy and fixed work counts."""
    from reference_baselines import validate_python_cache, validate_tool_cache

    operations = result["filesystem"]
    if set(operations) != set(OPERATIONS):
        raise ValueError("missing or unexpected resident workload")
    for number in range(1, 4):
        current = {}
        for operation in OPERATIONS:
            rounds = operations[operation]["rounds"]
            if len(rounds) != 3 or [value["round"] for value in rounds] != [1, 2, 3]:
                raise ValueError("invalid resident round identities")
            value = rounds[number - 1]
            if value["workload"] != operation or value["worker_ms"] < 0:
                raise ValueError("invalid resident operation")
            current[operation] = value
        subset = dict(result, filesystem=current)
        validate_python_cache(subset)
        validate_tool_cache(subset, result["tool_scratch"])
        if current["hot_open"]["check"]["opens"] != 10000:
            raise ValueError("wrong hot-open work count")
        stream = current["stream_write"]["check"]
        if stream["writes"] != STREAM_CALLS or stream["bytes"] != STREAM_CALLS * len(STREAM_CHUNK) or stream["fsyncs"] != 1:
            raise ValueError("wrong stream-write work count")


def validate_stream_files(row, backend):
    """Read every final stream byte on the host, outside all measured clocks."""
    root = Path(row["logs"])
    staged = backend in ("pvisor-staged", "pvisor-vm")
    base = root / ("stage/upper/_fs/stream-written" if staged else "workspace/_fs/stream-written")
    disk = root / "rootfs.ext4"
    if staged and (root / "workspace/_fs/stream-written").exists():
        raise ValueError("stream writes reached staged lower")
    expected = STREAM_CHUNK * STREAM_CALLS
    for number in range(1, 4):
        if disk.exists():
            target = root / f"verified-stream-{number}.bin"
            result = subprocess.run(["debugfs", "-R", f'dump /work/_fs/stream-written/{number:02d}.bin "{target}"', str(disk)],
                                    text=True, capture_output=True, check=True)
            (root / f"verified-stream-{number}.stderr").write_text(result.stderr)
        else:
            target = base / f"{number:02d}.bin"
        if target.is_symlink() or target.read_bytes() != expected:
            raise ValueError("wrong host-visible stream bytes")


def run(config_path):
    """Collect a new, independently retained quick cohort in the private parent."""
    import reference_baselines as reference

    config = json.loads(Path(config_path).read_text())
    previous = json.loads(Path(config["quick_source_report"]).read_text())
    setup = json.loads(Path(config["setup"]).read_text())
    arguments = previous["arguments"].copy()
    for key in ("samples", "warmups", "seed", "memory_mib", "budget_memory_mib", "docker_root_pid"):
        arguments[key] = int(arguments[key])
    for key in ("assets", "binary", "output", "build_receipt", "firmware", "resource_budget"):
        arguments[key] = Path(arguments[key])
    for key in ("fc_system_receipt", "fc_reference_receipt", "qemu_system_receipt"):
        arguments[key] = None
    output = Path("/tmp/private-out") / Path(config["outputs"]["formal"]).name
    arguments.update(output=output, assets=Path(config["assets"]), samples=5, warmups=0,
                     seed=20261016, binary=Path(config["build"]) / "bin/pvisor",
                     backends=",".join(CONDITIONS),
                     prepared_host_python=True,
                     resource_budget=Path(setup["cgroup"]), docker_host=setup["docker_host"],
                     docker_root_pid=setup["daemon"]["pid"], resource_observation="off")
    output.mkdir(parents=True, exist_ok=False)
    report = {key: previous[key] for key in ("schema", "benchmark_id", "benchmark_ids",
              "pvisor_sha256", "kernel_sha256", "bzimage_sha256", "binary_source_commit",
              "binary_source_manifest_sha256", "host_kernel", "host_cpu_model", "docker_version",
              "docker_storage_driver", "firecracker_version", "qemu-system-x86_64_version")}
    report.update(recorded_at=datetime.now(timezone.utc).isoformat(),
                  docker_version=setup["docker_info"]["ServerVersion"],
                  docker_storage_driver=setup["docker_info"]["Driver"],
                  assets=json.loads((Path(config["assets"]) / "assets.json").read_text()),
                  arguments={key: str(value) for key, value in arguments.items()},
                  firmware_sha256=config["firmware_sha256"],
                  workload_preparation=json.loads((Path(config["assets"]) / "preparation.json").read_text()),
                  protocol=dict(kind="quick user-facing resident comparison", conditions=CONDITIONS,
                                samples_per_condition=5, resident_rounds=3, warmups=0, seed=20261016,
                                startup_metric="launch to REFERENCE_READY; excludes task work and exit",
                                task_metric="each operation worker_ms by resident round; no launch or exit",
                                stream_metric="write + fsync + close, with write/fsync/close phases separate",
                                guest_cache=1, default_writeback=0, opt_in_writeback=1,
                                directory_images="not eligible: ordinary mutable CLI workspace", dax=False,
                                interference_gate=False, input_inventory_gate=False,
                                correctness="normal exit, output/cache/Bundle/isolation and full written bytes",
                                retain_all_valid_slow_samples=True), rows=[], failures=[])
    for condition in CONDITIONS:
        folder = output / "conditions" / condition / "bin"
        folder.mkdir(parents=True)
        shutil.copy2(arguments["binary"], folder / "pvisor")
    save(output / "report.json", report)
    rng = random.Random(arguments["seed"])
    for trial in range(5):
        order = list(CONDITIONS)
        rng.shuffle(order)
        for condition in order:
            backend = "pvisor-vm" if condition == "pvisor-vm-writeback" else condition
            args = SimpleNamespace(**arguments)
            args.output = output / "conditions" / condition
            overrides = dict(PVISOR_VM_GUEST_CACHE="1", PVISOR_VM_FS_WRITEBACK="1" if condition == "pvisor-vm-writeback" else "0")
            if condition in ("native", "pvisor-host", "pvisor-staged"):
                overrides["LD_LIBRARY_PATH"] = str(Path(config["assets"]) / "rootfs/bench/host-python-libs")
            old = {key: os.environ.get(key) for key in overrides}
            os.environ.update(overrides)
            try:
                row = reference.run_trial(args, report, backend, "filesystem", trial)
                validate_rounds(row["result"])
                if {value["python"] for operation in row["result"]["filesystem"].values()
                    for value in operation["rounds"]} != {config["python_version"]}:
                    raise ValueError("workloads did not use the common frozen Python version")
                validate_stream_files(row, backend)
                row.update(condition=condition, benchmark_id="B-FS-TOOLS", writeback=condition == "pvisor-vm-writeback")
                report["rows"].append(row)
            except Exception as error:
                report["failures"].append(dict(condition=condition, trial=trial, reason=str(error)))
            finally:
                for key, value in old.items():
                    if value is None:
                        os.environ.pop(key, None)
                    else:
                        os.environ[key] = value
            save(output / "report.json", report)
        print(f"RESIDENT round {trial + 1}/5; valid={len(report['rows'])}; failed={len(report['failures'])}", flush=True)
    report["finished_at"] = datetime.now(timezone.utc).isoformat()
    save(output / "report.json", report)
    print("RESIDENT complete " + str(config["outputs"]["formal"]), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-assets", type=Path, required=True)
    parser.add_argument("--setup", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    prepare(parser.parse_args())
