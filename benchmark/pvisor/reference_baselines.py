#!/usr/bin/env python3
"""Measure local familiar runtimes with one prepared complete Agent environment.

Benchmark: B-STARTUP (benchmark/README.md#b-startup) in ready mode;
B-FS-TOOLS (benchmark/README.md#b-fs-tools) in filesystem mode;
B-AGENT-TASK (benchmark/README.md#b-agent-task) in env/tools/CLI modes.
Role: user-facing. One invocation serves one registered benchmark ID.
Motivation: users choosing a mode need to know how much slower everyday
tools become compared with native, Docker and lightweight VMs.
Conclusion sought: per-operation and whole-task wait for pVisor staged/VM
against native, Docker, Firecracker and QEMU, and which tasks suit each mode.
Design: one offline tool environment for every runtime, seven fixed workloads,
matched two-core budget, fresh workspace per job, warmups plus >=30 samples;
fresh task caches under executor TMPDIR by default, workspace cache control
separate; output, isolation and untouched-host checks gate every counted sample.
Startup question: how does kernel source affect first correct output waiting
under the same prepared userspace and budget? fc-system is official stock primary;
fc-reference is independent custom supplemental; firecracker is legacy reference/
unknown. Optional FC ready-only requires unique Ready/Result/Exit0, no panic,
controlled SIGTERM and no completion metric; normal success-exit is unchanged.
QEMU can consume the same stock receipt's original vmlinuz and optional initrd;
stock QEMU/FC results retain exact kernel provenance separately from legacy assets.
"""

import argparse
from contextlib import nullcontext
import hashlib
import json
import os
import random
import re
import resource
import shutil
import signal
import subprocess
import threading
import time
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

from firecracker_kernels import verify_kernel_receipt
from bench import percentile
from resource_budget import BudgetViolation, ResourceBudget, parse_cpus
from reference_inputs import verify_reference_inputs
from v1.common import snapshot


BENCHMARK_IDS = {"ready": "B-STARTUP", "filesystem": "B-FS-TOOLS",
                 "env": "B-AGENT-TASK", "tools": "B-AGENT-TASK",
                 "claude": "B-AGENT-TASK", "codex": "B-AGENT-TASK"}


def benchmark_for_modes(modes):
    selected = modes.split(",")
    if any(mode not in BENCHMARK_IDS for mode in selected):
        raise ValueError("unknown workload")
    ids = {BENCHMARK_IDS[mode] for mode in selected}
    if len(ids) != 1:
        raise ValueError("run one benchmark ID per invocation; use separate output directories")
    return ids.pop()


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def verified_build_receipt(path, binary):
    """Bind measured bytes to their build record, not the runner's HEAD."""
    receipt = json.loads(path.read_text())
    if receipt.get("pvisor_sha256") != digest(binary):
        raise ValueError("build receipt does not match the measured pvisor binary")
    manifest = path.parent / "source-manifest.json"
    if not manifest.is_file() or digest(manifest) != receipt.get("source_manifest_sha256"):
        raise ValueError("build receipt source manifest is missing or mismatched")
    return receipt


def reference_budget(args):
    """An optional already established private parent, never a declared cap."""
    group = getattr(args, "resource_budget", None)
    if group is None:
        return None
    if not args.cpu_affinity:
        raise ValueError("a resource budget requires explicit CPU affinity")
    budget = ResourceBudget(Path(group), args.budget_memory_mib * 1024 * 1024,
                            frozenset(parse_cpus(args.cpu_affinity)),
                            cpu_placement=getattr(args, 'budget_cpu_placement', 'affinity'))
    if "docker" in args.backends.split(",") and not budget.group.name.endswith(".slice"):
        raise ValueError("Docker's systemd cgroup parent must be a private slice")
    required = [os.getpid()]
    if "docker" in args.backends.split(","):
        if not args.docker_root_pid:
            raise ValueError("a common Docker budget requires its private daemon PID")
        required.append(args.docker_root_pid)
    # This also rejects a runner/daemon launched outside the parent or with
    # unrestricted threads. Moving a live daemon is not part of this helper.
    budget.processes(required)
    return budget


def validate_python_cache(result):
    def valid(value):
        return (isinstance(value, dict)
                and set(value) == {'dont_write_bytecode', 'prefix', 'prefix_exists'}
                and value['dont_write_bytecode'] is True
                and value['prefix'] == '/__pvisor_reference_no_pyc__'
                and value['prefix_exists'] is False)
    if not valid(result.get('python_cache')):
        raise ValueError('Python payload did not enforce the common bytecode cache policy')
    for operation in result.get('filesystem', {}).values():
        if not valid(operation.get('python_cache')):
            raise ValueError('filesystem child did not enforce the common bytecode cache policy')


def validate_tool_cache(result, expected_scratch=None):
    """Require fresh task-local scratch, with the same child environment."""
    workspace = result.get('workspace')
    if not isinstance(workspace, str) or not Path(workspace).is_absolute():
        raise ValueError('missing absolute tool workspace')
    # Missing policy is accepted only for independent audits of retained
    # workspace-policy evidence. New jobs pass an explicit expected policy.
    policy = result.get('tool_scratch', 'workspace')
    if policy not in ('executor', 'workspace') or (expected_scratch is not None and result.get('tool_scratch') != expected_scratch):
        raise ValueError('tool scratch policy differs from the declared experiment')
    if policy == 'workspace':
        temporary = Path(workspace) / '_reference_tmp'
    else:
        value = result.get('tool_cache', {}).get('TMPDIR')
        if not isinstance(value, str):
            raise ValueError('missing task-local executor scratch')
        temporary = Path(value)
        if (not temporary.is_absolute() or '..' in temporary.parts or temporary.parent.name != '.data'
                or not re.fullmatch(r'pvisor-reference-[A-Za-z0-9_-]{6,}', temporary.name)):
            raise ValueError('executor scratch is not a fresh private task directory')
    expected = dict(TMPDIR=str(temporary), HOME=str(temporary / 'reference-home'),
                    CARGO_HOME=str(temporary / 'reference-cargo'),
                    NODE_COMPILE_CACHE=str(temporary / 'node-compile-cache'),
                    NODE_DISABLE_COMPILE_CACHE=None, NODE_OPTIONS=None)
    if result.get('tool_cache') != expected:
        raise ValueError('payload did not enforce task-local tool caches')
    for operation in result.get('filesystem', {}).values():
        if operation.get('tool_cache') != expected:
            raise ValueError('filesystem child did not inherit task-local tool caches')
        if operation.get('tool_scratch', 'workspace' if expected_scratch is None else None) != policy:
            raise ValueError('filesystem child scratch policy differs')
    if result.get('mode') == 'env':
        probe = result.get('versions', {}).get('node_compile_cache', {})
        directory = probe.get('directory')
        base = Path(expected['NODE_COMPILE_CACHE'])
        valid_directory = (isinstance(directory, str) and '..' not in Path(directory).parts
                           and (directory == str(base) or
                                (Path(directory).parent == base and
                                 re.fullmatch(r'v\d+\.\d+\.\d+-[A-Za-z0-9_-]+', Path(directory).name))))
        if probe.get('status') not in ('ENABLED', 'ALREADY_ENABLED') or not valid_directory:
            raise ValueError('actual Node compile cache did not use the task-local directory')
        if expected_scratch is not None and (type(probe.get('filesystem_type')) is not int or probe['filesystem_type'] <= 0):
            raise ValueError('actual Node cache storage type is unknown')


FC_BACKENDS = ('firecracker', 'fc-system', 'fc-reference')


def fc_kernel(args, backend):
    if backend == 'firecracker':
        return None
    receipt = getattr(args, backend.replace('-', '_') + '_receipt', None)
    if receipt is None:
        raise ValueError(f'{backend} requires --{backend}-receipt')
    return verify_kernel_receipt(receipt, backend)


def reference_kernel(args, backend):
    if backend in FC_BACKENDS:
        return fc_kernel(args, backend)
    if backend in ('qemu', 'qemu-microvm'):
        receipt = getattr(args, 'qemu_system_receipt', None)
        return verify_kernel_receipt(receipt, 'fc-system') if receipt else None
    return None


def fc_ready_only(args, backend, mode):
    selected = getattr(args, 'fc_ready_policy', 'normal') == 'ready-only'
    if selected and mode != 'ready':
        raise ValueError('FC ready-only policy is restricted to ready mode')
    return selected and backend in FC_BACKENDS


def validate_guest_output(output, mode):
    """VMM exit zero does not imply the guest workload or shutdown succeeded."""
    if re.search(r'kernel panic', output, re.I):
        raise ValueError("guest kernel panicked")
    lines = output.splitlines()
    markers = [line for line in lines if line.startswith('REFERENCE_EXIT')]
    if markers != ['REFERENCE_EXIT 0']:
        raise ValueError("guest workload did not complete successfully with unique Exit0")
    if [line for line in lines if line.startswith('REFERENCE_READY')] != ['REFERENCE_READY']:
        raise ValueError('guest must emit exactly one Ready marker')
    if not (lines.index('REFERENCE_READY') < next(
            (i for i, line in enumerate(lines) if line.startswith('REFERENCE_RESULT ')), -1)
            < lines.index('REFERENCE_EXIT 0')):
        raise ValueError('guest markers are out of order')
    if len([line for line in lines if line.startswith('REFERENCE_RESULT')]) != 1:
        raise ValueError('guest must emit exactly one Result marker')
    values = [
        json.loads(line.removeprefix("REFERENCE_RESULT "))
        for line in output.splitlines()
        if line.startswith("REFERENCE_RESULT ")
    ]
    if (
        len(values) != 1
        or not isinstance(values[0], dict)
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
        or not (docker_host in argv or f"--host={docker_host}" in argv or f"-H={docker_host}" in argv)
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


def validate_staged_filesystem(work, stage, expected_bytes):
    """Successful guest writes must stay in the staged view."""
    if (work / "_fs/written").exists():
        raise ValueError("staged filesystem writes reached the lower workspace")
    validate_written_files(stage / "upper/_fs/written", expected_bytes)


def validate_direct_filesystem(work, expected_bytes):
    """A direct-write control must actually publish all writes to its workspace."""
    validate_written_files(work / "_fs/written", expected_bytes)


def validate_written_files(written, expected_bytes):
    """Require the registered 256 x 64 KiB payload, not just plausible sizes."""
    if type(expected_bytes) is not int or expected_bytes != 256 * 64 * 1024:
        raise ValueError("invalid workload written byte count")
    if written.is_symlink() or not written.is_dir():
        raise ValueError("written files must be in a real directory")
    if {p.name for p in written.iterdir()} != {f"{i:04d}" for i in range(256)}:
        raise ValueError("expected all 256 written files")
    pattern = b"pvisor-workload\n"
    chunk = (pattern * (64 * 1024 // len(pattern) + 1))[:64 * 1024]
    for p in written.iterdir():
        if p.is_symlink() or not p.is_file() or p.read_bytes() != chunk:
            raise ValueError("written file contents differ from the registered workload")


def validate_bundle_execution(bundle, backend, staged_isolation="host_process", host_isolation="host_process"):
    assert bundle["run"]["state"] == "completed" and bundle["run"]["exit_code"] == 0
    expected = (
        "virtual_machine"
        if backend == "pvisor-vm"
        else staged_isolation
        if backend == "pvisor-staged"
        else host_isolation
        if backend in ("pvisor-host", "pvisor-fuse")
        else "host_process"
    )
    assert bundle["run"]["executor"]["isolation"] == expected
    if backend in ("pvisor-vm", "pvisor-staged"):
        assert bundle["safety"]["filesystem_changes_staged"]
    if backend in ("pvisor-host", "pvisor-fuse"):
        assert not bundle["safety"]["filesystem_changes_staged"]
    if backend == "pvisor-staged" and staged_isolation == "rootless_process":
        assert bundle["safety"]["filesystem_non_bypassable"]
        assert bundle["safety"]["filesystem_read_non_bypassable"]
        assert bundle["safety"]["filesystem_write_non_bypassable"]


def validate_passthrough_output(output):
    """Reject a control that bypassed the mount or failed to serve real data."""
    mounts = [line.removeprefix("PASSTHROUGH_MOUNT ") for line in output.splitlines()
              if line.startswith("PASSTHROUGH_MOUNT ")]
    stats = [json.loads(line.removeprefix("PASSTHROUGH_STATS "))
             for line in output.splitlines() if line.startswith("PASSTHROUGH_STATS ")]
    if len(mounts) != 1 or " - fuse" not in mounts[0] or len(stats) != 1:
        raise ValueError("passthrough control lacks unique FUSE mount/request evidence")
    value = stats[0]
    if (value.get("lookup", 0) <= 0 or value.get("read", 0) <= 0
            or value.get("write", 0) <= 0 or value.get("read_bytes", 0) < 64 * 1024 * 1024
            or value.get("write_bytes", 0) < 256 * 64 * 1024):
        raise ValueError("passthrough workload did not read/write the full fixture through FUSE")
    return value


def run_trial(args, metadata, backend, mode, trial):
    diagnostic_stderr = getattr(args, 'diagnostic_stderr_file', False)
    if diagnostic_stderr and not getattr(args, 'diagnostic_timing', False):
        raise ValueError('regular-file stderr capture is diagnostic only')
    ready_only = fc_ready_only(args, backend, mode)
    root = args.output / "trials" / f"{mode}-{backend}-{trial:03d}"
    root.mkdir(parents=True)
    kernel_identity = reference_kernel(args, backend)
    work = root / "workspace"
    prep = time.perf_counter_ns()
    subprocess.run(
        ["cp", "--reflink=auto", "-a", str(args.assets / "rootfs/work"), str(work)], check=True
    )
    stage = root / "stage"
    if (work / '_reference_tmp').exists() or (work / '_reference_tmp').is_symlink():
        raise ValueError('prepared fixture contains task-local tool cache')
    budget = reference_budget(args) if getattr(args, "resource_budget", None) else None
    env = {
        k: v
        for k, v in os.environ.items()
        if k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
    }
    env.update(
        PVISOR_RUN_HOME=str(root / "runs"),
        XDG_CONFIG_HOME=str(root / "config"),
        PVISOR_STARTUP_TIMING="1" if getattr(args, "diagnostic_timing", False) else "0",
        GIT_CONFIG_COUNT="1",
        GIT_CONFIG_KEY_0="safe.directory",
        GIT_CONFIG_VALUE_0="*",
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONPYCACHEPREFIX="/__pvisor_reference_no_pyc__",
        PVISOR_REFERENCE_TOOL_SCRATCH=getattr(args, 'tool_scratch', 'executor'),
    )
    env.pop('PVISOR_REFERENCE_CACHE_DIRECTORY', None)
    env.pop("PVISOR_TEST_ALLOW_NO_USERNS", None)
    image = metadata["assets"]["docker_image"]
    rootfs = args.assets / "rootfs"
    for prefix in (Path('/__pvisor_reference_no_pyc__'), rootfs / '__pvisor_reference_no_pyc__'):
        if prefix.exists() or prefix.is_symlink():
            raise ValueError('reference bytecode cache prefix must be absent before launch')
    isvm = backend in (*FC_BACKENDS, "qemu", "qemu-microvm")
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
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "PYTHONPYCACHEPREFIX=/__pvisor_reference_no_pyc__",
            "--env",
            "PVISOR_REFERENCE_TOOL_SCRATCH=" + env['PVISOR_REFERENCE_TOOL_SCRATCH'],
            "--workdir",
            "/work",
            "--mount",
            f"type=bind,source={work},target=/work",
            "--entrypoint",
            payload[0],
            image,
            *payload[1:],
        ]
        if budget is not None:
            # systemd driver resolves this explicit slice beneath the user's
            # manager; actual payload/shim membership still needs observation.
            argv[5:5] = ["--cgroup-parent", budget.group.name]
    elif backend.startswith("sdk-vm-"):
        launch = root / "guest.json"
        launch.write_text(json.dumps({
            "argv": ["/usr/bin/python3", "/bench/reference_workload.py", "--mode", mode],
            "env": {"PATH": "/opt/toolchain/bin:/usr/local/bin:/usr/bin:/bin", "HOME": "/root",
                    "PYTHONDONTWRITEBYTECODE": "1", "PYTHONPYCACHEPREFIX": "/__pvisor_reference_no_pyc__",
                    "PVISOR_REFERENCE_TOOL_SCRATCH": env['PVISOR_REFERENCE_TOOL_SCRATCH']},
            "cwd": "/work", "workspace": "/work", "stdio_ports": [True, True, True],
        }))
        argv = [str(args.sdk_driver), backend.removeprefix("sdk-vm-"),
                str(args.sdk_rootfs), str(work), str(stage), str(launch)]
        env["LD_LIBRARY_PATH"] = str(args.firmware)
    elif backend.startswith("pvisor"):
        argv = [
            str(args.output / "bin/pvisor"),
            "run",
            "--no-agent-defaults",
            "--pass-env",
            "PYTHONDONTWRITEBYTECODE",
            "--pass-env",
            "PYTHONPYCACHEPREFIX",
            "--pass-env",
            "PVISOR_REFERENCE_TOOL_SCRATCH",
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
        if backend in ("pvisor-staged", "pvisor-vm") and getattr(args, "stage_durability", None):
            argv += ["--stage-durability", args.stage_durability]
        if backend in ("pvisor-host", "pvisor-staged", "pvisor-fuse") and mode != "ready":
            payload = [
                "/usr/bin/python3",
                str(rootfs / "bench/reference_workload.py"),
                "--mode",
                mode,
            ]
        argv += ["--", *payload]
        if backend == "pvisor-fuse":
            argv = [str(args.fuse_driver), str(work), str(root / "fuse-view"),
                    str(getattr(args, "fuse_ttl_seconds", 1)), "--", *argv]
    elif isvm:
        disk = root / "rootfs.ext4"
        subprocess.run(
            ["cp", "--reflink=auto", str(args.assets / "agent-env.ext4"), str(disk)], check=True
        )
        boot = f"console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/bench/init quiet pvbench.mode={mode} pvbench.scratch={getattr(args, 'tool_scratch', 'executor')}"
        mem = 128 if mode == "ready" else args.memory_mib
        if backend in FC_BACKENDS:
            boot = boot.replace(" pci=off", "")
            config = {
                "boot-source": {
                    "kernel_image_path": kernel_identity['paths']['kernel'] if kernel_identity else str(args.assets / "vmlinux"),
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
            if kernel_identity and 'initrd' in kernel_identity['paths']:
                config['boot-source']['initrd_path'] = kernel_identity['paths']['initrd']
            config['logger'] = {'log_path': str(root / 'firecracker.log'), 'level': 'Warning',
                                'show_level': True, 'show_log_origin': True}
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
                kernel_identity['paths']['vmlinuz'] if kernel_identity else str(args.assets / "bzImage"),
                "-append",
                boot,
                "-drive",
                f"file={disk},format=raw,if=none,id=root",
                "-device",
                "virtio-blk-device,drive=root"
                if backend == "qemu-microvm"
                else "virtio-blk-pci,drive=root",
            ]
            if kernel_identity and 'initrd' in kernel_identity['paths']:
                argv += ['-initrd', kernel_identity['paths']['initrd']]
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
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN) if getattr(args, "diagnostic_timing", False) else None
    budget_before = budget.read() if budget else None
    budget_parent = budget.processes([os.getpid()]) if budget else None
    budget_observations = []
    budget_observed_scopes = set()
    observation_mode = getattr(args, 'resource_observation', 'sampled')
    if observation_mode not in ('off', 'sampled'):
        raise ValueError('unknown timed resource observation mode')
    budget_unknown = ([] if observation_mode == 'sampled' else [dict(
        offset_ms=None, type='NotObserved',
        reason='periodic process/thread observation disabled for timing; short lifetimes not proven')])
    budget_violations = []
    print(f"Launch {mode}/{backend}, trial {trial}: {json.dumps(argv)}", flush=True)
    start = time.perf_counter_ns()
    # Profile records can exceed a nonblocking pipe's capacity. Keep diagnostic
    # stderr bytes in a regular file; formal timing retains its original pipe.
    with ((root / 'stderr.log').open('wb') if diagnostic_stderr
          else nullcontext(subprocess.PIPE)) as stderr_target:
        proc = subprocess.Popen(
            argv,
            cwd=work,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=stderr_target,
            start_new_session=True,
        )

    controlled_termination = []

    def stdout():
        for line in proc.stdout:
            out.append(line)
            if line.strip() == b"REFERENCE_READY" or line.startswith(b"REFERENCE_ENV_READY "):
                ready.append(time.perf_counter_ns())
            if line.startswith(b"REFERENCE_RESULT "):
                result_times.append(time.perf_counter_ns())
            if ready_only and line.rstrip(b'\r\n') == b'REFERENCE_EXIT 0':
                try:
                    validate_guest_output(b''.join(out).decode(errors='replace'), mode)
                except (ValueError, TypeError):
                    continue
                if proc.poll() is None:
                    try:
                        os.killpg(proc.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    else:
                        controlled_termination.append(time.perf_counter_ns())

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
            rss, _, tree_pids = snapshot(roots, include_pids=True)
            peak[0] = max(peak[0], rss)
            if budget is not None:
                try:
                    # Check every observed thread each time: an existing PID
                    # may create a thread or change affinity. Retain distinct
                    # scopes rather than duplicating identical live snapshots.
                    # Missed lifetimes remain unknown; detached siblings count.
                    observation = budget.witness_all_members(sorted(roots | tree_pids))
                    scope = tuple((w['pid'], w['start_ticks'], w['cgroup'],
                                   tuple((t['tid'], t['start_ticks'], t['cgroup'], tuple(t['cpus']))
                                         for t in w.get('threads', [])))
                                  for w in observation['witnesses'])
                    if scope not in budget_observed_scopes:
                        budget_observations.append(observation)
                        budget_observed_scopes.add(scope)
                except BudgetViolation as error:
                    budget_violations.append({"offset_ms": (time.perf_counter_ns() - start) / 1e6,
                                              "reason": str(error), "evidence": error.evidence})
                except (OSError, ValueError) as error:
                    budget_unknown.append({"offset_ms": (time.perf_counter_ns() - start) / 1e6,
                                           "reason": str(error), "type": type(error).__name__})
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
    ]
    if not diagnostic_stderr:
        threads.append(threading.Thread(target=lambda: err.append(proc.stderr.read())))
    if observation_mode == 'sampled':
        threads.append(threading.Thread(target=monitor))
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
        if proc.stderr is not None:
            proc.stderr.close()
    ended = exit_ns[0]
    usage_after = resource.getrusage(resource.RUSAGE_CHILDREN) if usage_before else None
    output = b"".join(out).decode(errors="replace")
    error = ((root / 'stderr.log').read_bytes() if diagnostic_stderr
             else b"".join(err)).decode(errors="replace")
    (root / "stdout.log").write_text(output)
    if not diagnostic_stderr:
        (root / "stderr.log").write_text(error)
    (root / "command.json").write_text(
        json.dumps({"argv": argv, "exit": proc.returncode, "prepare_ms": prep_ms,
                    "stderr_capture": 'regular-file diagnostic' if diagnostic_stderr else 'pipe',
                    "fc_ready_policy": 'ready-only' if ready_only else 'normal',
                    "controlled_sigterm": bool(controlled_termination)}, indent=2)
    )
    budget_after = budget.read() if budget is not None else None
    if kernel_identity is not None and reference_kernel(args, backend) != kernel_identity:
        raise ValueError('reference kernel provenance changed during trial')
    budget_record = None
    if budget is not None:
        budget_record = dict(
            before=budget_before, after=budget_after,
            launch_parent=budget_parent, live_observations=budget_observations,
            unknown_observations=budget_unknown,
            violations=budget_violations,
            observation_mode=observation_mode,
            cpu_usec=budget_after["cpu_stat"]["usage_usec"] - budget_before["cpu_stat"]["usage_usec"],
            memory_events_delta={k: v - budget_before["memory_events"].get(k, 0)
                                 for k, v in budget_after["memory_events"].items()},
            scope="whole private parent accounting; live membership is sampled, not proof of every short lifetime",
        )
        (root / "resource-budget.json").write_text(json.dumps(budget_record, indent=2) + "\n")
        if budget_violations:
            raise RuntimeError(f"resource-budget violation during {mode}/{backend}: {root}")
        if any(budget_record["memory_events_delta"].get(k, 0) > 0
               for k in ("oom", "oom_kill", "oom_group_kill")):
            raise RuntimeError(f"resource-budget OOM during {mode}/{backend}: {root}")
    successful_exit = proc.returncode == 0
    if ready_only:
        successful_exit = proc.returncode == 0 or (
            bool(controlled_termination) and proc.returncode == -signal.SIGTERM)
    if not successful_exit or len(ready) != 1 or len(result_times) != 1:
        raise RuntimeError(
            f"{mode}/{backend}: exit {proc.returncode}, ready={len(ready)}, result={len(result_times)}; {root}\n{error[-1000:]}\n{output[-1500:]}"
        )
    if isvm:
        validate_guest_output(output + '\n' + error, mode)
    result = json.loads(
        next(
            line.removeprefix("REFERENCE_RESULT ")
            for line in output.splitlines()
            if line.startswith("REFERENCE_RESULT ")
        )
    )
    assert result["correctness"] == "passed" and result["mode"] == mode
    if mode != "ready":
        validate_python_cache(result)
        validate_tool_cache(result, getattr(args, 'tool_scratch', 'executor'))
    fuse_stats = None
    if backend == "pvisor-fuse":
        fuse_stats = validate_passthrough_output(output)
    if backend.startswith("sdk-vm-"):
        # The built-in pvisor guest reports workload status by root ioctl;
        # successful VMM exit above and the unique result are both required.
        if "Kernel panic" in output:
            raise ValueError("SDK guest kernel panicked")
        expected_bytes = result["filesystem"]["write"]["check"]["bytes"]
        if backend == "sdk-vm-overlay":
            validate_staged_filesystem(work, stage, expected_bytes)
        else:
            validate_direct_filesystem(work, expected_bytes)
        row_stage = "overlay" if backend == "sdk-vm-overlay" else "passthrough"
    else:
        row_stage = None
    if backend.startswith("pvisor"):
        bundles = list((root / "runs").glob("*/run-bundle.json")) + list(
            stage.glob("run-bundle.json")
        )
        assert len(bundles) == 1
        bundle = json.loads(bundles[0].read_text())
        validate_bundle_execution(
            bundle, backend, getattr(args, "staged_isolation", "host_process"),
            getattr(args, "host_isolation", "host_process"),
        )
        if backend in ("pvisor-vm", "pvisor-staged"):
            if mode == "filesystem":
                validate_staged_filesystem(
                    work, stage, result["filesystem"]["write"]["check"]["bytes"]
                )
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
        "completion_ms": None if ready_only else (ended - start) / 1e6,
        "peak_tree_rss_kib": peak[0] if observation_mode == 'sampled' else None,
        "resource_observation": observation_mode,
        "memory_scope": "not sampled; RSS unknown; cgroup before/after accounting is separate"
        if observation_mode == 'off'
        else "CLI + private daemon + exact container shim/descendants; sampled RSS proxy may miss short/unreadable processes and double-count shared pages"
        if backend == "docker" and args.docker_root_pid
        else "owned launcher tree; sampled RSS proxy may miss short/unreadable processes and double-count shared pages; Docker daemon/container RSS excluded",
        "result": result,
        "correctness": "passed",
        "logs": str(root),
        "stderr_capture": 'regular-file diagnostic' if diagnostic_stderr else 'pipe',
    }
    if backend in FC_BACKENDS:
        row['fc_ready_policy'] = 'ready-only' if ready_only else 'normal'
        row['controlled_sigterm'] = bool(controlled_termination)
        row['kernel_variant'] = backend if kernel_identity else 'legacy-reference/unknown'
        row['kernel_provenance'] = kernel_identity
    elif backend in ('qemu', 'qemu-microvm'):
        row['kernel_variant'] = 'qemu-system' if kernel_identity else 'legacy-reference/unknown'
        row['kernel_provenance'] = kernel_identity
    if usage_before is not None:
        row["waited_child_resources"] = {
            "scope": "launched process and waited descendants; excludes fixture preparation, parent collector and persistent external daemons",
            **{field: getattr(usage_after, field) - getattr(usage_before, field) for field in
               ("ru_utime", "ru_stime", "ru_minflt", "ru_majflt", "ru_inblock", "ru_oublock", "ru_nvcsw", "ru_nivcsw")},
        }
    if budget_record is not None:
        row["resource_budget"] = budget_record
    if mode == "filesystem" and backend in ("native", "pvisor-host", "pvisor-fuse", "docker"):
        validate_direct_filesystem(work, result["filesystem"]["write"]["check"]["bytes"])
    if fuse_stats is not None:
        row["fuse_requests"] = fuse_stats
    if row_stage:
        row["workspace_transport"] = row_stage
    for name in ("_model-requests.json", "_cli-output.json"):
        for directory in (work, stage / "upper"):
            if (directory / name).exists():
                shutil.copy2(directory / name, root / name)
    # Retain actual output bytes for independent publication checks, including
    # reference VM disks. Reflink/sparse allocation is not a retention guarantee;
    # available storage must be checked before a complete cohort starts.
    row["retained_artifacts"] = [str(p) for p in
        (work, root / "rootfs.ext4", stage / "upper") if p.exists()]
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--assets", type=Path, required=True)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--build-receipt", type=Path, help="Verified binary/source build record; runner HEAD is not binary provenance")
    p.add_argument("--firmware", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--docker-host", default="unix:///tmp/pv-docker-v2/docker.sock")
    p.add_argument(
        "--backends",
        default=None,
                help='Ready defaults to fc-system (stock primary); other workloads keep legacy firecracker. fc-reference is independent custom supplemental; firecracker is legacy reference/unknown',
    )
    p.add_argument('--fc-system-receipt', type=Path, help='Frozen official stock kernel receipt from firecracker_kernels.py')
    p.add_argument('--fc-reference-receipt', type=Path, help='Frozen independent custom kernel receipt from firecracker_kernels.py')
    p.add_argument('--qemu-system-receipt', type=Path, help='Stock receipt shared with FC; QEMU boots its original vmlinuz and optional initrd')
    p.add_argument('--fc-ready-policy', choices=('normal', 'ready-only'), default='normal',
                   help='FC only: normal requires successful process exit; ready-only sends SIGTERM after valid Ready/Result/Exit0 and reports no completion')
    p.add_argument("--modes", default="filesystem")
    p.add_argument("--samples", type=int, default=30)
    p.add_argument("--seed", type=int, default=20261005)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument('--resource-observation', choices=('off', 'sampled'), default='off',
                   help='Periodic PID/thread/RSS scanning changes timing; use sampled only in separate capability/resource probes')
    p.add_argument("--memory-mib", type=int, default=16384)
    p.add_argument('--tool-scratch', choices=('executor', 'workspace'), default='executor',
                   help='Fresh tool caches under executor TMPDIR (default), or a separate workspace-storage control')
    p.add_argument(
        "--staged-isolation", choices=("host_process", "rootless_process"), default="host_process"
    )
    p.add_argument("--host-isolation", choices=("host_process", "rootless_process"), default="rootless_process")
    p.add_argument("--docker-root-pid", type=int)
    p.add_argument("--resource-budget", type=Path,
                   help="Already established private cgroup parent; actual constraints are verified")
    p.add_argument("--budget-memory-mib", type=int, default=16384,
                   help="Whole-parent memory cap, distinct from configured guest RAM")
    p.add_argument('--budget-cpu-placement', choices=('affinity', 'cpuset'), default='affinity',
                   help='Require actual delegated cpuset for complete CPU placement; affinity observations alone are diagnostic')
    p.add_argument(
        "--cpu-affinity", default="0,1", help="Common host CPU affinity; empty string disables it"
    )
    args = p.parse_args()
    if args.backends is None:
        fc_default = 'fc-system' if args.modes == 'ready' else 'firecracker'
        args.backends = f'native,pvisor-host,pvisor-staged,pvisor-vm,docker,{fc_default},qemu,qemu-microvm'
    # This entry publishes uninstrumented user measurements. Diagnostic runners
    # call run_trial directly and retain their own explicit profiling settings.
    os.environ["PVISOR_FS_PROFILE"] = "0"
    os.environ["PVISOR_STARTUP_TIMING"] = "0"
    try:
        benchmark_id = benchmark_for_modes(args.modes)
    except ValueError as error:
        p.error(str(error))
    if args.fc_ready_policy == 'ready-only' and args.modes != 'ready':
        p.error('--fc-ready-policy ready-only requires --modes ready')
    try:
        kernel_variants = {backend: reference_kernel(args, backend) for backend in args.backends.split(',')
                           if backend in (*FC_BACKENDS, 'qemu', 'qemu-microvm')}
    except (OSError, ValueError, KeyError) as error:
        p.error(str(error))
    if args.samples < 1 or args.warmups < 0:
        p.error("samples must be positive and warmups nonnegative")
    if args.cpu_affinity and "docker" in args.backends.split(",") and not args.docker_root_pid:
        p.error(
            "CPU-controlled Docker measurement requires --docker-root-pid for the private daemon"
        )
    if args.cpu_affinity and args.docker_root_pid:
        pin_private_docker_tree(args.docker_root_pid, args.docker_host, args.cpu_affinity)
    budget = reference_budget(args)
    for key in ("assets", "binary", "output"):
        setattr(args, key, getattr(args, key).resolve())
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "bin").mkdir()
    shutil.copy2(args.binary, args.output / "bin/pvisor")
    build_receipt = verified_build_receipt(args.build_receipt.resolve(), args.output / "bin/pvisor") if args.build_receipt else None
    if build_receipt:
        shutil.copy2(args.build_receipt, args.output / "build-receipt.json")
        shutil.copy2(args.build_receipt.parent / "source-manifest.json", args.output / "source-manifest.json")
    shutil.copytree(
        Path(__file__).parent,
        args.output / "harness",
        ignore=shutil.ignore_patterns("__pycache__", ".pytest_cache", ".data"),
    )
    print("Verify the complete prepared input inventory before preflight/warmups", flush=True)
    try:
        input_verification = verify_reference_inputs(args.assets)
    except (OSError, ValueError) as error:
        (args.output / "input-verification.json").write_text(json.dumps({
            "state": "failed", "error_type": type(error).__name__, "reason": str(error),
            "assets": str(args.assets), "timing_samples": 0,
        }, indent=2) + "\n")
        raise
    (args.output / "input-verification.json").write_text(json.dumps({
        "state": "passed", **input_verification,
    }, indent=2) + "\n")
    metadata = {
        "schema": "pvisor-reference-environment/v1",
        "benchmark_id": benchmark_id,
        "benchmark_ids": {mode: BENCHMARK_IDS[mode] for mode in args.modes.split(",")},
        "recorded_at": datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(),
        "arguments": {k: str(v) for k, v in vars(args).items()},
        "assets": json.loads((args.assets / "assets.json").read_text()),
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "source_commit_scope": "runner worktree HEAD; measured binary source is recorded separately",
        "binary_build": build_receipt,
        "binary_source_commit": build_receipt["source_identity"]["head"] if build_receipt else "unknown",
        "binary_source_manifest_sha256": build_receipt["source_manifest_sha256"] if build_receipt else "unknown",
        "assets_metadata_sha256": digest(args.assets / "assets.json"),
        "input_manifest_sha256": digest(args.assets / "input-manifest.json") if (args.assets / "input-manifest.json").is_file() else "unknown",
        "input_verification": input_verification,
        "firecracker_kernels": {k: v for k, v in kernel_variants.items() if k in FC_BACKENDS},
        "reference_kernels": kernel_variants,
        "legacy_firecracker_label": 'legacy-reference/unknown',
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
            "python_cache": "missing /__pvisor_reference_no_pyc__ prefix, no bytecode writes; actual parent/child flags required",
            "tool_cache": f"fresh task-local {args.tool_scratch} scratch; Node compile cache enabled; no cross-task Node/HOME/Cargo cache",
            "resource_observation": args.resource_observation,
            "image_preparation": "excluded from timed job; measured separately",
            "vm_shape": f"2 vCPU; shell ready 128 MiB; complete environment {args.memory_mib} MiB configured RAM",
            "agent_model": "same-guest deterministic fixture; no real inference",
            "codex_sandbox": "danger-full-access uniformly; fixed commands, outer runtime boundary",
            "resource_scope": f"All launch trees bound to host CPUs {args.cpu_affinity or 'unrestricted'}; Docker private daemon pinned separately; VMs 2 vCPU; Rust -j2; RSS sums may double-count shared pages",
            "order": "seeded random backend per round",
            "percentile": "linear interpolation; public P95 is descriptive, no public P99 below 100 samples",
            "exclusion": "reject failed output/isolation checks; no post-hoc timing exclusions",
            "seed": args.seed,
        },
    }
    for tool in ("docker", "firecracker", "qemu-system-x86_64"):
        metadata[tool + "_version"] = subprocess.run(
            [tool, "--version"], capture_output=True, text=True
        ).stdout.strip()
    metadata["host_cpu_model"] = next(
        (line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
         if line.startswith("model name")), "unknown")
    if "docker" in args.backends.split(","):
        driver = subprocess.run(
            ["docker", "--host", args.docker_host, "info", "--format", "{{.Driver}}"],
            capture_output=True, text=True, timeout=30)
        metadata["docker_storage_driver"] = driver.stdout.strip() if driver.returncode == 0 else "unavailable"
        if budget is not None:
            info = subprocess.run(
                ["docker", "--host", args.docker_host, "info", "--format", "{{json .}}"],
                capture_output=True, text=True, timeout=30, check=True)
            details = json.loads(info.stdout)
            if (details.get("CgroupDriver") != "systemd" or str(details.get("CgroupVersion")) != "2"
                    or "name=rootless" not in details.get("SecurityOptions", [])):
                raise ValueError("budgeted Docker requires verified rootless/systemd/cgroupv2")
            metadata["docker_cgroup_configuration"] = {
                k: details[k] for k in ("CgroupDriver", "CgroupVersion", "SecurityOptions")}
    if budget is not None:
        metadata["resource_budget"] = budget.read()
        metadata["protocol"]["resource_scope"] = (
            "One verified private cgroup parent: all descendants share CPU quota2, "
            f"memory cap{args.budget_memory_mib}MiB, zero swap; declared CPU IDs{args.cpu_affinity}. "
            "Parent threads and sampled subtree members are checked; complete short-lifetime scope proof pending. "
            "Cgroup peak is lifecycle-wide; precharged shared cache outside the parent is excluded.")
    rows = []
    caps = {}
    rng = random.Random(args.seed)

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
                    row["benchmark_id"] = metadata["benchmark_ids"][mode]
                    rows.append(row)
                    save()
            if trial >= 0:
                print(mode, trial + 1, "/", args.samples, flush=True)
    print("Verify prepared inputs again after all timed conditions", flush=True)
    try:
        final_inputs = verify_reference_inputs(args.assets)
        final_kernels = {backend: reference_kernel(args, backend) for backend in kernel_variants}
        if final_kernels != kernel_variants:
            raise ValueError('reference kernel provenance changed during the cohort')
        if final_inputs != input_verification:
            raise ValueError("prepared input identities changed during the cohort")
    except (OSError, ValueError) as error:
        metadata["input_final_verification"] = dict(state="failed", error_type=type(error).__name__, reason=str(error))
        caps["prepared-inputs"] = dict(state="failed", reason=str(error))
    else:
        metadata["input_final_verification"] = dict(state="passed", **final_inputs)
    (args.output / "input-final-verification.json").write_text(
        json.dumps(metadata["input_final_verification"], indent=2) + "\n")
    save()
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
                        ) if all(r[k] is not None for r in selected)
                    },
                }
    metadata["summary"] = summary
    save()
    print("report", args.output / "report.json", flush=True)
    if any(v["state"] != "available" or v.get("failures") for v in caps.values()):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
