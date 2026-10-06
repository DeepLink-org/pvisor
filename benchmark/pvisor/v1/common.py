"""Shared evidence, process and validation helpers for product benchmarks."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import platform
import shutil
import subprocess
import threading
import tempfile
import time
from pathlib import Path
from zoneinfo import ZoneInfo

def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def checked(argv, **kwargs):
    result = subprocess.run(
        argv, capture_output=True, text=True, timeout=kwargs.pop("timeout", 120), **kwargs
    )
    if result.returncode:
        raise RuntimeError(f"{argv!r}: exit {result.returncode}\n{result.stderr[-4000:]}")
    return result


def retain_command(directory, stdout, stderr, details):
    directory = Path(directory)
    history = directory / "commands"
    history.mkdir(exist_ok=True)
    command = Path(tempfile.mkdtemp(prefix="command-", dir=history))
    payloads = {"command.stdout": stdout, "command.stderr": stderr,
                "command.json": (json.dumps(details) + "\n").encode()}
    for name, content in payloads.items():
        (command / name).write_bytes(content)
        # Retain the previous reader contract while keeping every invocation.
        (directory / name).write_bytes(content)


def snapshot(roots, include_pids=False):
    """RSS proxy; optional PID discovery also retains stat-only processes.

    RSS may miss short/unreadable processes or double-count shared pages.
    A missing RSS does not remove a process from the requested scope check.
    """
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        try:
            fields = (path / "stat").read_text().rsplit(") ", 1)[1].split()
            try:
                rss = next(
                    int(line.split()[1])
                    for line in (path / "status").read_text().splitlines()
                    if line.startswith("VmRSS:")
                )
            except (OSError, StopIteration):
                if not include_pids:
                    continue
                rss = 0  # only the RSS proxy omits it; PID coverage retains it
            processes[int(path.name)] = (int(fields[1]), rss)
        except (OSError, ValueError, StopIteration):
            continue
    owned = set(roots)
    while True:
        additions = {pid for pid, (parent, _) in processes.items() if parent in owned} - owned
        if not additions:
            break
        owned.update(additions)
    members = owned & processes.keys()
    result = (sum(processes[pid][1] for pid in members), len(members))
    return (*result, members) if include_pids else result


class Context:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=False, mode=0o700)
        self.repo = Path(__file__).resolve().parents[3]
        (self.output / "bin").mkdir()
        self.binary = self.output / "bin/pvisor"
        shutil.copy2(args.binary.resolve(), self.binary)
        build_receipt = None
        if getattr(args, "build_receipt", None):
            from reference_baselines import verified_build_receipt
            build_receipt = verified_build_receipt(args.build_receipt.resolve(), self.binary)
            shutil.copy2(args.build_receipt, self.output / "build-receipt.json")
            shutil.copy2(args.build_receipt.resolve().parent / "source-manifest.json", self.output / "source-manifest.json")
        self.firmware = self.output / "firmware"
        shutil.copytree(args.firmware.resolve(), self.firmware, symlinks=False)
        self.toolchain = Path(checked(["rustc", "--print", "sysroot"]).stdout.strip())
        self.image = None
        self.rootfs = None
        self.counter = 0
        self.counter_lock = threading.Lock()
        self.rows = []
        self.capabilities = {}
        shutil.copytree(
            self.repo / "benchmark/pvisor",
            self.output / "harness",
            ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache", ".data"),
        )
        self.env = os.environ.copy()
        for key in (
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "no_proxy",
        ):
            self.env.pop(key, None)
        self.env["PVISOR_STARTUP_TIMING"] = "0"
        self.env["PVISOR_FS_PROFILE"] = "0"
        self.env["GIT_CONFIG_GLOBAL"] = "/dev/null"
        self.env["GIT_CONFIG_NOSYSTEM"] = "1"
        self.env["LC_ALL"] = "C"
        self.env.pop("PVISOR_TEST_ALLOW_NO_USERNS", None)
        self.metadata = dict(
            cli_arguments={
                key: str(value) if isinstance(value, Path) else value
                for key, value in vars(args).items()
            },
            schema="pvisor-benchmark/v1",
            suite="product-v1",
            recorded_at=dt.datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
            platform=platform.platform(),
            cpu=next(
                line.split(":", 1)[1].strip()
                for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")
            ),
            memory_total_kib=next(
                int(line.split()[1])
                for line in Path("/proc/meminfo").read_text().splitlines()
                if line.startswith("MemTotal:")
            ),
            logical_cpus=os.cpu_count(),
            source_commit=checked(["git", "rev-parse", "HEAD"], cwd=self.repo).stdout.strip(),
            source_status=checked(["git", "status", "--porcelain"], cwd=self.repo).stdout,
            binary_sha256=digest(self.binary),
            binary_build=build_receipt,
            binary_source_commit=build_receipt["source_identity"]["head"] if build_receipt else "unknown",
            source_commit_scope="runner worktree HEAD; not measured binary provenance",
            source_binary=str(args.binary.resolve()),
            firmware_sha256=digest(self.firmware / "libkrunfw.so.5"),
            python=checked(["/usr/bin/python3", "--version"]).stdout.strip(),
            protocol=dict(
                samples=args.samples,
                warmups=args.warmups,
                caches="warm; no host cache eviction",
                startup_logging=False,
                vm_memory=args.vm_memory,
                correctness_required=True,
                percentile="linear interpolation",
                timing="wall includes process launch and teardown; worker_ms is workload only",
                git_configuration="global/system config disabled; isolated fixture repository config; C locale",
            ),
            harness_sha256={
                str(p.relative_to(self.output / "harness")): digest(p)
                for p in (self.output / "harness").rglob("*.py")
            },
            load_before=os.getloadavg(),
        )
        self.save()

    def save(self):
        value = self.metadata | dict(
            capabilities=self.capabilities, rows=self.rows, load_after=os.getloadavg()
        )
        temporary = self.output / "report.tmp"
        temporary.write_text(json.dumps(value, indent=2) + "\n")
        temporary.replace(self.output / "report.json")

    def fresh(self, name):
        with self.counter_lock:
            self.counter += 1
            path = self.output / "trials" / f"{self.counter:05d}-{name}"
        path.mkdir(parents=True)
        return path

    def run(self, argv, *, cwd, env=None, timeout=120, expected=0):
        is_podman = argv[0] == "podman"
        affinity = getattr(getattr(self, "args", None), "cpu_affinity", None)
        if affinity:
            argv = ["taskset", "--cpu-list", affinity, *argv]
        runenv = self.env | (env or {})
        if is_podman:
            runenv["HOME"] = os.environ["HOME"]
        started = time.perf_counter_ns()
        try:
            process = subprocess.Popen(
                argv,
                cwd=cwd,
                env=runenv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
            try:
                stdout, stderr = process.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                import signal

                os.killpg(process.pid, signal.SIGKILL)
                stdout, stderr = process.communicate()
                retain_command(Path(cwd).parent, stdout, stderr, dict(
                    argv=argv, cwd=str(cwd), exit_code=process.returncode,
                    timed_out=True, timeout_seconds=timeout))
                raise TimeoutError(f"command timed out: {argv!r}")
        except BaseException:
            if "process" in locals() and process.poll() is None:
                import signal

                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
            raise
        elapsed = (time.perf_counter_ns() - started) / 1e6
        retain_command(Path(cwd).parent, stdout, stderr, dict(
            argv=argv, cwd=str(cwd), exit_code=process.returncode,
            wall_ms=elapsed, timed_out=False))
        if process.returncode != expected:
            raise RuntimeError(
                f"exit {process.returncode}, expected {expected}; logs in {cwd}\n{stderr.decode(errors='replace')[-2000:]}"
            )
        return elapsed, stdout.decode(errors="replace"), stderr.decode(errors="replace")

    def command(self, backend, workspace, stage, payload, *, network=None):
        if backend == "native":
            return payload
        if backend == "podman":
            if self.image is None:
                raise RuntimeError("prepared OCI image unavailable")
            return [
                "podman",
                *getattr(self, "podman_options", []),
                "run",
                "--rm",
                "--network",
                "host" if network else "none",
                "--entrypoint",
                payload[0],
                "-v",
                f"{workspace}:/work:Z",
                *(["-v", f"{self.toolchain}:{self.toolchain}:ro"] if self.metadata.get("benchmark_id") == "B-AGENT-TASK" else []),
                "-w",
                "/work",
                self.image,
                *payload[1:],
            ]
        argv = [
            str(self.binary),
            "run",
            "--no-agent-defaults",
            "--stdio",
            "capture",
            "--timeout",
            "120s",
            "--overlaynet",
            "off",
        ]
        if backend in ("staged", "safe"):
            argv += ["--stage", str(stage)]
        if backend == "safe":
            argv[argv.index("--overlaynet") + 1] = "proxy"
            argv += ["--safe", "--filesystem", "sandbox", "--mount", f"{self.toolchain}:read"]
        if backend == "vm":
            argv += [
                "--vm",
                "--rootfs",
                str(self.rootfs or "/"),
                "--vm-library-dir",
                str(self.firmware),
                "--cpu",
                "2",
                "--memory",
                self.args.vm_memory,
                "--stage",
                str(stage),
            ]
        if backend == "container":
            argv += [
                "--executor",
                "container",
                "--container-rootfs",
                str(self.rootfs),
                "--container-runtime",
                "crun",
                "--container-network",
                "host" if network else "none",
                "--container-mount",
                f'source="{workspace}",target="/work",read_only=false',
                "--container-workdir",
                "/work",
            ]
            if self.metadata.get("benchmark_id") == "B-AGENT-TASK":
                argv += ["--container-mount", f'source="{self.toolchain}",target="{self.toolchain}",read_only=true']
        if network:
            off = argv.index("--overlaynet")
            argv[off + 1] = network[0]
            argv += network[1:]
        return argv + ["--", *payload]

    def validate_bundle(self, backend, runs, stage, expected_isolation=None):
        if backend in ("native", "podman"):
            return None
        files = list(Path(runs).glob("*/run-bundle.json"))
        if Path(stage, "run-bundle.json").is_file():
            files.append(Path(stage, "run-bundle.json"))
        files = list(set(files))
        if len(files) != 1:
            raise RuntimeError(f"expected one Run Bundle, found {len(files)}")
        bundle = json.loads(files[0].read_text())
        if bundle["run"]["state"] != "completed" or bundle["run"]["exit_code"] != 0:
            raise RuntimeError("completed zero-exit Run Bundle required")
        observed = bundle["run"]["executor"]["isolation"]
        expected = {
            "host": "rootless_process",
            "staged": "rootless_process",
            "safe": "rootless_process",
            "vm": "virtual_machine",
            "container": "container",
        }[backend]
        expected = expected_isolation or expected
        if observed != expected:
            raise RuntimeError(f"observed isolation {observed}, expected {expected}")
        if (
            backend in ("staged", "safe", "vm")
            and not bundle["safety"]["filesystem_changes_staged"]
        ):
            raise RuntimeError("staging not observed")
        if backend == "safe" and not bundle["safety"]["filesystem_non_bypassable"]:
            raise RuntimeError("non-bypassable filesystem required")
        if backend == "staged" and expected == "rootless_process" and not all(
            bundle["safety"].get(field, False) for field in
            ("filesystem_non_bypassable", "filesystem_read_non_bypassable", "filesystem_write_non_bypassable")
        ):
            raise RuntimeError("rootless staging requires non-bypassable read/write filesystem evidence")
        return bundle

    def record(self, row):
        if row.get("correctness") != "passed":
            raise RuntimeError("failed samples cannot enter performance summary")
        self.rows.append(row)
        with (self.output / "samples.jsonl").open("a") as f:
            f.write(json.dumps(row) + "\n")
        self.save()
