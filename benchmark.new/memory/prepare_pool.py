#!/usr/bin/env python3
"""Prepare frozen B-MEMORY-SCALE inputs; builds only, never runs measurements."""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

from scanner_compare import REPO, digest, save

sys.path.insert(0, str(REPO / "benchmark/pvisor"))
from firecracker_ksm import WORKER, page  # noqa: E402


def run(command, root, name, cwd=REPO):
    with (
        (root / (name + ".stdout")).open("wb") as out,
        (root / (name + ".stderr")).open("wb") as err,
    ):
        completed = subprocess.run(command, cwd=cwd, stdout=out, stderr=err, timeout=900)
    save(
        root / (name + ".command.json"),
        dict(command=[str(x) for x in command], returncode=completed.returncode),
    )
    completed.check_returncode()


def source_inputs():
    names = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=REPO
    )
    selected = sorted({os.fsdecode(x) for x in names.split(b"\0") if x})
    rows = []
    for relative in selected:
        if not (
            relative in ("Cargo.toml", "Cargo.lock", ".cargo/config", ".cargo/config.toml")
            or relative.startswith(("crates/", "vendor/", "fw/"))
        ):
            continue
        path = REPO / relative
        if path.is_symlink():
            row = dict(path=relative, symlink=os.readlink(path))
        elif path.is_file():
            row = dict(path=relative, bytes=path.stat().st_size, sha256=digest(path))
        else:
            row = dict(path=relative, absent=True)
        rows.append(row)
    return rows


def tree_manifest(root):
    """Bind Python userspace bytes, links and modes without following links."""
    if root.is_symlink() or not root.is_dir():
        raise ValueError("userspace root must be a real directory")
    rows = [dict(path=".", mode=root.stat().st_mode & 0o7777, directory=True)]

    def failed(error):
        raise error

    for directory, dirs, files in os.walk(root, followlinks=False, onerror=failed):
        for name in sorted(dirs + files):
            path = Path(directory) / name
            row = dict(path=str(path.relative_to(root)), mode=path.lstat().st_mode & 0o7777)
            if path.is_symlink():
                row["symlink"] = os.readlink(path)
            elif path.is_file():
                row.update(bytes=path.stat().st_size, sha256=digest(path))
            elif path.is_dir():
                row["directory"] = True
            else:
                raise ValueError(f"unsupported rootfs object: {path}")
            rows.append(row)
    return sorted(rows, key=lambda row: row["path"])


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--firmware", type=Path, required=True)
    p.add_argument("--kernel", type=Path, required=True)
    p.add_argument("--firecracker", type=Path, default=Path("/usr/bin/firecracker"))
    p.add_argument(
        "--firecracker-filter",
        type=Path,
        help="explicit official seccomp BPF for stock Firecracker",
    )
    p.add_argument(
        "--firecracker-build-receipt", type=Path, help="frozen stock Firecracker build receipt"
    )
    p.add_argument(
        "--library-rootfs",
        type=Path,
        help="optional prepared Python rootfs for common dlopen controls",
    )
    a = p.parse_args()
    root = a.output.resolve()
    root.mkdir()
    before = source_inputs()
    save(root / "source-files.json", before)
    with tarfile.open(root / "source.tar.gz", "w:gz", dereference=False) as archive:
        for row in before:
            if not row.get("absent"):
                archive.add(REPO / row["path"], arcname=row["path"], recursive=False)
    (root / "source.patch").write_bytes(
        subprocess.check_output(["git", "diff", "--binary", "HEAD"], cwd=REPO)
    )
    (root / "source.status").write_bytes(
        subprocess.check_output(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=REPO
        )
    )
    run(
        [
            "cargo",
            "build",
            "--locked",
            "--offline",
            "--release",
            "-p",
            "pvisor",
            "--example",
            "pool_memory_case",
            "-p",
            "pvisor-daemon",
            "--bin",
            "pvisor-daemon",
        ],
        root,
        "product-build",
    )
    if source_inputs() != before:
        raise ValueError("product source changed during build")
    for source, name in [
        (REPO / "target/release/examples/pool_memory_case", "probe"),
        (REPO / "target/release/pvisor-daemon", "daemon"),
        (a.firmware, "firmware"),
        (a.kernel, "kernel"),
        (a.firecracker, "firecracker"),
    ]:
        shutil.copy2(source, root / name)
    extras = []
    for source, name in [
        (a.firecracker_filter, "firecracker-filter.bpf"),
        (a.firecracker_build_receipt, "firecracker-build-receipt.json"),
    ]:
        if source:
            shutil.copy2(source, root / name)
            extras.append(name)
    if a.firecracker_build_receipt:
        fc = json.loads((root / "firecracker-build-receipt.json").read_text())
        for name in ("firecracker", *(["firecracker-filter.bpf"] if a.firecracker_filter else [])):
            if digest(root / name) != fc["hashes"][name]:
                raise ValueError("Firecracker build receipt mismatch")
    (root / "worker.rs").write_text(WORKER)
    run(
        [
            "rustc",
            "--edition=2021",
            "--target",
            "x86_64-unknown-linux-musl",
            "-C",
            "opt-level=3",
            "-o",
            str(root / "worker"),
            str(root / "worker.rs"),
        ],
        root,
        "guest-build",
    )
    guest = root / "root"
    guest.mkdir()
    for name in ("dev", "proc", "sys", "tmp", "run", "etc"):
        (guest / name).mkdir()
    shutil.copy2(root / "worker", guest / "worker")
    with (root / "guest.ext4").open("xb") as stream:
        stream.truncate(96 * 1024**2)
    run(["mkfs.ext4", "-q", "-F", "-d", str(guest), str(root / "guest.ext4")], root, "guest-image")
    if a.library_rootfs:
        shutil.copytree(a.library_rootfs, root / "python-root", symlinks=True)
        library = root / "libraries"
        library.mkdir()
        shutil.copy2(Path(__file__).with_name("library_payload.c"), root / "library_payload.c")
        shutil.copy2(Path(__file__).with_name("library_worker.py"), root / "library_worker.py")
        with (root / "payload").open("xb") as stream:
            for n in range(64 * 1024**2 // 4096):
                stream.write(page("random-shared", 0, n, 0))
        run(
            [
                "objcopy",
                "--input-target",
                "binary",
                "--output-target",
                "elf64-x86-64",
                "--binary-architecture",
                "i386:x86-64",
                "--rename-section",
                ".data=.rodata,alloc,load,readonly,data,contents",
                "payload",
                "payload.o",
            ],
            root,
            "library-object",
            root,
        )
        run(
            [
                "objcopy",
                "--set-section-alignment",
                ".rodata=4096",
                "payload.o",
                "payload-aligned.o",
            ],
            root,
            "library-alignment",
            root,
        )
        run(
            [
                "gcc",
                "-shared",
                "-fPIC",
                "-O2",
                "-Wl,-z,noexecstack",
                "library_payload.c",
                "payload-aligned.o",
                "-o",
                "libraries/libpayload.so",
            ],
            root,
            "library-link",
            root,
        )
        normal_library = root / "python-root/libraries"
        normal_library.mkdir()
        # Independent copies: the immutable DAX source keeps its single-link contract.
        shutil.copy2(library / "libpayload.so", normal_library / "libpayload.so")
        worker_path = root / "python-root/library-worker.py"
        if worker_path.exists() or worker_path.is_symlink():
            raise ValueError("prepared source already contains the reserved library worker path")
        shutil.copy2(root / "library_worker.py", worker_path)
        os.chmod(library / "libpayload.so", 0o444)
        os.chmod(library, 0o555)
        manifest = tree_manifest(root / "python-root")
        save(root / "python-root-manifest.json", manifest)
        total = sum(row.get("bytes", 0) for row in manifest)
        quantum = 64 * 1024**2
        image_size = max(
            3 * quantum, ((total * 3 // 2 + quantum + quantum - 1) // quantum) * quantum
        )
        with (root / "library.ext4").open("xb") as stream:
            stream.truncate(image_size)
        run(
            ["mkfs.ext4", "-q", "-F", "-d", str(root / "python-root"), str(root / "library.ext4")],
            root,
            "library-image",
        )
    runtime = [
        "probe",
        "daemon",
        "firmware",
        "kernel",
        "firecracker",
        "worker",
        "worker.rs",
        "root/worker",
        "guest.ext4",
    ]
    runtime += extras
    if a.library_rootfs:
        runtime += [
            "libraries/libpayload.so",
            "library_worker.py",
            "library_payload.c",
            "payload",
            "python-root-manifest.json",
            "library.ext4",
        ]
    harness = root / "preparation-source"
    harness.mkdir()
    for name in (
        "prepare_pool.py",
        "pool_compare.py",
        "scanner_compare.py",
        "library_payload.c",
        "library_worker.py",
        "library_compare.py",
        "library_protocol.py",
        "library_evidence.py",
    ):
        shutil.copy2(Path(__file__).with_name(name), harness / name)
    receipt = dict(
        hashes={name: digest(root / name) for name in runtime},
        source_head=subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
        ).strip(),
        source_dirty=bool((root / "source.status").read_text()),
        source_archive_sha256=digest(root / "source.tar.gz"),
        source_manifest_sha256=digest(root / "source-files.json"),
        binary_source_relationship="Cargo release build; actual repository inputs archived and verified unchanged across build",
        dependency_scope="external Cargo registry cache and separate libkrunfw source not archived; firmware bytes frozen",
        compiler=subprocess.check_output(["rustc", "-Vv"], text=True),
        preparation_sha256={f.name: digest(f) for f in harness.iterdir()},
        fc_version=subprocess.check_output([str(root / "firecracker"), "--version"], text=True),
        fc_kernel="retained reference kernel; differs from pVisor firmware",
    )
    if a.firecracker_build_receipt:
        receipt["firecracker_build"] = json.loads(
            (root / "firecracker-build-receipt.json").read_text()
        )
    if a.library_rootfs:
        receipt["library_toolchain"] = {}
        for name, command in (
            ("gcc", ["gcc", "--version"]),
            ("objcopy", ["objcopy", "--version"]),
            ("mkfs.ext4", ["mkfs.ext4", "-V"]),
        ):
            result = subprocess.run(command, capture_output=True, text=True, check=True, timeout=10)
            receipt["library_toolchain"][name] = dict(
                command=command, stdout=result.stdout, stderr=result.stderr
            )
        receipt["library_dependency_scope"] = (
            "system C headers/tool binaries not archived; exact generated library, Python userspace manifest and ext4 bytes frozen"
        )
    save(root / "receipt.json", receipt)
    print(root)


if __name__ == "__main__":
    main()
