#!/usr/bin/env python3
"""B-FS-TOOLS: prepared-tool native/host/staged launch adapters for V2.

All mounts belong to a fresh user/mount/PID namespace. A private pivot root
contains the same prepared tools used by the VM, a read-only fixture, a private
temporary directory and one writable trial directory. Namespace construction
precedes USER_WORKLOADS_LAUNCH, so it is excluded from command Ready. This module
does not collect or publish measurements and never contacts an existing Host
listener. The caller owns the enclosing cgroup and supervision timeout.
"""

import argparse
import ctypes
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

NAMESPACE_BACKENDS = ("native", "host", "staged", "vm", "vm-writeback")
BACKENDS = NAMESPACE_BACKENDS + ("docker", "fc-system", "qemu", "qemu-microvm", "agentenv")
MOUNT_BIND = 4096
MOUNT_REC = 16384
MOUNT_RDONLY = 1
MOUNT_REMOUNT = 32
MOUNT_PRIVATE = 1 << 18
DETACH = 2


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def tool_paths(root):
    """Record the selected distribution's compiler helpers, without Fedora paths."""
    root = Path(root)
    helpers = sorted(
        set(root.glob("usr/libexec/gcc/*/*/cc1")) | set(root.glob("usr/lib/gcc/*/*/cc1"))
    )
    if not helpers:
        raise ValueError("prepared C compiler helper cc1 missing")
    return ["usr/bin/cc", "usr/bin/git", "usr/bin/python3"] + [
        str(p.relative_to(root)) for p in helpers
    ]


def _install(source, destination, root):
    """Copy bytes into our private tree without following destination aliases."""
    source, destination, root = Path(source), Path(destination), Path(root)
    if not destination.parent.resolve(strict=False).is_relative_to(root.resolve()):
        raise ValueError("installation escaped the private prepared root")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_symlink():
        destination.unlink()
    shutil.copy2(source.resolve(strict=True), destination)
    return dict(
        source=str(source.resolve()),
        path=str(destination.relative_to(root)),
        sha256=sha(destination),
    )


def prepare_rootfs(batch, rootfs, source_batch, artifacts):
    """Freeze one reflink tool root, returning its absolute path.

    ``source_batch`` is a completed V2 fixture/payload batch. ``artifacts`` is
    the frozen CLI build directory or its bin directory. Only preparation runs
    here; no executable workload, VM or product listener is started. A new
    destination is required, and each added runtime library is recorded.
    """
    batch, rootfs = Path(batch).resolve(), Path(rootfs).resolve(strict=True)
    source_batch = Path(source_batch).resolve(strict=True)
    artifacts = Path(artifacts).resolve(strict=True)
    binary_dir = artifacts / "bin" if (artifacts / "bin").is_dir() else artifacts
    toolroot = batch / "toolroot"
    toolroot.mkdir(mode=0o700, parents=False, exist_ok=False)
    commands = [
        ["cp", "-a", "--reflink=auto", str(rootfs) + "/.", str(toolroot)],
        ["cp", "-a", "--reflink=auto", str(source_batch / "fixture-layer") + "/.", str(toolroot)],
    ]
    for command in commands:
        subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    launch = toolroot / "bench-artifacts"
    launch.mkdir(mode=0o700)
    installed = [_install(source_batch / "bin/payload", launch / "workload", toolroot)]
    harness = batch / "harness"
    harness.mkdir(exist_ok=True)
    wrapper_source = Path(__file__).with_name("user_workloads_cwd.rs")
    frozen_wrapper = harness / wrapper_source.name
    if wrapper_source.resolve() != frozen_wrapper.resolve():
        shutil.copy2(wrapper_source, frozen_wrapper)
    wrapper_command = [
        "rustc",
        "--edition=2024",
        "-O",
        "--target",
        "x86_64-unknown-linux-musl",
        str(frozen_wrapper),
        "-o",
        str(launch / "payload"),
    ]
    subprocess.run(wrapper_command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    commands.append(wrapper_command)
    installed.append(
        dict(
            source=str(frozen_wrapper),
            path="bench-artifacts/payload",
            source_sha256=sha(frozen_wrapper),
            sha256=sha(launch / "payload"),
            role="resolve product-selected cwd, then exec frozen workload",
        )
    )
    for name in ("pvisor", "pvisor-guest"):
        if (binary_dir / name).is_file():
            installed.append(_install(binary_dir / name, launch / name, toolroot))
            (launch / name).chmod(0o700)
    if not (launch / "pvisor").is_file():
        raise ValueError("frozen CLI artifact is missing")
    firmware = source_batch / "bin/libkrunfw.so"
    installed.append(_install(firmware, launch / "libkrunfw.so.5.6.2", toolroot))
    (launch / "libkrunfw.so").symlink_to("libkrunfw.so.5.6.2")
    (launch / "libkrunfw.so.5").symlink_to("libkrunfw.so.5.6.2")
    dependencies = []
    for executable in sorted(launch.iterdir()):
        if executable.name not in ("pvisor", "pvisor-guest"):
            continue
        linked = subprocess.run(["ldd", str(executable)], text=True, capture_output=True)
        # A static companion reports "not a dynamic executable" with status 1.
        if linked.returncode and not any(
            s in linked.stdout + linked.stderr
            for s in ("not a dynamic executable", "statically linked")
        ):
            raise ValueError("cannot identify frozen CLI runtime: " + linked.stderr)
        for line in linked.stdout.splitlines():
            if "not found" in line:
                raise ValueError("frozen CLI dependency unavailable: " + line)
            match = re.search(r"(?:=>\s*)?(/\S+)\s+\(", line)
            if match:
                source = Path(match.group(1))
                record = _install(source, toolroot / source.relative_to("/"), toolroot)
                if record not in dependencies:
                    dependencies.append(record)
    for required in ("usr/bin/cc", "usr/bin/python3", "fs-v2-fixture/sentinel.bin"):
        if not (toolroot / required).is_file():
            raise ValueError("prepared tool/input is missing: " + required)
    receipt = dict(
        schema=1,
        benchmark_id="B-FS-TOOLS",
        toolroot=str(toolroot),
        prepared_rootfs=str(rootfs),
        source_batch=str(source_batch),
        cli_artifacts=str(artifacts),
        commands=commands,
        installed=installed,
        runtime_dependencies=dependencies,
        mutation_scope="new private reflink tree only; source inputs unchanged",
        status="prepared; no workloads executed",
    )
    save(batch / "toolroot-receipt.json", receipt)
    return toolroot


def _payload(case, rounds=3, ram_mib=512):
    # The product selects the staged workspace through cwd. An absolute lower
    # path would bypass that view for a host-staged Job.
    return ["/bench-artifacts/payload", case, "output", "input", str(rounds), str(ram_mib)]


def _host_argv(case, backend, rounds=3, ram_mib=512):
    payload = _payload(case, rounds, ram_mib)
    if backend == "native":
        return payload
    argv = [
        "/bench-artifacts/pvisor",
        "run",
        "--no-agent-defaults",
        "--pass-env",
        "PYTHONDONTWRITEBYTECODE",
        "--pass-env",
        "PYTHONPYCACHEPREFIX",
        "--overlaynet",
        "off",
        "--stdio",
        "inherit",
        "--timeout",
        "300s",
    ]
    if backend == "staged":
        argv += ["--stage", "/bench-trial/stage"]
    return argv + ["--", *payload]


def build_command(batch, protocol, folder, case, backend):
    """Prepare one fresh trial and return ``(argv, environment)``.

    Preparation is not a measurement. Invoke the command with a process-group
    timeout; ``unshare --kill-child`` plus PID-namespace teardown bounds detached
    product descendants. Ready starts at the emitted monotonic launch marker.
    Work, stages, Run Bundles and the namespace receipt remain in ``folder``.
    """
    if backend not in BACKENDS:
        raise ValueError("unknown user workload backend")
    if backend == "agentenv":
        from user_workloads_agentenv import build_command as reference_command

        return reference_command(batch, protocol, folder, case)
    if backend == "docker":
        from user_workloads_docker import build_command as reference_command

        return reference_command(batch, protocol, folder, case)
    if backend in ("fc-system", "qemu", "qemu-microvm"):
        from user_workloads_reference_vm import build_command as reference_command

        return reference_command(batch, protocol, folder, case, backend)
    folder = Path(folder).resolve(strict=True)
    root = folder / "namespace-root"
    root.mkdir(mode=0o700)
    for name in ("work", "stage", "runs", "config"):
        (folder / name).mkdir(mode=0o700)
    stage_storage = None
    if protocol.get("stage_storage") and backend in ("staged", "vm", "vm-writeback"):
        stage_storage = Path(protocol["stage_storage"]).resolve(strict=True) / folder.name
        stage_storage.mkdir(mode=0o700, exist_ok=False)
    for name in ("input", "output"):
        (folder / "work" / name).mkdir(mode=0o700)
    rounds = int(protocol.get("resident_rounds", 3))
    ram = int(protocol.get("guest_memory_mib", 512))
    environment = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("PVISOR_", "WORKLOADS_V2_"))
        and k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
    }
    environment.update(
        PATH="/usr/bin:/bin",
        HOME="/tmp",
        LC_ALL="C",
        GIT_CONFIG_NOSYSTEM="1",
        GIT_CONFIG_GLOBAL="/dev/null",
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONPYCACHEPREFIX="/__pvisor_reference_no_pyc__",
        PVISOR_RUN_HOME="/bench-trial/runs",
        XDG_CONFIG_HOME="/bench-trial/config",
        PVISOR_STARTUP_TIMING="0",
        PVISOR_STARTUP_PROFILE="0",
        PVISOR_FS_PROFILE="0",
        RUST_LOG="off",
    )
    if backend.startswith("vm"):
        from user_workloads_vm import vm_environment, vm_payload_argv

        vm_backend = "pvisor-vm-wb" if backend == "vm-writeback" else "pvisor-vm"
        inner = vm_payload_argv(case, vm_backend, rounds, ram)
        environment.update(vm_environment(vm_backend))
        workaround = protocol.get("krun_enomem_workaround", "auto")
        if workaround not in ("auto", "0", "1"):
            raise ValueError("invalid frozen KVM ENOMEM workaround selection")
        if workaround != "auto":
            environment["PVISOR_KRUN_ENOMEM_WORKAROUND"] = workaround
        if protocol.get("prepared_inputs"):
            selected = protocol["prepared_inputs"]
            if set(selected) != {"rootfs", "workspace"}:
                raise ValueError("both authenticated prepared input selectors are required")
            index = inner.index("--rootfs")
            del inner[index : index + 2]
            for kind in ("rootfs", "workspace"):
                selector = selected[kind]
                Path(selector["store"]).resolve(strict=True)
                if not isinstance(selector["base"]["id"], str) or not selector["base"]["id"]:
                    raise ValueError("prepared base id missing")
                index = inner.index("--")
                inner[index:index] = [
                    f"--{kind}-base-store",
                    f"/bench-prepared/{kind}",
                    f"--{kind}-base",
                    selector["base"]["id"],
                ]
            environment["PVISOR_VM_GUEST_CACHE"] = "1"
            environment["PVISOR_VM_CACHE_EXPERIMENT"] = "root-data"
        if protocol.get("immutable_input_cache", False):
            if not protocol.get("prepared_inputs"):
                raise ValueError("immutable input caches require authenticated prepared inputs")
            # Keep upper/namespace checks. The DATA+WB combination additionally
            # requires matching firmware capability; old peers fall back.
            environment["PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE"] = "1"
            environment["PVISOR_VM_FS_LOWER_DATA_CACHE"] = "1"
        if protocol.get("diagnostic_profile"):
            if protocol.get("role") != "diagnostic":
                raise ValueError("profiles require a separate diagnostic protocol")
            environment["PVISOR_FS_PROFILE"] = "1"
    else:
        inner = _host_argv(case, backend, rounds, ram)
    config = dict(
        schema=1,
        backend=backend,
        case_id=case,
        toolroot=str(Path(protocol.get("toolroot", Path(batch) / "toolroot")).resolve(strict=True)),
        folder=str(folder),
        stage_storage=str(stage_storage) if stage_storage else None,
        prepared_inputs=protocol.get("prepared_inputs") if backend.startswith("vm") else None,
        runtime_artifacts=protocol.get("runtime_artifacts") if backend.startswith("vm") else None,
        namespace_root=str(root),
        argv=inner,
        vm_environment={k: v for k, v in environment.items() if k.startswith(("PVISOR_", "KRUN_"))},
        parent_namespaces={
            name: os.readlink("/proc/self/ns/" + name) for name in ("user", "mnt", "pid")
        },
    )
    config_path = folder / "launch.json"
    save(config_path, config)
    command = [
        "unshare",
        "--user",
        "--map-root-user",
        "--mount",
        "--pid",
        "--fork",
        "--kill-child=KILL",
        "--propagation",
        "private",
        sys.executable,
        str(Path(__file__).resolve()),
        "--namespace-child",
        str(config_path),
    ]
    if protocol.get("apparmor_profile"):
        command = ["aa-exec", "-p", protocol["apparmor_profile"], "--", *command]
    if protocol.get("diagnostic_sync_trace"):
        if protocol.get("role") != "diagnostic" or not protocol.get("diagnostic_profile"):
            raise ValueError("sync tracing requires an independent diagnostic")
        command = [
            "strace",
            "-f",
            "-qq",
            "-ttt",
            "-T",
            "-y",
            "-e",
            "trace=fsync,fdatasync",
            "-o",
            str(folder / "sync.trace"),
            *command,
        ]
    return command, environment


def output_root(folder, backend):
    """Return the retained logical output source and whether it is an upper."""
    folder = Path(folder)
    stage = folder / "stage"
    if (folder / "launch.json").is_file():
        config = json.loads((folder / "launch.json").read_text())
        if config.get("stage_storage"):
            stage = Path(config["stage_storage"])
    if backend.startswith("vm"):
        return stage / "upper/output", True
    return (
        (stage / "upper/output", True) if backend == "staged" else (folder / "work/output", False)
    )


def finalize_output(batch, protocol, folder, case, backend):
    """Export reference output after exit; callers still verify all bytes."""
    if backend == "agentenv":
        from user_workloads_agentenv import finalize_trial

        return finalize_trial(batch, protocol, folder, case, backend), False
    if backend == "docker":
        from user_workloads_docker import finalize_trial

        return finalize_trial(batch, protocol, folder, case, backend), False
    if backend in ("fc-system", "qemu", "qemu-microvm"):
        from user_workloads_reference_vm import finalize_trial

        return finalize_trial(batch, protocol, folder, case, backend), False
    return output_root(folder, backend)


def cleanup_trial(batch, protocol, folder, case, backend):
    """Reap only reference resources owned by this trial, including failures."""
    if backend == "agentenv":
        from user_workloads_agentenv import cleanup_trial as cleanup

        cleanup(batch, protocol, folder, case, backend)
    elif backend == "docker":
        from user_workloads_docker import cleanup_trial as cleanup

        cleanup(batch, protocol, folder, case, backend)
    elif backend in ("fc-system", "qemu", "qemu-microvm"):
        from user_workloads_reference_vm import cleanup_trial as cleanup

        cleanup(batch, protocol, folder, case, backend)


def runtime_artifact_source(frozen):
    """Validate a private, frozen host runtime without modifying guest stores."""
    source = Path(frozen["path"]).resolve(strict=True)
    if not {"pvisor", "payload", "workload", "libkrunfw.so.5"}.issubset(frozen["sha256"]):
        raise ValueError("incomplete runtime artifact receipt")
    for name, digest in frozen["sha256"].items():
        member = (source / name).resolve(strict=True)
        if not member.is_relative_to(source) or sha(member) != digest:
            raise ValueError("runtime artifact identity mismatch: " + name)
    return source


def _namespace_child(config_path):
    config = json.loads(Path(config_path).read_text())
    identities = {name: os.readlink("/proc/self/ns/" + name) for name in ("user", "mnt", "pid")}
    if any(identities[name] == config["parent_namespaces"][name] for name in identities):
        raise RuntimeError("fresh user/mount/PID namespace was not installed")
    root, toolroot, folder = map(
        Path, (config["namespace_root"], config["toolroot"], config["folder"])
    )
    libc = ctypes.CDLL(None, use_errno=True)
    libc.mount.argtypes = [
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_ulong,
        ctypes.c_void_p,
    ]
    libc.mount.restype = ctypes.c_int
    libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]
    libc.umount2.restype = ctypes.c_int

    def mount(source, target, kind=None, flags=0, options=None):
        def encoded(value):
            return None if value is None else os.fsencode(value)

        if (
            libc.mount(encoded(source), encoded(target), encoded(kind), flags, encoded(options))
            != 0
        ):
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code), str(target))

    mount(None, "/", flags=MOUNT_PRIVATE | MOUNT_REC)
    reserved = {"tmp", "run", "proc", "dev", "sys", "bench-trial", "bench-prepared", ".old-root"}
    for path in toolroot.iterdir():
        if path.name in reserved:
            continue
        destination = root / path.name
        if path.is_symlink():
            destination.symlink_to(os.readlink(path))
        elif path.is_dir():
            destination.mkdir(mode=0o700)
        else:
            destination.touch(mode=0o600)
    for name in reserved:
        (root / name).mkdir(mode=0o700)
    # This root is entirely ours and becomes the namespace mount root.
    mount(root, root, flags=MOUNT_BIND)
    mounts = []
    for path in toolroot.iterdir():
        if path.name in reserved or path.is_symlink():
            continue
        target = root / path.name
        source = path
        if path.name == "bench-artifacts" and config.get("runtime_artifacts"):
            source = runtime_artifact_source(config["runtime_artifacts"])
        mount(source, target, flags=MOUNT_BIND)
        mount(None, target, flags=MOUNT_REMOUNT | MOUNT_BIND | MOUNT_RDONLY)
        mounts.append(dict(source=str(source), target="/" + path.name, readonly=True))
    mount(folder, root / "bench-trial", flags=MOUNT_BIND)
    if config.get("stage_storage"):
        mount(config["stage_storage"], root / "bench-trial/stage", flags=MOUNT_BIND)
    for kind, selector in (config.get("prepared_inputs") or {}).items():
        target = root / "bench-prepared" / kind
        target.mkdir(mode=0o700)
        # Store owners need writable lease/catalog access. Only the selected
        # generation is exported to the guest, never this control-plane mount.
        mount(selector["store"], target, flags=MOUNT_BIND)
    fixture_target = root / "bench-trial/work/input"
    mount(toolroot / "fs-v2-fixture", fixture_target, flags=MOUNT_BIND)
    mount(None, fixture_target, flags=MOUNT_REMOUNT | MOUNT_BIND | MOUNT_RDONLY)
    mount("tmpfs", root / "tmp", "tmpfs", options="mode=1777,size=1024m")
    mount("tmpfs", root / "run", "tmpfs", options="mode=0755,size=16m")
    mount("proc", root / "proc", "proc")
    mount("/dev", root / "dev", flags=MOUNT_BIND | MOUNT_REC)
    mount("/sys", root / "sys", flags=MOUNT_BIND | MOUNT_REC)
    receipt = dict(
        schema=1,
        backend=config["backend"],
        namespaces=identities,
        parent_namespaces=config["parent_namespaces"],
        toolroot=str(toolroot),
        readonly_prepared_mounts=mounts,
        trial_mount="/bench-trial",
        stage_storage=config.get("stage_storage"),
        workspace_input=dict(
            source=str(toolroot / "fs-v2-fixture"), target="/bench-trial/work/input", readonly=True
        ),
        private_tmp="1024 MiB tmpfs",
        root_switch="pivot_root",
        old_root_detached=False,
    )
    # A portable libc entry avoids executing host mount tools after root switch.
    pivot = getattr(libc, "pivot_root", None)
    if pivot is None:
        raise RuntimeError("libc pivot_root is unavailable")
    pivot.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
    pivot.restype = ctypes.c_int
    os.chdir(root)
    if pivot(os.fsencode(root), os.fsencode(root / ".old-root")) != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), "pivot_root")
    os.chdir("/")
    if libc.umount2(b"/.old-root", DETACH) != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), "old-root detach")
    receipt["old_root_detached"] = True
    receipt["mountinfo"] = Path("/proc/self/mountinfo").read_text()
    save("/bench-trial/namespace.json", receipt)
    os.chdir("/bench-trial/work")
    print(
        "USER_WORKLOADS_LAUNCH "
        + json.dumps(dict(monotonic_ns=time.monotonic_ns(), backend=config["backend"])),
        flush=True,
    )
    os.execvpe(config["argv"][0], config["argv"], os.environ.copy())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--namespace-child", required=True, type=Path)
    args = parser.parse_args()
    _namespace_child(args.namespace_child)
