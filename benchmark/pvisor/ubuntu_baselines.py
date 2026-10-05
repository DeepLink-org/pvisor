#!/usr/bin/env python3
"""Complete official Ubuntu on Firecracker/QEMU versus image-free pVisor.

Benchmark: B-STARTUP (benchmark/README.md#b-startup), role user-facing.
Motivation: many users would otherwise boot a full distribution per task.
Conclusion sought: the seconds a full Ubuntu boot costs and how much an
image-free pVisor VM avoids; not a VMM ranking under identical OS setups.
Design: official image, distribution kernel and services retained, private
disk copy per trial, separate batches never pooled.
"""

import argparse
import json
import os
import random
import re
import shutil
import signal
import subprocess
import threading
import time
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

from reference_baselines import digest, validate_guest_output
from v1.density import snapshot

BACKENDS = (
    "native",
    "pvisor-staged",
    "pvisor-vm-hostroot",
    "firecracker-ubuntu",
    "firecracker-ubuntu-firstboot",
    "qemu-ubuntu",
    "qemu-microvm-ubuntu",
)


def qemu_command(args, backend, disk, meta, mode):
    """Boot the whole Ubuntu GPT disk with its unmodified vendor kernel/initrd."""
    microvm = backend == "qemu-microvm-ubuntu"
    kernel = Path(meta["initrd"]).with_name(
        Path(meta["initrd"]).name.replace("initrd-generic", "vmlinuz-generic")
    )
    return [
        "qemu-system-x86_64",
        "-no-user-config",
        "-machine",
        "microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=off" if microvm else "q35",
        "-accel",
        "kvm",
        "-cpu",
        "host",
        "-smp",
        "2",
        "-m",
        str(2048 if mode == "ready" else args.memory_mib),
        "-nodefaults",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-no-reboot",
        "-kernel",
        str(kernel),
        "-initrd",
        meta["initrd"],
        "-append",
        f"console=ttyS0 reboot=t panic=1 rw quiet rd.driver.pre=virtio_mmio root=PARTUUID={meta['root_partuuid']} pvbench.mode={mode}",
        "-drive",
        f"file={disk},format=raw,if=none,id=root",
        "-device",
        "virtio-blk-device,drive=root" if microvm else "virtio-blk-pci,drive=root",
        "-netdev",
        "user,id=net,net=10.77.0.0/24,host=10.77.0.1,dns=10.77.0.3",
        "-device",
        ("virtio-net-device" if microvm else "virtio-net-pci")
        + ",netdev=net,mac=06:00:ac:10:00:02",
    ]


def protocol_line(line):
    """Accept only bare protocol lines or our named journald console records."""
    line = line.rstrip("\r\n")
    # agetty can emit ANSI control sequences immediately before a journal record.
    line = re.sub(
        r"\x1b\][^\x1b\x07]*(?:\x07|\x1b\\)|\x1b[P^_X][^\x1b]*\x1b\\|\x1b\[[0-?]*[ -/]*[@-~]",
        "",
        line,
    )
    line = re.sub(
        r"^(?:pvisor-ubuntu-reference login: )?\[\s*\d+\.\d+\] reference-bench\[\d+\]: ", "", line
    )
    return line


def validate_os(output):
    values = [
        json.loads(s.removeprefix("REFERENCE_OS_READY "))
        for s in output.splitlines()
        if s.startswith("REFERENCE_OS_READY ")
    ]
    if len(values) != 1:
        raise ValueError("Full Ubuntu readiness proof missing or duplicated")
    value = values[0]
    if (
        value.get("pid1") != "systemd"
        or "Ubuntu" not in value.get("os_release", "")
        or not value.get("kernel", "").endswith("-generic")
        or any(
            value.get(k) != "active"
            for k in ("multi_user", "network_online", "cloud_final", "ssh_socket")
        )
    ):
        raise ValueError(f"Ubuntu services not ready: {value}")
    return value


def run_trial(args, backend, mode, trial):
    root = args.output / "trials" / f"{mode}-{backend}-{trial:03d}"
    root.mkdir(parents=True)
    prep = time.perf_counter_ns()
    work = root / "workspace"
    vm = backend.startswith(("firecracker", "qemu"))
    if vm:
        work.mkdir()
    else:
        subprocess.run(["cp", "--reflink=auto", "-a", str(args.fixture), str(work)], check=True)
    env = {k: v for k, v in os.environ.items() if k in ("PATH", "LANG", "LC_ALL", "TZ")}
    env.update(
        PVISOR_RUN_HOME=str(root / "runs"),
        XDG_CONFIG_HOME=str(root / "config"),
        PVISOR_REFERENCE_TOOL_ROOT="/",
        PVISOR_REFERENCE_TOOLCHAIN=str(args.toolchain),
        PVISOR_REFERENCE_HARNESS=str(args.output / "harness"),
        PVISOR_REFERENCE_TMPDIR=str(work / "_tmp"),
        GIT_CONFIG_COUNT="1",
        GIT_CONFIG_KEY_0="safe.directory",
        GIT_CONFIG_VALUE_0="*",
    )
    payload = (
        [
            "/bin/sh",
            "-c",
            """printf 'REFERENCE_READY\\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\\n' """,
        ]
        if mode == "ready"
        else [
            "/usr/bin/python3",
            str(args.output / "harness/reference_workload.py"),
            "--mode",
            mode,
        ]
    )
    if backend == "native":
        argv = payload
    elif backend.startswith("pvisor"):
        argv = [
            str(args.output / "bin/pvisor"),
            "run",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--stdio",
            "inherit",
            "--timeout",
            "120s",
            "--stage",
            str(root / "stage"),
        ]
        if backend == "pvisor-vm-hostroot":
            argv += [
                "--vm",
                "--rootfs",
                "host",
                "--cpu",
                "2",
                "--memory",
                f"{2048 if mode == 'ready' else args.memory_mib}MiB",
            ]
        for key in (
            "PVISOR_REFERENCE_TOOL_ROOT",
            "PVISOR_REFERENCE_TOOLCHAIN",
            "PVISOR_REFERENCE_HARNESS",
            "PVISOR_REFERENCE_TMPDIR",
        ):
            argv += ["--pass-env", key]
        argv += ["--", *payload]
    elif vm:
        disk = root / "ubuntu.raw"
        subprocess.run(
            [
                "cp",
                "--reflink=auto",
                str(
                    args.assets
                    / ("ubuntu-stock.raw" if backend.endswith("firstboot") else "ubuntu-agent.raw")
                ),
                str(disk),
            ],
            check=True,
        )
        meta = json.loads((args.assets / "assets.json").read_text())
        if backend.startswith("qemu"):
            argv = qemu_command(args, backend, disk, meta, mode)
        else:
            cfg = root / "firecracker.json"
            cfg.write_text(
                json.dumps(
                    {
                        "boot-source": {
                            "kernel_image_path": str(args.assets / "ubuntu-vmlinux"),
                            "initrd_path": meta["initrd"],
                            "boot_args": f"console=ttyS0 reboot=t panic=1 rw quiet rd.driver.pre=virtio_mmio pvbench.mode={mode}",
                        },
                        "drives": [
                            {
                                "drive_id": "rootfs",
                                "path_on_host": str(disk),
                                "is_root_device": True,
                                "is_read_only": False,
                                "partuuid": meta["root_partuuid"],
                            }
                        ],
                        "machine-config": {
                            "vcpu_count": 2,
                            "mem_size_mib": 2048 if mode == "ready" else args.memory_mib,
                        },
                        "network-interfaces": [
                            {
                                "iface_id": "eth0",
                                "guest_mac": "06:00:ac:10:00:02",
                                "host_dev_name": "pvbench-tap",
                            }
                        ],
                    },
                    indent=2,
                )
                + "\n"
            )
            argv = [
                "bash",
                str(args.output / "harness/ubuntu_vm_network.sh"),
                "firecracker",
                "--no-api",
                "--config-file",
                str(cfg),
            ]
    else:
        raise ValueError(backend)
    argv = ["taskset", "--cpu-list", args.cpu_affinity, *argv]
    prep_ms = (time.perf_counter_ns() - prep) / 1e6
    (root / "command.json").write_text(
        json.dumps(
            {
                "argv": argv,
                "env_overrides": {k: v for k, v in env.items() if k.startswith("PVISOR_")},
                "prepare_ms": prep_ms,
            },
            indent=2,
        )
        + "\n"
    )
    marks = {"ready": [], "os_ready": [], "result": []}
    output, error, peak = [], [], [0]
    stopped, exited = threading.Event(), threading.Event()
    exit_time = []
    start = time.perf_counter_ns()
    proc = subprocess.Popen(
        argv,
        cwd=work,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )

    def reader():
        for line in proc.stdout:
            output.append(line)
            line = protocol_line(line.decode(errors="replace")).encode()
            if line.strip() == b"REFERENCE_READY" or line.startswith(b"REFERENCE_ENV_READY "):
                marks["ready"].append(time.perf_counter_ns())
            if line.startswith(b"REFERENCE_OS_READY "):
                marks["os_ready"].append(time.perf_counter_ns())
            if line.startswith(b"REFERENCE_RESULT "):
                marks["result"].append(time.perf_counter_ns())

    def monitor():
        while not stopped.is_set():
            rss, _ = snapshot({proc.pid})
            peak[0] = max(peak[0], rss)
            stopped.wait(0.02)

    def waiter():
        proc.wait()
        exit_time.append(time.perf_counter_ns())
        exited.set()

    threads = [
        threading.Thread(target=reader),
        threading.Thread(target=lambda: error.append(proc.stderr.read())),
        threading.Thread(target=monitor),
        threading.Thread(target=waiter),
    ]
    for t in threads:
        t.start()
    if not exited.wait(args.timeout):
        os.killpg(proc.pid, signal.SIGKILL)
        exited.wait()
    stopped.set()
    for t in threads:
        t.join()
    proc.stdout.close()
    proc.stderr.close()
    out, err = b"".join(output).decode(errors="replace"), b"".join(error).decode(errors="replace")
    (root / "stdout.log").write_text(out)
    (root / "stderr.log").write_text(err)
    if proc.returncode or len(marks["ready"]) != 1 or len(marks["result"]) != 1:
        raise RuntimeError(
            f"{backend}/{mode}: exit={proc.returncode}; logs: {root}; {out[-800:]} {err[-500:]}"
        )
    protocol = "\n".join(protocol_line(s) for s in out.splitlines())
    result = json.loads(
        next(
            s.removeprefix("REFERENCE_RESULT ")
            for s in protocol.splitlines()
            if s.startswith("REFERENCE_RESULT ")
        )
    )
    if result.get("mode") != mode or result.get("correctness") != "passed":
        raise ValueError("Requested task did not pass")
    proof = validate_os(protocol) if vm else None
    if vm:
        validate_guest_output(protocol, mode)
    if backend.startswith("pvisor"):
        bundles = list((root / "stage").glob("run-bundle.json"))
        if len(bundles) != 1:
            raise ValueError("Missing pVisor run bundle")
        bundle = json.loads(bundles[0].read_text())
        if (
            bundle["run"]["state"] != "completed"
            or bundle["run"]["exit_code"] != 0
            or bundle["run"]["executor"]["isolation"]
            != ("virtual_machine" if backend == "pvisor-vm-hostroot" else "host_process")
        ):
            raise ValueError("pVisor did not complete in requested isolation mode")
        if not bundle["safety"]["filesystem_changes_staged"]:
            raise ValueError("pVisor staging was not enabled")
        if (
            mode in ("tools", "claude", "codex")
            and (work / "python/adder.py").read_text() != "def add(a, b):\n    return a - b\n"
        ):
            raise ValueError("Staged writes modified original workspace")
    row = {
        "backend": backend,
        "mode": mode,
        "trial": trial,
        "prepare_ms": prep_ms,
        "ready_ms": (marks["ready"][0] - start) / 1e6,
        "os_ready_ms": (marks["os_ready"][0] - start) / 1e6 if vm else None,
        "result_ms": (marks["result"][0] - start) / 1e6,
        "completion_ms": (exit_time[0] - start) / 1e6,
        "peak_tree_rss_kib": peak[0],
        "result": result,
        "os_proof": proof,
        "correctness": "passed",
        "logs": str(root),
    }
    if vm:
        disk.unlink()
    for folder in (work, root / "stage/upper"):
        for name in ("_model-requests.json", "_cli-output.json"):
            if (folder / name).exists():
                shutil.copy2(folder / name, root / name)
        shutil.rmtree(folder, ignore_errors=True)
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ("assets", "fixture", "binary", "output"):
        p.add_argument("--" + name, type=Path, required=True)
    p.add_argument("--toolchain", type=Path)
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--backends", default=",".join(BACKENDS))
    p.add_argument("--modes", default="ready,env,filesystem,tools,claude,codex")
    p.add_argument("--memory-mib", type=int, default=16384)
    p.add_argument("--cpu-affinity", default="0,1")
    p.add_argument("--timeout", type=int, default=180)
    args = p.parse_args()
    for key in ("assets", "fixture", "binary", "output"):
        setattr(args, key, getattr(args, key).resolve())
    args.toolchain = (
        args.toolchain
        or Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    ).resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    shutil.copytree(
        Path(__file__).parent,
        args.output / "harness",
        ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache"),
    )
    rows, caps = [], {}
    meta = {
        "schema": "pvisor-full-ubuntu-reference/v1",
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {k: str(v) for k, v in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "pvisor_sha256": digest(args.output / "bin/pvisor"),
        "driver_sha256": digest(Path(__file__)),
        "workload_sha256": digest(args.output / "harness/reference_workload.py"),
        "network_helper_sha256": digest(args.output / "harness/ubuntu_vm_network.py"),
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "source_status": subprocess.check_output(["git", "status", "--porcelain"], text=True),
        "host_os": Path("/etc/os-release").read_text(),
        "host_kernel": os.uname().release,
        "firecracker_version": subprocess.check_output(
            ["firecracker", "--version"], text=True
        ).strip(),
        "protocol": {
            "shape": f"2 vCPU; ready 2048 MiB, tasks {args.memory_mib} MiB; CPUs {args.cpu_affinity}",
            "cache": "Warm host page cache; fresh disk reflink/workspace per trial; no memory snapshot",
            "image_policy": "pVisor --rootfs host: no image; Firecracker/QEMU complete official Ubuntu image",
            "task": "Same fixtures and CLI versions; Ubuntu native apt tool builds may differ; private workspace TMPDIR on all backends",
            "timer": "Launch to task ready/result/exit; clone and provisioning costs recorded separately",
            "network": "Firecracker: private TAP plus QEMU -machine none DNS/NAT helper; QEMU: built-in user networking; all launcher helpers included in time/RSS; same-guest fixture model, no real inference",
            "resource": "Owned launcher tree RSS sum sampled every 20 ms; shared pages can be double-counted",
            "order": "Random backend per round, seed 20261004",
        },
        "load_before": os.getloadavg(),
    }

    selected = args.backends.split(",")
    if any(backend not in BACKENDS for backend in selected):
        p.error("Unknown backend")
    if any(backend.startswith("qemu") for backend in selected):
        meta["qemu_version"] = subprocess.check_output(
            ["qemu-system-x86_64", "--version"], text=True
        ).strip()
        assets = meta["assets"]
        kernel = Path(assets["initrd"]).with_name(
            Path(assets["initrd"]).name.replace("initrd-generic", "vmlinuz-generic")
        )
        if digest(kernel) != assets["assets"][kernel.name]["sha256"]:
            raise ValueError("QEMU kernel does not match the official Ubuntu artifact")
        meta["qemu_kernel_sha256"] = digest(kernel)

    def save():
        tmp = args.output / "report.tmp"
        tmp.write_text(
            json.dumps(
                meta | {"rows": rows, "capabilities": caps, "load_after": os.getloadavg()}, indent=2
            )
            + "\n"
        )
        tmp.replace(args.output / "report.json")

    rng = random.Random(20261004)
    save()
    for mode in args.modes.split(","):
        available = []
        for backend in args.backends.split(","):
            if backend.endswith("firstboot") and mode != "ready":
                continue
            key = f"{mode}/{backend}"
            try:
                run_trial(args, backend, mode, -100)
            except Exception as e:
                caps[key] = {"state": "failed-preflight", "reason": str(e)}
                print("PREFLIGHT FAIL", key, str(e)[:500], flush=True)
            else:
                caps[key] = {"state": "available"}
                available.append(backend)
                print("PREFLIGHT PASS", key, flush=True)
            save()
        for trial in range(-args.warmups, args.samples):
            order = available.copy()
            rng.shuffle(order)
            for backend in order:
                try:
                    row = run_trial(args, backend, mode, trial)
                except Exception as e:
                    caps[f"{mode}/{backend}"].setdefault("failures", []).append(
                        {"trial": trial, "reason": str(e)}
                    )
                    print("TRIAL FAIL", mode, backend, trial, str(e)[:500], flush=True)
                else:
                    if trial >= 0:
                        rows.append(row)
                    print(
                        "TRIAL PASS", mode, backend, trial, round(row["result_ms"], 2), flush=True
                    )
                save()
    print(args.output / "report.json", flush=True)


if __name__ == "__main__":
    main()
