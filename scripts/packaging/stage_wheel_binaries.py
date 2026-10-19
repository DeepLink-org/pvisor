#!/usr/bin/env python3
"""Build pVisor components and stage native product binaries for wheels."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

import firmware as firmware_build

ROOT = Path(__file__).resolve().parents[2]
WHEEL_DATA = ROOT / "target" / "wheel-data"
CLI_BINARIES = (
    "pvisor",
    "pvisor-tui",
    "pvisor-replay",
)
EXPECTED_BINARIES = (*CLI_BINARIES, "pvisor-daemon")
BUILD_COMPONENTS = {
    "cli": ("pvisor-cli", CLI_BINARIES),
    "daemon": ("pvisor-daemon", ("pvisor-daemon",)),
    "shim": ("pvisor-shim", ("containerd-shim-pvisor-v2",)),
    "shim-vm": ("pvisor-shim", ("containerd-shim-pvisor-v2",)),
    "guest": ("pvisor-guest", ("pvisor-guest",)),
}
SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-musl",
    "aarch64-apple-darwin",
}
MACOS_ENTITLEMENTS = ROOT / "crates" / "pvisor" / "macos-hypervisor.entitlements"


@dataclass(frozen=True)
class BuildOptions:
    target: str | None = None
    profile: str = "release"
    target_dir: str | None = None
    locked: bool = True
    frozen: bool = False
    offline: bool = False
    jobs: str | None = None
    bundle_firmware: bool = True


def copy_artifact(source: Path, destination: Path) -> None:
    """Replace a built artifact without truncating an executable still in use."""
    with tempfile.TemporaryDirectory(prefix=f".{destination.name}-", dir=destination.parent) as tmp:
        staged = Path(tmp) / destination.name
        shutil.copy2(source, staged)
        os.replace(staged, destination)


def ensure_wheel_data_directory() -> Path:
    """Create the wheel scripts layout without compiling the CLI binaries."""
    scripts = WHEEL_DATA / "scripts"
    scripts.mkdir(parents=True, exist_ok=True)
    return scripts


def _setting(config: Mapping[str, Any] | None, name: str) -> str | None:
    if not config:
        return None
    value = config.get(name)
    if value is None:
        value = config.get(f"--{name}")
    if isinstance(value, list):
        value = value[-1] if value else None
    return None if value is None else str(value)


def _bool_setting(config: Mapping[str, Any] | None, name: str, *, default: bool) -> bool:
    value = _setting(config, name)
    if value is None:
        return default
    normalized = value.strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    raise RuntimeError(f"{name} must be a boolean, got {value!r}")


def _normalize_target(target: str | None) -> str | None:
    if target is None:
        machine = platform.machine().lower()
        if sys.platform == "linux" and machine in {"x86_64", "amd64"}:
            return "x86_64-unknown-linux-musl"
        if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
            return None
        raise RuntimeError(
            f"wheel CLI staging is not supported on host {sys.platform}/{platform.machine()}"
        )

    aliases = {
        "x86_64": "x86_64-unknown-linux-musl",
        "aarch64": "aarch64-apple-darwin"
        if sys.platform == "darwin"
        else "aarch64-unknown-linux-musl",
        "arm64": "aarch64-apple-darwin",
    }
    normalized = aliases.get(target, target)
    if normalized not in SUPPORTED_TARGETS:
        supported = ", ".join(sorted(SUPPORTED_TARGETS))
        raise RuntimeError(f"unsupported wheel target {normalized!r}; expected one of: {supported}")
    return normalized


def options_from_build_backend(
    config_settings: Mapping[str, Any] | None,
    *,
    editable: bool,
) -> BuildOptions:
    """Resolve Cargo options for the setuptools-backed PEP 517 build."""
    default_profile = "dev" if editable else "release"
    target = _setting(config_settings, "cargo-target") or os.getenv("CARGO_BUILD_TARGET")
    target_dir = _setting(config_settings, "cargo-target-dir") or os.getenv("CARGO_TARGET_DIR")
    return BuildOptions(
        target=_normalize_target(target),
        profile=_setting(config_settings, "cargo-profile") or default_profile,
        target_dir=target_dir,
        locked=_bool_setting(config_settings, "cargo-locked", default=True),
        frozen=_bool_setting(config_settings, "cargo-frozen", default=False),
        offline=_bool_setting(config_settings, "cargo-offline", default=False),
        jobs=_setting(config_settings, "cargo-jobs"),
        bundle_firmware=_bool_setting(config_settings, "bundle-firmware", default=not editable),
    )


def _cargo_command(
    options: BuildOptions,
    *,
    component: str = "cli",
) -> list[str]:
    target = (
        _normalize_target(options.target)
        if options.target is not None or sys.platform == "linux"
        else None
    )
    if component not in BUILD_COMPONENTS:
        raise ValueError(f"unknown build component: {component}")
    if component in ("shim", "shim-vm") and target != "x86_64-unknown-linux-musl":
        raise RuntimeError("shim builds require Linux x86_64 (static musl)")
    package, binaries = BUILD_COMPONENTS[component]
    command = [
        "cargo",
        "zigbuild" if target == "x86_64-unknown-linux-musl" else "build",
        "--profile",
        options.profile,
        "--message-format=json-render-diagnostics",
        "-p",
        package,
    ]
    for name in binaries:
        command.extend(("--bin", name))
    if component == "cli":
        command.extend(("--features", "pvisor-cli/gateway"))
    elif component == "shim-vm":
        command.extend(("--features", "vm"))
    elif component == "shim":
        command.append("--no-default-features")
    if target is not None:
        command.extend(("--target", target))
    if options.target_dir is not None:
        command.extend(("--target-dir", options.target_dir))
    if options.frozen:
        command.append("--frozen")
    elif options.locked:
        command.append("--locked")
    if options.offline:
        command.append("--offline")
    if options.jobs is not None:
        command.extend(("--jobs", options.jobs))
    return command


def _prepare_zig_file_limit() -> None:
    import resource

    # Thin-LTO links open thousands of object files. Cargo's job limit does not
    # reduce the descriptors needed by an individual Zig linker process.
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    desired = 16_384
    if soft == resource.RLIM_INFINITY or soft >= desired:
        return
    available = desired if hard == resource.RLIM_INFINITY else min(desired, hard)
    if available <= soft:
        return
    try:
        resource.setrlimit(resource.RLIMIT_NOFILE, (available, hard))
    except (OSError, ValueError) as error:
        raise RuntimeError(
            f"Cannot raise the Zig build open-file limit from {soft} to {available} "
            f"(hard limit {hard}); increase the build process's open-file limit"
        ) from error
    print(f"Zig build open-file limit: {soft} -> {available}", file=sys.stderr)


def _build(
    options: BuildOptions,
    *,
    firmware: firmware_build.ResolvedFirmware | None = None,
) -> dict[str, Path]:
    firmware = firmware if firmware is not None else _resolve_firmware(options)
    artifacts = _build_component(options, firmware=firmware)
    artifacts.update(_build_component(options, component="daemon", firmware=firmware))
    return artifacts


def _build_component(
    options: BuildOptions,
    *,
    component: str = "cli",
    firmware: firmware_build.ResolvedFirmware | None = None,
) -> dict[str, Path]:
    command = _cargo_command(options, component=component)
    expected = BUILD_COMPONENTS[component][1]
    print(f"Building pVisor {component}: {shlex.join(command)}", file=sys.stderr)
    build_env = os.environ.copy()
    if component in ("cli", "daemon", "shim-vm"):
        firmware = firmware if firmware is not None else _resolve_firmware(options)
    for selector in (
        "PVISOR_LIBKRUNFW_PATH",
        "PVISOR_KRUNFW_PATH",
        "PVISOR_KRUNFW_KERNEL_BUNDLE",
    ):
        build_env.pop(selector, None)
    if firmware is not None:
        build_env.update(firmware.cargo_env)
    if command[1] == "zigbuild":
        _prepare_zig_file_limit()
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        env=build_env,
        stdout=subprocess.PIPE,
        text=True,
    )
    assert process.stdout is not None
    artifacts: dict[str, Path] = {}
    for line in process.stdout:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(line, end="", file=sys.stderr)
            continue
        if message.get("reason") == "compiler-message":
            rendered = message.get("message", {}).get("rendered")
            if rendered:
                print(rendered, end="", file=sys.stderr)
        if message.get("reason") != "compiler-artifact":
            continue
        executable = message.get("executable")
        name = message.get("target", {}).get("name")
        kinds = message.get("target", {}).get("kind", [])
        if executable and name in expected and "bin" in kinds:
            artifacts[name] = Path(executable)

    return_code = process.wait()
    if return_code != 0:
        raise subprocess.CalledProcessError(return_code, command)
    missing = sorted(set(expected) - artifacts.keys())
    if missing:
        raise RuntimeError(
            f"Cargo did not report expected {component} binaries: {', '.join(missing)}"
        )
    if _is_macos(options) and firmware is not None:
        assert firmware.library_name is not None
        for directory in {path.parent for path in artifacts.values()}:
            destination = directory / firmware.library_name
            if firmware.path != destination.resolve():
                copy_artifact(firmware.path, destination)
            (directory / "libkrunfw.SOURCE").write_text(firmware.source_record)
    return artifacts


def _build_all(options: BuildOptions) -> dict[str, Path]:
    """Build supported workspace executables, sharing one firmware resolution."""
    firmware = _resolve_firmware(options)
    artifacts = _build(options, firmware=firmware)
    if not _is_macos(options):
        artifacts.update(_build_component(options, component="shim-vm", firmware=firmware))
        artifacts.update(_build_component(options, component="guest"))
    return artifacts


def _is_macos(options: BuildOptions) -> bool:
    return options.target == "aarch64-apple-darwin" or (
        options.target is None and sys.platform == "darwin"
    )


def _resolve_firmware(options: BuildOptions) -> firmware_build.ResolvedFirmware:
    target = (
        _normalize_target(options.target)
        if options.target is not None
        else "aarch64-apple-darwin"
        if _is_macos(options)
        else firmware_build.host_target()
    )
    assert target is not None
    return firmware_build.resolve_firmware(
        target,
        target_dir=options.target_dir,
        jobs=options.jobs,
        offline=options.offline or options.frozen,
    )


def _sign_macos_pvisor(path: Path) -> None:
    subprocess.run(
        [
            "codesign",
            "--force",
            "--sign",
            "-",
            "--entitlements",
            str(MACOS_ENTITLEMENTS),
            str(path),
        ],
        check=True,
    )


def stage_wheel_binaries(options: BuildOptions) -> Path:
    """Build the host CLI and atomically replace the wheel scripts directory."""
    firmware = _resolve_firmware(options)
    artifacts = _build(options, firmware=firmware)
    ensure_wheel_data_directory()
    staged = WHEEL_DATA / f".scripts-{os.getpid()}"
    backup = WHEEL_DATA / f".scripts-old-{os.getpid()}"
    shutil.rmtree(staged, ignore_errors=True)
    shutil.rmtree(backup, ignore_errors=True)
    staged.mkdir()

    try:
        for name in EXPECTED_BINARIES:
            source = artifacts[name]
            destination = staged / name
            shutil.copy2(source, destination)
            destination.chmod(
                destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
            )
            print(f"Staged {name}: {source} -> {destination}", file=sys.stderr)

        if options.bundle_firmware:
            if _is_macos(options):
                assert firmware.library_name is not None
                firmware_destination = staged / firmware.library_name
                shutil.copy2(firmware.path, firmware_destination)
                print(
                    f"Staged libkrunfw: {firmware.path} -> {firmware_destination}",
                    file=sys.stderr,
                )
            (staged / "libkrunfw.SOURCE").write_text(firmware.source_record, encoding="utf-8")
        if _is_macos(options):
            for name in EXPECTED_BINARIES:
                _sign_macos_pvisor(staged / name)

        scripts = WHEEL_DATA / "scripts"
        if scripts.exists():
            os.replace(scripts, backup)
        try:
            os.replace(staged, scripts)
        except BaseException:
            if backup.exists():
                os.replace(backup, scripts)
            raise
        shutil.rmtree(backup, ignore_errors=True)
        return scripts
    finally:
        shutil.rmtree(staged, ignore_errors=True)


def main() -> None:
    """Build components and publish artifacts into the selected profile directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("dev", "release", "performance"), default="release")
    parser.add_argument("--target-dir", default=os.getenv("CARGO_TARGET_DIR", str(ROOT / "target")))
    parser.add_argument("--target", default=os.getenv("CARGO_BUILD_TARGET"))
    component = parser.add_mutually_exclusive_group()
    component.add_argument(
        "--all",
        action="store_const",
        dest="component",
        const="all",
        help="build all supported workspace binaries",
    )
    for name in ("daemon", "shim", "shim-vm"):
        component.add_argument(f"--{name}", action="store_const", dest="component", const=name)
    args = parser.parse_args()
    target = _normalize_target(args.target) if args.target else None
    if args.component in ("shim", "shim-vm"):
        if target and target != "x86_64-unknown-linux-musl":
            parser.error("shim builds require x86_64-unknown-linux-musl")
        target = "x86_64-unknown-linux-musl"
    options = BuildOptions(target=target, profile=args.profile, target_dir=args.target_dir)
    if args.component == "all":
        artifacts = _build_all(options)
    elif args.component:
        artifacts = _build_component(options, component=args.component)
    else:
        artifacts = _build(options)
    if _is_macos(options):
        directory = next(iter(artifacts.values())).parent
        artifacts.update(
            {name: directory / name for name in ("libkrunfw.5.dylib", "libkrunfw.SOURCE")}
        )
    directory = Path(args.target_dir) / ("debug" if args.profile == "dev" else args.profile)
    directory.mkdir(parents=True, exist_ok=True)
    for name, source in artifacts.items():
        destination = directory / name
        if source.resolve() != destination.resolve():
            copy_artifact(source, destination)


if __name__ == "__main__":
    main()
