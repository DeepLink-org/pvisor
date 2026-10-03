#!/usr/bin/env python3
"""Measure local familiar runtimes with one prepared complete Agent environment."""

import argparse
import hashlib
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

from bench import percentile
from v1.density import snapshot


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def validate_guest_output(output, mode):
    """VMM exit zero does not imply the guest workload or shutdown succeeded."""
    if "Kernel panic" in output:
        raise ValueError("guest kernel panicked")
    if not re.search(r"REFERENCE_EXIT 0\r?$", output, re.M):
        raise ValueError("guest workload did not complete successfully")
    values = [
        json.loads(line.removeprefix("REFERENCE_RESULT "))
        for line in output.splitlines()
        if line.startswith("REFERENCE_RESULT ")
    ]
    if (
        len(values) != 1
        or values[0].get("mode") != mode
        or values[0].get("correctness") != "passed"
    ):
        raise ValueError("guest result does not match the requested successful task")


def pin_private_docker_tree(root_pid, docker_host, affinity):
    """Only pin the explicitly selected private daemon and its owned descendants."""
    root = Path("/proc") / str(root_pid)
    argv = [v.decode() for v in (root / "cmdline").read_bytes().split(b"\0") if v]
    if (
        not argv
        or Path(argv[0]).name != "dockerd"
        or docker_host not in argv
        or root.stat().st_uid != os.getuid()
    ):
        raise ValueError(
            "--docker-root-pid must identify this user's private daemon on --docker-host"
        )
    cpus = set(map(int, affinity.split(",")))
    if not cpus <= os.sched_getaffinity(0):
        raise ValueError("requested CPUs are outside the host process's allowed affinity")
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            processes[int(path.name)] = int(
                (path / "stat").read_text().rsplit(") ", 1)[1].split()[1]
            )
        except (OSError, ValueError):
            continue
    owned = {root_pid}
    while True:
        more = {pid for pid, parent in processes.items() if parent in owned} - owned
        if not more:
            break
        owned.update(more)
    for pid in owned:
        try:
            threads = list((Path("/proc") / str(pid) / "task").iterdir())
        except FileNotFoundError:
            continue
        for thread in threads:
            try:
                os.sched_setaffinity(int(thread.name), cpus)
            except ProcessLookupError:
                pass


def run_trial(args, metadata, backend, mode, trial):
    root = args.output / "trials" / f"{mode}-{backend}-{trial:03d}"
    root.mkdir(parents=True)
    work = root / "workspace"
    prep = time.perf_counter_ns()
    subprocess.run(
        ["cp", "--reflink=auto", "-a", str(args.assets / "rootfs/work"), str(work)], check=True
    )
    stage = root / "stage"
    env = {
        k: v
        for k, v in os.environ.items()
        if k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
    }
    env.update(
        PVISOR_RUN_HOME=str(root / "runs"),
        XDG_CONFIG_HOME=str(root / "config"),
        PVISOR_STARTUP_TIMING="0",
        GIT_CONFIG_COUNT="1",
        GIT_CONFIG_KEY_0="safe.directory",
        GIT_CONFIG_VALUE_0="*",
    )
    env.pop("PVISOR_TEST_ALLOW_NO_USERNS", None)
    image = metadata["assets"]["docker_image"]
    rootfs = args.assets / "rootfs"
    isvm = backend in ("firecracker", "qemu", "qemu-microvm")
    payload = (
        [
            "/bin/sh",
            "-c",
            'printf \'REFERENCE_READY\\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\\n\'',
        ]
        if mode == "ready"
        else ["/usr/bin/python3", "/bench/reference_workload.py", "--mode", mode]
    )
    if backend == "native":
        argv = (
            payload
            if mode == "ready"
            else ["/usr/bin/python3", str(rootfs / "bench/reference_workload.py"), "--mode", mode]
        )
    elif backend == "docker":
        if args.cpu_affinity:
            payload = ["/bench/affinity", args.cpu_affinity, *payload]
        argv = [
            "docker",
            "-H",
            args.docker_host,
            "run",
            "--rm",
            "--cidfile",
            str(root / "container.cid"),
            "--network",
            "none",
            "--workdir",
            "/work",
            "--mount",
            f"type=bind,source={work},target=/work",
            "--entrypoint",
            payload[0],
            image,
            *payload[1:],
        ]
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
        ]
        if backend == "pvisor-vm":
            argv += [
                "--vm",
                "--rootfs",
                str(rootfs),
                "--cpu",
                "2",
                "--memory",
                f"{128 if mode == 'ready' else args.memory_mib}MiB",
                "--stage",
                str(stage),
            ]
            if args.firmware:
                argv += ["--vm-library-dir", str(args.firmware)]
        if backend == "pvisor-staged":
            argv += ["--stage", str(stage)]
        if backend in ("pvisor-host", "pvisor-staged") and mode != "ready":
            payload = [
                "/usr/bin/python3",
                str(rootfs / "bench/reference_workload.py"),
                "--mode",
                mode,
            ]
        argv += ["--", *payload]
    elif isvm:
        disk = root / "rootfs.ext4"
        subprocess.run(
            ["cp", "--reflink=auto", str(args.assets / "agent-env.ext4"), str(disk)], check=True
        )
        boot = f"console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/bench/init quiet pvbench.mode={mode}"
        mem = 128 if mode == "ready" else args.memory_mib
        if backend == "firecracker":
            boot = boot.replace(" pci=off", "")
            config = {
                "boot-source": {
                    "kernel_image_path": str(args.assets / "vmlinux"),
                    "boot_args": boot,
                },
                "drives": [
                    {
                        "drive_id": "rootfs",
                        "path_on_host": str(disk),
                        "is_root_device": True,
                        "is_read_only": False,
                    }
                ],
                "machine-config": {"vcpu_count": 2, "mem_size_mib": mem},
            }
            cfg = root / "firecracker.json"
            cfg.write_text(json.dumps(config))
            argv = ["firecracker", "--enable-pci", "--no-api", "--config-file", str(cfg)]
        else:
            machine = (
                "microvm,acpi=off,x-option-roms=off,pit=off,pic=off,rtc=off"
                if backend == "qemu-microvm"
                else "q35"
            )
            if backend == "qemu":
                boot = boot.replace(" pci=off", "")
            if backend == "qemu-microvm":
                boot = boot.replace("reboot=k", "reboot=t")
            argv = [
                "qemu-system-x86_64",
                "-machine",
                machine,
                "-accel",
                "kvm",
                "-cpu",
                "host",
                "-smp",
                "2",
                "-m",
                str(mem),
                "-nodefaults",
                "-display",
                "none",
                "-serial",
                "stdio",
                "-no-reboot",
                "-kernel",
                str(args.assets / "bzImage"),
                "-append",
                boot,
                "-drive",
                f"file={disk},format=raw,if=none,id=root",
                "-device",
                "virtio-blk-device,drive=root"
                if backend == "qemu-microvm"
                else "virtio-blk-pci,drive=root",
            ]
    else:
        raise ValueError(backend)
    if args.cpu_affinity:
        argv = ["taskset", "--cpu-list", args.cpu_affinity, *argv]
    prep_ms = (time.perf_counter_ns() - prep) / 1e6
    ready = []
    result_times = []
    out = []
    err = []
    peak = [0]
    stop = threading.Event()
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

    def stdout():
        for line in proc.stdout:
            out.append(line)
            if line.strip() == b"REFERENCE_READY" or line.startswith(b"REFERENCE_ENV_READY "):
                ready.append(time.perf_counter_ns())
            if line.startswith(b"REFERENCE_RESULT "):
                result_times.append(time.perf_counter_ns())

    def monitor():
        while not stop.is_set():
            roots = {proc.pid}
            if backend == "docker" and args.docker_root_pid:
                roots.add(args.docker_root_pid)
                cidfile = root / "container.cid"
                if cidfile.is_file():
                    cid = cidfile.read_text().strip()
                    if cid:
                        # containerd shims are detached from dockerd's parent tree.
                        for path in Path("/proc").iterdir():
                            if not path.name.isdigit():
                                continue
                            try:
                                argv = [
                                    v.decode()
                                    for v in (path / "cmdline").read_bytes().split(b"\0")
                                    if v
                                ]
                            except (OSError, UnicodeError):
                                continue
                            if (
                                argv
                                and Path(argv[0]).name == "containerd-shim-runc-v2"
                                and cid in argv
                            ):
                                roots.add(int(path.name))
            rss, _ = snapshot(roots)
            peak[0] = max(peak[0], rss)
            stop.wait(0.02)

    exit_ns = []
    exited = threading.Event()

    def waiter():
        proc.wait()
        exit_ns.append(time.perf_counter_ns())
        exited.set()

    threads = [
        threading.Thread(target=waiter),
        threading.Thread(target=stdout),
        threading.Thread(target=lambda: err.append(proc.stderr.read())),
        threading.Thread(target=monitor),
    ]
    for t in threads:
        t.start()
    try:
        if not exited.wait(timeout=30 if mode in ("ready", "env") else 120):
            os.killpg(proc.pid, signal.SIGKILL)
            exited.wait()
    finally:
        stop.set()
        for t in threads:
            t.join()
        proc.stdout.close()
        proc.stderr.close()
    ended = exit_ns[0]
    output = b"".join(out).decode(errors="replace")
    error = b"".join(err).decode(errors="replace")
    (root / "stdout.log").write_text(output)
    (root / "stderr.log").write_text(error)
    (root / "command.json").write_text(
        json.dumps({"argv": argv, "exit": proc.returncode, "prepare_ms": prep_ms}, indent=2)
    )
    if proc.returncode != 0 or len(ready) != 1 or len(result_times) != 1:
        raise RuntimeError(
            f"{mode}/{backend}: exit {proc.returncode}, ready={len(ready)}, result={len(result_times)}; {root}\n{error[-1000:]}\n{output[-1500:]}"
        )
    result = json.loads(
        next(
            line.removeprefix("REFERENCE_RESULT ")
            for line in output.splitlines()
            if line.startswith("REFERENCE_RESULT ")
        )
    )
    assert result["correctness"] == "passed" and result["mode"] == mode
    if isvm:
        validate_guest_output(output, mode)
    if backend.startswith("pvisor"):
        bundles = list((root / "runs").glob("*/run-bundle.json")) + list(
            stage.glob("run-bundle.json")
        )
        assert len(bundles) == 1
        bundle = json.loads(bundles[0].read_text())
        assert bundle["run"]["state"] == "completed" and bundle["run"]["exit_code"] == 0
        assert bundle["run"]["executor"]["isolation"] == (
            "virtual_machine" if backend == "pvisor-vm" else "host_process"
        )
        if backend in ("pvisor-vm", "pvisor-staged"):
            assert bundle["safety"]["filesystem_changes_staged"]
        if mode in ("tools", "claude", "codex"):
            assert (
                (work / "python/adder.py").read_text() == "def add(a, b):\n    return a - b\n"
                if backend in ("pvisor-vm", "pvisor-staged")
                else True
            )
    row = {
        "backend": backend,
        "mode": mode,
        "trial": trial,
        "prepare_ms": prep_ms,
        "ready_ms": (ready[0] - start) / 1e6,
        "result_ms": (result_times[0] - start) / 1e6,
        "completion_ms": (ended - start) / 1e6,
        "peak_tree_rss_kib": peak[0],
        "memory_scope": "CLI + private daemon + exact container shim/descendants; sampled RSS sum"
        if backend == "docker" and args.docker_root_pid
        else "owned launcher tree; Docker daemon/container RSS excluded",
        "result": result,
        "correctness": "passed",
        "logs": str(root),
    }
    for name in ("_model-requests.json", "_cli-output.json"):
        for directory in (work, stage / "upper"):
            if (directory / name).exists():
                shutil.copy2(directory / name, root / name)
    shutil.rmtree(work)
    if isvm:
        (root / "rootfs.ext4").unlink()
    if stage.exists():
        shutil.rmtree(stage / "upper", ignore_errors=True)
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--assets", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--firmware", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--docker-host", default="unix:///tmp/pv-docker-v2/docker.sock")
    p.add_argument(
        "--backends",
        default="native,pvisor-host,pvisor-staged,pvisor-vm,docker,firecracker,qemu,qemu-microvm",
    )
    p.add_argument("--modes", default="ready,env,filesystem,tools,claude,codex")
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--memory-mib", type=int, default=16384)
    p.add_argument("--docker-root-pid", type=int)
    p.add_argument(
        "--cpu-affinity", default="0,1", help="Common host CPU affinity; empty string disables it"
    )
    args = p.parse_args()
    if args.cpu_affinity and "docker" in args.backends.split(",") and not args.docker_root_pid:
        p.error(
            "CPU-controlled Docker measurement requires --docker-root-pid for the private daemon"
        )
    if args.cpu_affinity and args.docker_root_pid:
        pin_private_docker_tree(args.docker_root_pid, args.docker_host, args.cpu_affinity)
    for key in ("assets", "binary", "output"):
        setattr(args, key, getattr(args, key).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    shutil.copytree(
        Path(__file__).parent,
        args.output / "harness",
        ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache"),
    )
    metadata = {
        "schema": "pvisor-reference-environment/v1",
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {k: str(v) for k, v in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "source_status": subprocess.check_output(["git", "status", "--porcelain"], text=True),
        "pvisor_sha256": digest(args.output / "bin/pvisor"),
        "kernel_sha256": digest(args.assets / "vmlinux"),
        "bzimage_sha256": digest(args.assets / "bzImage"),
        "driver_sha256": digest(Path(__file__)),
        "workload_sha256": digest(args.assets / "rootfs/bench/reference_workload.py"),
        "docker_idle_tree_rss_kib": snapshot({args.docker_root_pid})[0]
        if args.docker_root_pid
        else None,
        "host_os": Path("/etc/os-release").read_text(),
        "host_kernel": os.uname().release,
        "load_before": os.getloadavg(),
        "protocol": {
            "cache": "warm; no eviction",
            "image_preparation": "excluded from timed job; measured separately",
            "vm_shape": "2 vCPU; shell ready 128 MiB; complete environment 16 GiB configured RAM",
            "agent_model": "same-guest deterministic fixture; no real inference",
            "codex_sandbox": "danger-full-access uniformly; fixed commands, outer runtime boundary",
            "resource_scope": f"All launch trees bound to host CPUs {args.cpu_affinity or 'unrestricted'}; Docker private daemon pinned separately; VMs 2 vCPU; Rust -j2; RSS sums may double-count shared pages",
            "order": "seeded random backend per round",
            "percentile": "linear interpolation",
        },
    }
    for tool in ("docker", "firecracker", "qemu-system-x86_64"):
        metadata[tool + "_version"] = subprocess.run(
            [tool, "--version"], capture_output=True, text=True
        ).stdout.strip()
    rows = []
    caps = {}
    rng = random.Random(20261004)

    def save():
        report = metadata | {"rows": rows, "capabilities": caps, "load_after": os.getloadavg()}
        temporary = args.output / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(args.output / "report.json")

    save()
    for mode in args.modes.split(","):
        available = []
        for backend in args.backends.split(","):
            try:
                run_trial(args, metadata, backend, mode, -100)
            except Exception as error:
                import traceback

                (args.output / "trials" / f"{mode}-{backend}--100" / "failure.txt").write_text(
                    traceback.format_exc()
                )
                caps[mode + "/" + backend] = {"state": "failed-preflight", "reason": str(error)}
                print("preflight failed", mode, backend, str(error)[:500], flush=True)
            else:
                available.append(backend)
                caps[mode + "/" + backend] = {"state": "available"}
            save()
        for trial in range(-args.warmups, args.samples):
            order = available.copy()
            rng.shuffle(order)
            for backend in order:
                try:
                    row = run_trial(args, metadata, backend, mode, trial)
                except Exception as error:
                    caps[mode + "/" + backend].setdefault("failures", []).append(
                        {"trial": trial, "reason": str(error)}
                    )
                    save()
                    continue
                if trial >= 0:
                    rows.append(row)
                    save()
            if trial >= 0:
                print(mode, trial + 1, "/", args.samples, flush=True)
    summary = {}
    for mode in args.modes.split(","):
        for backend in args.backends.split(","):
            selected = [x for x in rows if x["mode"] == mode and x["backend"] == backend]
            if selected:
                summary[mode + "/" + backend] = {
                    "n": len(selected),
                    **{
                        k: {f"p{q}": percentile([r[k] for r in selected], q) for q in (50, 95, 99)}
                        for k in (
                            "ready_ms",
                            "result_ms",
                            "completion_ms",
                            "prepare_ms",
                            "peak_tree_rss_kib",
                        )
                    },
                }
    metadata["summary"] = summary
    save()
    print("report", args.output / "report.json", flush=True)
    if any(v["state"] != "available" or v.get("failures") for v in caps.values()):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
