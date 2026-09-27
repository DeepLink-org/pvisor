#!/usr/bin/env python3
"""One-command pVisor startup benchmark with local setup and preflight."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace

import startup

REPO = Path(__file__).parents[2]


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    command.add_argument("--output", type=Path, default=Path("target/pvisor-benchmark/startup"))
    command.add_argument("--pvisor", type=Path, default=Path("target/release/pvisor"))
    command.add_argument("--no-build", action="store_true", help="use an existing release binary")
    command.add_argument("--container-rootfs", type=Path)
    command.add_argument("--container-pvisor-binary", type=Path)
    command.add_argument("--container-runtime", help="crun or runc; auto-detected by default")
    command.add_argument("--vm-rootfs", help="prepared rootfs path; local rootfs by default")
    command.add_argument("--adapter", type=Path)
    command.add_argument(
        "--cases", help="comma-separated case names; default: all built-ins and adapters"
    )
    command.add_argument("--warmups", type=int, default=3)
    command.add_argument("--samples", type=int, default=30)
    command.add_argument("--resource-hold-ms", type=int, default=200)
    command.add_argument("--timeout-s", type=float, default=60)
    command.add_argument("--shell", default="/bin/sh")
    command.add_argument("--common-cpu", type=int)
    command.add_argument("--common-memory")
    command.add_argument("--keep-setup", action="store_true", help="retain the prepared rootfs")
    return command


def ldd_paths(binary: Path) -> set[Path]:
    result = subprocess.run(["ldd", str(binary)], text=True, capture_output=True, check=False)
    if "not a dynamic executable" in result.stdout + result.stderr:
        return set()
    if result.returncode != 0:
        raise RuntimeError(f"inspect dynamic libraries for {binary}: {result.stderr}")
    if "not found" in result.stdout:
        raise RuntimeError(f"missing dynamic library for {binary}: {result.stdout}")
    return {Path(value) for value in re.findall(r"(?<!\S)(/[^\s()]+)(?=\s+\(0x)", result.stdout)}


def prepare_rootfs(rootfs: Path, pvisor: Path) -> list[str]:
    """Copy only the workload programs and their loader dependencies."""
    shell = Path("/bin/sh")
    tools = [
        Path(shutil.which(name) or f"/usr/bin/{name}") for name in ("sleep", "mount", "env", "rm")
    ]
    if not shell.is_file() or any(not tool.is_file() for tool in tools):
        raise RuntimeError("local /bin/sh, sleep, mount, env, and rm are required")
    sources = {shell, *tools}
    for binary in (shell, *tools, pvisor):
        sources.update(ldd_paths(binary))
    copied = []
    for source in sorted(sources):
        target = rootfs / source.relative_to("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target, follow_symlinks=True)
        copied.append(str(source))
    for name in ("dev", "proc", "sys", "tmp", "run", "opt/persisting", "etc"):
        (rootfs / name).mkdir(parents=True, exist_ok=True)
    (rootfs / "etc/passwd").write_text("root:x:0:0:root:/root:/bin/sh\n")
    (rootfs / "etc/group").write_text("root:x:0:\n")
    (rootfs / "opt/persisting/pvisor").touch()
    env = tools[2]
    if env != Path("/usr/bin/env"):
        (rootfs / "usr/bin").mkdir(parents=True, exist_ok=True)
        shutil.copy2(env, rootfs / "usr/bin/env", follow_symlinks=True)
    return copied


def preflight(
    names: list[str],
    templates: dict[str, list[str]],
    scratch: Path,
    timeout_s: float,
    shell: str,
    adapter_cases: dict[str, list[str]],
    diagnostics: Path,
) -> tuple[list[str], dict[str, str]]:
    available: list[str] = []
    skipped: dict[str, str] = {}
    for name in names:
        print(f"preflight: {name}", flush=True)
        verify = name not in adapter_cases and name != "direct"
        try:
            for phase, workload in (
                ("startup", [shell, "-c", ":"]),
                ("occupancy", [shell, "-c", "sleep 0.05"]),
            ):
                startup.run_trial(
                    name,
                    templates[name],
                    phase=phase,
                    workload=workload,
                    scratch=scratch,
                    interval_ms=2,
                    timeout_s=timeout_s,
                    verify_pvisor=verify,
                    keep=False,
                )
        except (OSError, RuntimeError, TimeoutError, ValueError) as error:
            skipped[name] = str(error)
            diagnostics.mkdir(parents=True, exist_ok=True)
            for trial in scratch.glob(f"{name}-*"):
                log = trial / "stderr.log"
                if log.is_file():
                    shutil.copy2(log, diagnostics / f"{trial.name}.log")
            print(f"skipped {name}: {error}", flush=True)
        else:
            available.append(name)
    return available, skipped


def main() -> int:
    args = parser().parse_args()
    if args.warmups < 0 or args.samples < 1 or args.resource_hold_ms < 1:
        parser().error("warmups must be >= 0; samples and resource-hold-ms must be > 0")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    pvisor = args.pvisor.resolve()
    if not args.no_build:
        print("building release pVisor", flush=True)
        subprocess.run(["just", "build", "release"], cwd=REPO, check=True)
    if not pvisor.is_file():
        raise RuntimeError(f"pVisor binary does not exist: {pvisor}")

    adapter_cases = startup.load_adapter(args.adapter) if args.adapter else {}
    selected = (
        [name.strip() for name in args.cases.split(",")]
        if args.cases
        else [*startup.DEFAULT_CASES, *adapter_cases]
    )
    if not selected or len(selected) != len(set(selected)):
        raise ValueError("--cases must contain unique case names")
    unknown = set(selected) - set(startup.DEFAULT_CASES) - set(adapter_cases)
    if unknown:
        raise ValueError(f"unknown cases: {', '.join(sorted(unknown))}")
    if "direct" not in selected:
        selected.insert(0, "direct")
    if args.container_rootfs and args.container_rootfs.resolve() == Path("/"):
        raise ValueError("the automatic benchmark requires a prepared container rootfs, not /")
    if args.vm_rootfs == "host":
        raise ValueError("the automatic benchmark requires a prepared VM rootfs")
    if args.vm_rootfs and args.vm_rootfs.startswith("image="):
        raise ValueError("prepare the VM image first, then pass its rootfs directory")

    setup = Path(tempfile.mkdtemp(prefix="pvisor-benchmark-setup-"))
    rootfs = setup / "rootfs"
    runtime = args.container_runtime or shutil.which("crun") or shutil.which("runc")
    skipped: dict[str, str] = {}
    needs_rootfs = ("container" in selected and not args.container_rootfs) or (
        "vm" in selected and not args.vm_rootfs
    )
    copied: list[str] = []
    if needs_rootfs:
        try:
            rootfs.mkdir()
            guest_binary = (args.container_pvisor_binary or pvisor).resolve()
            copied = prepare_rootfs(rootfs, guest_binary)
        except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
            skipped.update(
                {
                    name: f"prepare local rootfs: {error}"
                    for name in ("container", "vm")
                    if name in selected
                }
            )
    container_rootfs = rootfs
    vm_rootfs = str(rootfs)
    if args.container_rootfs:
        source = args.container_rootfs.resolve()
        if source.is_dir():
            container_rootfs = setup / "container-rootfs"
            try:
                shutil.copytree(source, container_rootfs, symlinks=True)
            except OSError as error:
                skipped["container"] = f"copy prepared container rootfs: {error}"
        else:
            container_rootfs = source
    if args.vm_rootfs:
        source = Path(args.vm_rootfs).resolve()
        if source.is_dir():
            prepared_vm = setup / "vm-rootfs"
            try:
                shutil.copytree(source, prepared_vm, symlinks=True)
                vm_rootfs = str(prepared_vm)
            except OSError as error:
                skipped["vm"] = f"copy prepared VM rootfs: {error}"
        else:
            vm_rootfs = str(source)
    if "container" in selected and not runtime:
        skipped["container"] = "crun or runc is unavailable"
    if "vm" in selected and not os.access("/dev/kvm", os.R_OK | os.W_OK):
        skipped["vm"] = "/dev/kvm is unavailable or inaccessible"
    if "container" in selected and not container_rootfs.is_dir():
        skipped["container"] = f"container rootfs is unavailable: {container_rootfs}"
    if "vm" in selected and vm_rootfs != "host" and not vm_rootfs.startswith("image="):
        if not Path(vm_rootfs).is_dir():
            skipped["vm"] = f"VM rootfs is unavailable: {vm_rootfs}"
    if args.common_memory and "host_memory_limit" in selected:
        skipped["host_memory_limit"] = "conflicts with --common-memory"

    settings = SimpleNamespace(
        pvisor=pvisor,
        common_cpu=args.common_cpu,
        common_memory=args.common_memory,
        container_runtime=runtime or "crun",
        container_rootfs=container_rootfs,
        container_image=None,
        container_pvisor_binary=(args.container_pvisor_binary or pvisor).resolve(),
        container_network="none",
        vm_rootfs=vm_rootfs,
    )
    candidates = [name for name in selected if name not in skipped]
    templates = {
        name: adapter_cases.get(name) or startup.pvisor_command(name, settings)
        for name in candidates
    }
    available, preflight_skipped = preflight(
        candidates,
        templates,
        setup,
        args.timeout_s,
        args.shell,
        adapter_cases,
        output / "preflight-logs",
    )
    skipped.update(preflight_skipped)
    if "direct" not in available:
        raise RuntimeError("direct control failed preflight; no valid benchmark can be produced")
    command = [
        sys.executable,
        str(Path(__file__).with_name("startup.py")),
        "--output",
        str(output),
        "--pvisor",
        str(pvisor),
        "--cases",
        ",".join(available),
        "--warmups",
        str(args.warmups),
        "--samples",
        str(args.samples),
        "--resource-hold-ms",
        str(args.resource_hold_ms),
        "--timeout-s",
        str(args.timeout_s),
        "--shell",
        args.shell,
    ]
    if "container" in available:
        command += [
            "--container-rootfs",
            str(container_rootfs),
            "--container-pvisor-binary",
            str(settings.container_pvisor_binary),
            "--container-runtime",
            settings.container_runtime,
        ]
    if "vm" in available:
        command += ["--vm-rootfs", vm_rootfs]
    if args.adapter:
        command += ["--adapter", str(args.adapter.resolve())]
    if args.common_cpu:
        command += ["--common-cpu", str(args.common_cpu)]
    if args.common_memory:
        command += ["--common-memory", args.common_memory]
    print(f"measuring {len(available)} cases; skipping {len(skipped)}", flush=True)
    completed = subprocess.run(command, cwd=REPO, check=False)
    if completed.returncode != 0:
        print(f"startup benchmark failed; preflight workspace retained: {setup}", file=sys.stderr)
        return completed.returncode

    report_path = output / "startup.json"
    report = json.loads(report_path.read_text())
    report["skipped_cases"] = skipped
    report["setup"] = {
        "prepared_rootfs_files": copied,
        "container_rootfs": str(container_rootfs),
        "container_rootfs_source": str(args.container_rootfs) if args.container_rootfs else None,
        "vm_rootfs": vm_rootfs,
        "vm_rootfs_source": args.vm_rootfs,
        "rootfs_is_temporary": bool(needs_rootfs or args.container_rootfs or args.vm_rootfs)
        and not args.keep_setup,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    markdown = startup.render_markdown(report)
    if skipped:
        markdown += "\n## Skipped cases\n\n| Case | Reason |\n| --- | --- |\n"
        for name, reason in skipped.items():
            cell = reason.replace("|", "/").replace("\n", "<br>")
            markdown += f"| {name} | {cell} |\n"
    (output / "startup.md").write_text(markdown)
    if args.keep_setup:
        print(f"prepared rootfs retained at {setup}")
    else:
        shutil.rmtree(setup)
    print(f"results: {output / 'startup.md'} and {report_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"one-click benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
