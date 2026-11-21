"""Build the in-tree experimental Rust native kernel for the existing pVisor VMM."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
MAX_IMAGE_SIZE = 256 * 1024 * 1024


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def source_files() -> list[Path]:
    files = [
        HERE / name
        for name in (
            "Cargo.toml",
            "Cargo.lock",
            "Components.toml",
            "rust-toolchain.toml",
            "provenance.json",
            "IMPORT.md",
            "build.py",
            ".licenserc.yaml",
            "OSDK.toml",
        )
    ]
    files.extend(HERE.glob("LICENSE*"))
    for directory in ("kernel", "ostd", "osdk", "vdso"):
        files.extend(
            file
            for file in (HERE / directory).rglob("*")
            if file.is_file()
            and not any(part in {"target", "__pycache__"} for part in file.relative_to(HERE).parts)
        )
    return sorted(files)


def kernel_image(elf_path: Path, architecture: str = "x86_64") -> tuple[bytes, int, int]:
    from elftools.elf.elffile import ELFFile

    with elf_path.open("rb") as stream:
        elf = ELFFile(stream)
        machine = {"x86_64": "EM_X86_64", "aarch64": "EM_AARCH64"}[architecture]
        if elf.elfclass != 64 or not elf.little_endian or elf["e_machine"] != machine:
            raise RuntimeError(f"Expected a little-endian {architecture} ELF kernel")
        segments = [segment for segment in elf.iter_segments() if segment["p_type"] == "PT_LOAD"]
        if not segments:
            raise RuntimeError("Kernel has no loadable segments")
        start = min(segment["p_paddr"] for segment in segments)
        end = max(segment["p_paddr"] + segment["p_memsz"] for segment in segments)
        end = (end + 4095) & ~4095
        if start % 4096 or not 0 < end - start <= MAX_IMAGE_SIZE:
            raise RuntimeError("Invalid kernel physical span")
        image = bytearray(end - start)
        ranges = []
        for segment in segments:
            address, size = segment["p_paddr"], segment["p_memsz"]
            if segment["p_filesz"] > size:
                raise RuntimeError("ELF file segment exceeds memory segment")
            if any(address < limit and address + size > base for base, limit in ranges):
                raise RuntimeError("Overlapping kernel segments")
            ranges.append((address, address + size))
            offset = address - start
            image[offset : offset + segment["p_filesz"]] = segment.data()
        symbols = elf.get_section_by_name(".symtab")
        symbol = "__linux64_boot" if architecture == "x86_64" else "_header"
        entries = symbols.get_symbol_by_name(symbol) if symbols else None
        if not entries or len(entries) != 1:
            raise RuntimeError(f"Kernel is missing its boot entry {symbol}")
        entry = entries[0]["st_value"]
        if architecture == "aarch64" and (
            start != 0x80200000
            or entry != start
            or elf["e_entry"] != start + 64
            or image[:4] != b"\x10\x00\x00\x94"
            or int.from_bytes(image[8:16], "little") != 0x200000
            or not 64 <= int.from_bytes(image[16:24], "little") <= len(image)
            or int.from_bytes(image[24:32], "little") != 2
            or image[56:60] != b"ARM\x64"
        ):
            raise RuntimeError("Invalid pVisor ARM64 Linux Image header or physical layout")
        if not any(
            segment["p_flags"] & 1
            and segment["p_paddr"] <= entry < segment["p_paddr"] + segment["p_filesz"]
            for segment in segments
        ):
            raise RuntimeError("Linux boot entry is outside executable physical segments")
        return bytes(image), start, entry


def build(args: argparse.Namespace) -> Path:
    if sys.platform not in {"linux", "darwin"}:
        raise RuntimeError("Rust native kernel requires a Linux or macOS build host")
    architecture = args.architecture
    triple = {
        "x86_64": "x86_64-unknown-none",
        "aarch64": "aarch64-unknown-none-softfloat",
    }[architecture]
    toolchain = json.loads((HERE / "provenance.json").read_text())["toolchain"]
    cache = args.build_dir.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    for variable in (
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "RUSTUP_TOOLCHAIN",
    ):
        env.pop(variable, None)
    env.update(
        RUSTUP_HOME=str(cache / "rustup"),
        OSDK_LOCAL_DEV="1",
        CARGO_NET_OFFLINE=str(args.offline).lower(),
        CARGO_BUILD_JOBS=str(args.jobs),
        OSDK_TARGET_ARCH=architecture,
    )
    if args.prepare_toolchain:
        if args.offline:
            raise RuntimeError("--prepare-toolchain requires network access")
        subprocess.run(
            [
                "rustup",
                "toolchain",
                "install",
                toolchain,
                "--profile",
                "minimal",
                "--component",
                "rust-src,rustc-dev,llvm-tools-preview",
                "--target",
                triple,
            ],
            env=env,
            check=True,
            stdout=sys.stderr,
        )
    tools = subprocess.check_output(
        ["rustup", "run", toolchain, "rustc", "-vV"],
        env=env,
        text=True,
    )
    files = source_files()
    identity = {
        "inputs": {file.relative_to(HERE).as_posix(): sha256(file) for file in files},
        "toolchain": tools,
        "profile": f"rust-native-virtiofs-{architecture}-v1",
    }
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    with (cache / ".build.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        work = cache / key
        output = work / "bundle"
        receipt = output / "pvisor-kernel.SOURCE"
        if receipt.is_file():
            record = json.loads(receipt.read_text())
            if (
                all(record.get(name) == value for name, value in identity.items())
                and set(record.get("artifacts", {})) == {"kernel.bin", "kernel.json"}
                and all(
                    (output / name).is_file() and sha256(output / name) == digest
                    for name, digest in record["artifacts"].items()
                )
            ):
                return output
            raise RuntimeError(f"Cached native kernel bundle changed: {output}")
        if work.exists():
            shutil.rmtree(work)
        source = work / "source"
        source.mkdir(parents=True)
        for file in files:
            copied = source / file.relative_to(HERE)
            copied.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(file, copied)
        env["VDSO_LIBRARY_DIR"] = str(source / "vdso")
        env["CARGO_TARGET_DIR"] = str(work / "target")
        if architecture == "aarch64":
            sysroot = subprocess.check_output(
                ["rustup", "run", toolchain, "rustc", "--print", "sysroot"],
                env=env,
                text=True,
            ).strip()
            host = next(
                line.removeprefix("host: ")
                for line in tools.splitlines()
                if line.startswith("host: ")
            )
            objcopy = Path(sysroot) / "lib/rustlib" / host / "bin/llvm-objcopy"
            if not objcopy.is_file():
                raise RuntimeError("Missing llvm-objcopy; rerun with --prepare-toolchain")
            tool_dir = work / "tools"
            tool_dir.mkdir()
            (tool_dir / "rust-objcopy").symlink_to(objcopy)
            env["PATH"] = str(tool_dir) + os.pathsep + env["PATH"]
        subprocess.run(
            [
                "rustup",
                "run",
                toolchain,
                "cargo",
                "build",
                "--locked",
                "--release",
                "--manifest-path",
                str(source / "osdk/Cargo.toml"),
            ],
            env=env,
            cwd=source,
            check=True,
            stdout=sys.stderr,
        )
        subprocess.run(
            [
                str(work / "target/release/cargo-osdk"),
                "osdk",
                "build",
                "--release",
                "--no-default-features",
                "--boot-method",
                "vmm-direct",
            ],
            env=env,
            cwd=source / "kernel",
            check=True,
            stdout=sys.stderr,
        )
        image, guest_addr, entry_addr = kernel_image(
            work / f"target/{triple}/release/pvisor-kernel-osdk-bin", architecture
        )
        output.mkdir()
        (output / "kernel.bin").write_bytes(image)
        (output / "kernel.json").write_text(
            json.dumps(
                {
                    "format": 1,
                    "kernel": "rust-native",
                    "architecture": architecture,
                    "boot_protocol": "linux64" if architecture == "x86_64" else "linux-arm64",
                    "guest_addr": guest_addr,
                    "entry_addr": entry_addr,
                    "size": len(image),
                    "sha256": hashlib.sha256(image).hexdigest(),
                },
                indent=2,
            )
            + "\n"
        )
        receipt.write_text(
            json.dumps(
                {
                    **identity,
                    "kernel_lock_sha256": sha256(
                        work / "target/osdk/pvisor-kernel-run-base/Cargo.lock"
                    ),
                    "artifacts": {
                        name: sha256(output / name) for name in ("kernel.bin", "kernel.json")
                    },
                },
                indent=2,
                sort_keys=True,
            )
            + "\n"
        )
        return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--architecture",
        choices=("x86_64", "aarch64"),
        default={
            "arm64": "aarch64",
            "aarch64": "aarch64",
            "x86_64": "x86_64",
            "amd64": "x86_64",
        }.get(platform.machine().lower()),
    )
    parser.add_argument("--build-dir", type=Path, default=ROOT / "target/pvisor-kernel")
    parser.add_argument("--jobs", "-j", type=int, default=min(os.cpu_count() or 1, 8))
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--prepare-toolchain", action="store_true")
    args = parser.parse_args()
    if args.architecture is None:
        parser.error("unsupported host architecture; specify --architecture explicitly")
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    print(build(args))


if __name__ == "__main__":
    main()
