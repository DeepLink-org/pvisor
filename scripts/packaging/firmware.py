"""Build the in-tree guest firmware and retain its corresponding sources."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from contextlib import contextmanager
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parents[2]
FW = ROOT / "fw"
MACOS_DEPLOYMENT_TARGET = "11.0"


class ResolvedFirmware(NamedTuple):
    path: Path
    library_name: str | None
    cargo_variable: str
    source_record: str

    @property
    def cargo_env(self) -> dict[str, str]:
        return {self.cargo_variable: str(self.path)}


def host_target() -> str:
    machine = platform.machine().lower()
    if sys.platform == "darwin" and machine in {"arm64", "aarch64"}:
        return "aarch64-apple-darwin"
    if sys.platform == "linux" and machine in {"x86_64", "amd64"}:
        return "x86_64-unknown-linux-musl"
    raise RuntimeError(
        f"automatic libkrunfw preparation is unsupported on {sys.platform}/{machine}"
    )


def resolve_firmware(
    target: str,
    *,
    target_dir: str | None = None,
    jobs: str | None = None,
    offline: bool = False,
) -> ResolvedFirmware:
    """Select one input for compilation, payload copying and provenance."""
    selectors = {
        name: value
        for name in (
            "PVISOR_LIBKRUNFW_PATH",
            "PVISOR_KRUNFW_PATH",
            "PVISOR_KRUNFW_KERNEL_BUNDLE",
        )
        if (value := os.getenv(name))
    }
    if len(selectors) > 1:
        raise RuntimeError(
            "Conflicting firmware selectors: " + ", ".join(selectors) + "; set only one"
        )
    macos = target == "aarch64-apple-darwin"
    name = "libkrunfw.5.dylib" if macos else "libkrunfw.so.5"
    if selectors:
        selector, configured = next(iter(selectors.items()))
        source = Path(configured).expanduser().resolve()
        if selector == "PVISOR_KRUNFW_KERNEL_BUNDLE":
            if macos:
                raise RuntimeError("PVISOR_KRUNFW_KERNEL_BUNDLE is supported only on Linux")
            for filename in ("kernel.bin", "kernel.json"):
                if not (source / filename).is_file():
                    raise RuntimeError(f"Kernel bundle input does not exist: {source / filename}")
            record = (
                json.dumps(
                    {
                        "origin": "explicit-kernel-bundle",
                        "path": str(source),
                        "files": {
                            filename: sha256(source / filename)
                            for filename in ("kernel.bin", "kernel.json")
                        },
                        "licenses": "GPL-2.0-only (Linux kernel)",
                    },
                    indent=2,
                )
                + "\n"
            )
            return ResolvedFirmware(source, None, selector, record)
        if selector == "PVISOR_LIBKRUNFW_PATH" and source.is_dir():
            directory = source
            source = directory / name
            if not source.is_file() and not macos:
                candidates = sorted(directory.glob("libkrunfw.so.5.*"))
                if len(candidates) == 1:
                    source = candidates[0]
            source = source.resolve()
    else:
        source = build_firmware(target, target_dir=target_dir, jobs=jobs, offline=offline).resolve()
    if not source.is_file():
        raise RuntimeError(f"libkrunfw payload does not exist: {source}")
    return ResolvedFirmware(source, name, "PVISOR_KRUNFW_PATH", source_record(source))


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_files() -> list[Path]:
    files = [FW / "Makefile", FW / "bin2cbundle.py", FW / "README.md", FW / "IMPORT.md"]
    files.extend(FW.glob("config-libkrunfw*"))
    files.extend(FW.glob("LICENSE-*"))
    files.extend(FW.glob("build_on_krunvm*.sh"))
    for directory in ("patches", "patches-tee", "include", "qboot", "initrd", "utils", "tests"):
        files.extend(
            p
            for p in (FW / directory).rglob("*")
            if p.is_file() and "__pycache__" not in p.parts and p.suffix != ".pyc"
        )
    return sorted(files)


def metadata() -> dict[str, str]:
    text = (FW / "Makefile").read_text()
    result = {}
    for key in ("KERNEL_VERSION", "KERNEL_SHA256", "FULL_VERSION", "ABI_VERSION"):
        match = re.search(rf"^{key}\s*=\s*([\w.-]+)\s*$", text, re.MULTILINE)
        if not match:
            raise RuntimeError(f"fw/Makefile must declare a literal {key}")
        result[key] = match[1]
    result["KERNEL_URL"] = (
        "https://cdn.kernel.org/pub/linux/kernel/v6.x/" + result["KERNEL_VERSION"] + ".tar.xz"
    )
    return result


def build_tools(target: str) -> tuple[list[str], dict[str, str], dict[str, str]]:
    macos = target == "aarch64-apple-darwin"
    search_path = os.environ.get("PATH", os.defpath)
    if macos and (brew := shutil.which("brew")):
        try:
            prefix = Path(
                subprocess.check_output(
                    [brew, "--prefix"], text=True, stderr=subprocess.PIPE, timeout=5
                ).strip()
            )
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
            pass  # A manually configured toolchain can still build without Homebrew.
        else:
            directories = [
                prefix / "opt/llvm/bin",
                prefix / "opt/lld/bin",
                prefix / "opt/make/libexec/gnubin",
                prefix / "opt/gnu-sed/libexec/gnubin",
                prefix / "opt/gnu-tar/libexec/gnubin",
                prefix / "bin",
            ]
            search_path = os.pathsep.join(
                [str(path) for path in directories if path.is_dir()] + [search_path]
            )
    compiler = "/usr/bin/cc" if macos else "gcc"
    command = [
        "gmake" if macos else "make",
        "SEV=0",
        "TDX=0",
        "CROSS_COMPILE=",
        "OS=Darwin" if macos else "OS=Linux",
        "ARCH=arm64" if macos else "ARCH=x86_64",
        f"CC={compiler}",
    ]
    tools = {"make": command[0], "cc": compiler}
    if macos:
        command.extend(
            [
                "MACOS_BUILDER=native",
                "KERNEL_BUILD_TARGET=Image",
                f"LIBRARY_FLAGS=-mmacosx-version-min={MACOS_DEPLOYMENT_TARGET}",
            ]
        )
        for key, tool in {
            "KERNEL_MAKE": "gmake",
            "KERNEL_CC": "clang",
            "KERNEL_LD": "ld.lld",
            "KERNEL_AR": "llvm-ar",
            "KERNEL_NM": "llvm-nm",
            "KERNEL_OBJCOPY": "llvm-objcopy",
            "KERNEL_OBJDUMP": "llvm-objdump",
            "KERNEL_STRIP": "llvm-strip",
            "KERNEL_READELF": "llvm-readelf",
        }.items():
            path = shutil.which(tool, path=search_path)
            if not path:
                raise RuntimeError(
                    f"Missing firmware tool {tool}; install with "
                    "brew install llvm lld make gnu-sed gnu-tar; see fw/README.md"
                )
            command.append(f"{key}={path}")
            tools[key] = path
        command[0] = tools["make"] = tools["KERNEL_MAKE"]
    else:
        tools.update(ld="ld", strip="strip")
    identities = {}
    for key, tool in tools.items():
        try:
            identities[key] = subprocess.check_output(
                [tool, "--version"], text=True, stderr=subprocess.STDOUT
            ).strip()
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError(f"Cannot identify firmware tool {tool}; see fw/README.md") from error
    # Native CLI builds need the same dependency as isolated PEP 517 builds.
    if importlib.util.find_spec("elftools") is None:
        if not shutil.which("uv"):
            raise RuntimeError("Firmware builds need pyelftools==0.33 or uv; see fw/README.md")
        command = ["uv", "run", "--no-project", "--with", "pyelftools==0.33", *command]
        command.append("PYTHON=python")
    else:
        command.append(f"PYTHON={sys.executable}")
    return command, identities, {"PATH": search_path} if macos else {}


def download_kernel(cache: Path, info: dict[str, str], *, offline: bool) -> Path:
    archive = cache / (info["KERNEL_VERSION"] + ".tar.xz")
    if archive.is_file() and sha256(archive) == info["KERNEL_SHA256"]:
        return archive
    if offline:
        raise RuntimeError(f"Offline firmware build needs verified kernel source: {archive}")
    cache.mkdir(parents=True, exist_ok=True)
    temporary = archive.with_suffix(".download")
    try:
        print(f"Downloading guest kernel source: {info['KERNEL_URL']}", file=sys.stderr)
        with (
            urllib.request.urlopen(info["KERNEL_URL"], timeout=60) as source,
            temporary.open("wb") as out,
        ):
            shutil.copyfileobj(source, out)
        actual = sha256(temporary)
        if actual != info["KERNEL_SHA256"]:
            raise RuntimeError(
                f"Kernel source checksum mismatch: expected {info['KERNEL_SHA256']}, got {actual}"
            )
        os.replace(temporary, archive)
    finally:
        temporary.unlink(missing_ok=True)
    return archive


@contextmanager
def firmware_build_directory(work: Path, macos: bool):
    """Keep case-sensitive kernel trees on a temporary volume when the host needs it."""
    if not macos:
        yield work
        return
    with tempfile.TemporaryDirectory(dir=work) as directory:
        probe = Path(directory) / "CaseProbe"
        probe.touch()
        case_sensitive = not probe.with_name("caseprobe").exists()
    if case_sensitive:
        yield work
        return

    temporary = Path(tempfile.mkdtemp(prefix=".macos-fw-", dir=work.parent))
    image = temporary / "build.sparseimage"
    volume = temporary / "volume"
    attached = False
    try:
        print("Preparing case-sensitive APFS firmware build volume", file=sys.stderr)
        subprocess.run(
            [
                "hdiutil",
                "create",
                "-size",
                "32g",
                "-type",
                "SPARSE",
                "-fs",
                "Case-sensitive APFS",
                "-volname",
                "pvisor-fw",
                str(image),
            ],
            check=True,
        )
        volume.mkdir()
        subprocess.run(
            ["hdiutil", "attach", "-nobrowse", "-mountpoint", str(volume), str(image)],
            check=True,
        )
        attached = True
        build = volume / "fw"
        shutil.copytree(work, build, symlinks=True)
        yield build
    finally:
        # Do not remove a mounted volume if detach fails, including a partial attach.
        if attached or volume.is_mount():
            subprocess.run(["hdiutil", "detach", str(volume)], check=True)
        shutil.rmtree(temporary)


def build_firmware(
    target: str, *, target_dir: str | None = None, jobs: str | None = None, offline: bool = False
) -> Path:
    from platform import machine

    host = (sys.platform, machine().lower())
    expected = {
        "x86_64-unknown-linux-musl": ("linux", {"x86_64", "amd64"}),
        "aarch64-apple-darwin": ("darwin", {"arm64", "aarch64"}),
    }
    if (
        target not in expected
        or host[0] != expected[target][0]
        or host[1] not in expected[target][1]
    ):
        raise RuntimeError(
            f"Cannot build {target} firmware on {host}; supply an explicit firmware input"
        )
    command, tools, tool_env = build_tools(target)
    if offline and command[0] == "uv":
        command.insert(2, "--offline")
    info = metadata()
    files = source_files()
    inputs = {p.relative_to(FW).as_posix(): sha256(p) for p in files}
    identity = {
        "target": target,
        "inputs": inputs,
        "tools": tools,
        "builder_sha256": sha256(Path(__file__)),
        "macos_deployment_target": MACOS_DEPLOYMENT_TARGET,
    }
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    cache = (
        Path(os.getenv("PVISOR_FW_BUILD_DIR") or str(Path(target_dir or ROOT / "target") / "fw"))
        .expanduser()
        .resolve()
    )
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / ".build.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        work = cache / f"{target}-{key}"
        name = (
            f"libkrunfw.{info['ABI_VERSION']}.dylib"
            if target == "aarch64-apple-darwin"
            else f"libkrunfw.so.{info['FULL_VERSION']}"
        )
        output = work / name
        receipt = work / "libkrunfw.SOURCE"
        if output.is_file() and receipt.is_file():
            record = json.loads(receipt.read_text())
            if record.get("build_id") == key and record.get("firmware_sha256") == sha256(output):
                return output
        archive = download_kernel(cache / "tarballs", info, offline=offline)
        # Never resume a partially patched tree or trust artifacts without a receipt.
        shutil.rmtree(work, ignore_errors=True)
        work.mkdir()
        for source in files:
            destination = work / source.relative_to(FW)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        (work / "tarballs").mkdir()
        (work / "tarballs" / archive.name).symlink_to(archive)
        env = os.environ.copy()
        env.update(tool_env)
        # Cargo target flags and inherited Make overrides must not select a different guest.
        for variable in (
            "MAKEFLAGS",
            "MFLAGS",
            "KBUILD_OUTPUT",
            "KCONFIG_CONFIG",
            "LLVM",
            "LLVM_IAS",
            "KCFLAGS",
            "KAFLAGS",
            "KCPPFLAGS",
            "CFLAGS_KERNEL",
            "AFLAGS_KERNEL",
            "CFLAGS_MODULE",
            "AFLAGS_MODULE",
            "LDFLAGS_vmlinux",
            "HOSTCFLAGS",
            "HOSTCXXFLAGS",
            "HOSTLDFLAGS",
            "KBUILD_BUILD_USER",
            "KBUILD_BUILD_HOST",
            "KBUILD_BUILD_TIMESTAMP",
            "KBUILD_BUILD_VERSION",
            "KCONFIG_ALLCONFIG",
            "CONFIG_",
            "CFLAGS",
            "CPPFLAGS",
            "LDFLAGS",
        ):
            env.pop(variable, None)
        config = work / info["KERNEL_VERSION"] / ".config"
        with firmware_build_directory(work, target == "aarch64-apple-darwin") as build:
            command.extend(
                ["-C", str(build), "-j", jobs or str(min(os.cpu_count() or 1, 8)), "all"]
            )
            print(f"Building in-tree firmware: {shlex.join(command)}", file=sys.stderr)
            subprocess.run(command, check=True, env=env)
            built_output = build / name
            if not built_output.is_file():
                raise RuntimeError(f"Firmware build did not produce {built_output}")
            if build != work:
                shutil.copy2(built_output, output)
                config.parent.mkdir(parents=True)
                shutil.copy2(build / info["KERNEL_VERSION"] / ".config", config)
        record = {
            "origin": "pvisor/fw",
            "libkrunfw_version": info["FULL_VERSION"],
            "abi_version": info["ABI_VERSION"],
            "kernel_version": info["KERNEL_VERSION"],
            "kernel_source_url": info["KERNEL_URL"],
            "kernel_source_sha256": info["KERNEL_SHA256"],
            "build_id": key,
            "firmware_sha256": sha256(output),
            "actual_config_sha256": sha256(config),
            "command": command,
            **identity,
            "licenses": "GPL-2.0-only (Linux kernel), LGPL-2.1-only (library)",
        }
        receipt.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        return output


def source_record(source: Path) -> str:
    receipt = source.parent / "libkrunfw.SOURCE"
    if receipt.is_file():
        record = json.loads(receipt.read_text())
        if record.get("firmware_sha256") == sha256(source):
            return receipt.read_text()
    return (
        json.dumps(
            {
                "origin": "explicit-firmware-input",
                "path": str(source),
                "firmware_sha256": sha256(source),
                "licenses": "GPL-2.0-only (Linux kernel), LGPL-2.1-only (library)",
            },
            indent=2,
        )
        + "\n"
    )


def source_archive(source: Path, destination: Path) -> None:
    record = json.loads(source_record(source))
    if record["origin"] != "pvisor/fw":
        raise RuntimeError("Cannot export corresponding sources for an external firmware override")
    work = source.parent
    archive = work / "tarballs" / (record["kernel_version"] + ".tar.xz")
    if sha256(archive) != record["kernel_source_sha256"]:
        raise RuntimeError("Kernel source cache changed since firmware build")
    for name, digest in record["inputs"].items():
        if sha256(work / name) != digest:
            raise RuntimeError(f"Firmware source snapshot changed: {name}")
    config = work / record["kernel_version"] / ".config"
    if sha256(config) != record["actual_config_sha256"]:
        raise RuntimeError("Actual kernel config changed since firmware build")
    builder = Path(__file__)
    if sha256(builder) != record["builder_sha256"]:
        raise RuntimeError("Firmware builder changed since firmware build")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
            temporary = Path(output.name)
        with tarfile.open(temporary, "w:gz") as bundle:
            for name in record["inputs"]:
                bundle.add(work / name, arcname=f"fw/{name}", recursive=False)
            bundle.add(archive.resolve(), arcname=f"fw/tarballs/{archive.name}")
            bundle.add(config, arcname="actual.config")
            bundle.add(work / "libkrunfw.SOURCE", arcname="libkrunfw.SOURCE")
            bundle.add(builder, arcname="build/firmware.py")
        os.replace(temporary, destination)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main() -> None:
    """Build firmware and optionally export its receipt and corresponding sources."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default=os.getenv("CARGO_BUILD_TARGET"))
    parser.add_argument("--target-dir", default=os.getenv("CARGO_TARGET_DIR"))
    parser.add_argument("--jobs", "-j")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--source-output", type=Path)
    parser.add_argument("--source-archive", type=Path)
    args = parser.parse_args()
    source = build_firmware(
        args.target or host_target(),
        target_dir=args.target_dir,
        jobs=args.jobs,
        offline=args.offline,
    )
    if args.source_output:
        args.source_output.parent.mkdir(parents=True, exist_ok=True)
        args.source_output.write_text(source_record(source))
    if args.source_archive:
        source_archive(source, args.source_archive)
    print(source)


if __name__ == "__main__":
    main()
