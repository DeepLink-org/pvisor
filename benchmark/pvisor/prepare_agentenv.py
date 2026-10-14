#!/usr/bin/env python3
"""Prepare an AgentENV BuildKit context from an existing reference rootfs.

Preparation only: no daemon, mounts, downloads or template creation. Source
rootfs stays untouched. Generated archive may require several GiB of disk.
"""

import argparse
import hashlib
import json
import os
import shutil
import stat
import tarfile
from pathlib import Path

from agentenv_guest import digest

INPUT_ROOTS = ("bin", "sbin", "lib", "lib64", "usr", "opt/toolchain", "work")


def inventory(root):
    records = {}
    for name in INPUT_ROOTS:
        start = root / name
        if not start.exists() and not start.is_symlink():
            continue
        paths = [start]
        if start.is_dir() and not start.is_symlink():
            paths += sorted(start.rglob("*"))
        for path in paths:
            relative = str(path.relative_to(root))
            mode = path.lstat().st_mode
            item = dict(mode=stat.S_IMODE(mode))
            if stat.S_ISLNK(mode):
                item["symlink"] = os.readlink(path)
            elif stat.S_ISREG(mode):
                item["sha256"] = digest(path)
            elif stat.S_ISDIR(mode):
                continue
            else:
                raise ValueError(f"unsupported input: {relative}")
            records[relative] = item
    return records


def prepare(rootfs, output):
    rootfs = rootfs.resolve()
    output = output.absolute()
    if output.is_relative_to(rootfs):
        raise ValueError("output cannot be inside source rootfs")
    for name in (
        "usr/bin/python3",
        "usr/bin/git",
        "usr/bin/rg",
        "usr/bin/node",
        "opt/toolchain/bin/cargo",
        "work/_fs/fixture.json",
    ):
        if not (rootfs / name).exists():
            raise ValueError(f"reference rootfs missing {name}")
    fixture = json.loads((rootfs / "work/_fs/fixture.json").read_text())
    if fixture["toolchain"] != "/opt/toolchain":
        raise ValueError("reference fixture requires /opt/toolchain")
    for name in (
        "written",
        "_cargo-target",
        "_cargo-home",
        "_tmp",
        "rust/Cargo.lock",
        "node/node_modules",
    ):
        if (rootfs / "work/_fs" / name).exists():
            raise ValueError(f"fixture is not pristine: {name}")
    output.mkdir(parents=True, exist_ok=False)
    if shutil.disk_usage(output).free < 6 * 1024**3:
        raise ValueError("at least 6 GiB preparation headroom required")
    source = Path(__file__).resolve().parent
    manifest = inventory(rootfs)
    # Own copies leave the reference rootfs and prior frozen harnesses intact.
    shutil.copyfile(source / "agentenv_guest.py", output / "guest.py")
    shutil.copyfile(source / "v1/workload.py", output / "workload.py")
    for name in ("guest.py", "workload.py"):
        (output / name).chmod(0o644)
        manifest[f"bench/pvisor-agentenv/{name}"] = dict(mode=0o644, sha256=digest(output / name))
    raw = (json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode()
    (output / "input-manifest.json").write_bytes(raw)

    def normalize(info):
        if not (info.isfile() or info.isdir() or info.issym() or info.islnk()):
            raise ValueError(f"cannot export special input: {info.name}")
        if info.name == "./bench/pvisor-agentenv" or info.name.startswith(
            "./bench/pvisor-agentenv/"
        ):
            raise ValueError("reserved guest helper path already exists")
        info.uid = info.gid = 0
        info.uname = info.gname = "root"
        return info

    with tarfile.open(output / "rootfs.tar.gz", "w:gz", dereference=False) as archive:
        archive.add(rootfs, arcname=".", filter=normalize)
    before = {k: v for k, v in manifest.items() if not k.startswith("bench/pvisor-agentenv/")}
    if inventory(rootfs) != before:
        raise ValueError("source changed while exporting")
    (output / "Dockerfile").write_text(
        "FROM scratch\nADD rootfs.tar.gz /\n"
        "COPY guest.py workload.py input-manifest.json /bench/pvisor-agentenv/\n"
        'ENV PATH="/opt/toolchain/bin:/usr/local/bin:/usr/bin:/bin"\n'
        "ENV PYTHONDONTWRITEBYTECODE=1 PYTHONPYCACHEPREFIX=/__pvisor_reference_no_pyc__\n"
        'WORKDIR /work\nUSER root\nCMD ["/usr/bin/python3", "-c", "pass"]\n'
    )
    (output / ".dockerignore").write_text(
        "*\n!Dockerfile\n!rootfs.tar.gz\n!guest.py\n!workload.py\n!input-manifest.json\n"
    )
    receipt = dict(
        schema=1,
        rootfs=str(rootfs),
        source_unchanged=True,
        input_manifest_sha256=hashlib.sha256(raw).hexdigest(),
        files={
            name: digest(output / name)
            for name in (
                "rootfs.tar.gz",
                "guest.py",
                "workload.py",
                "input-manifest.json",
                "Dockerfile",
                ".dockerignore",
            )
        },
        limitations=[
            "guest identity verification prewarms input bytes",
            "template/kernel/envd additions require live inspection",
            "does not establish snapshot or durability equivalence",
        ],
    )
    (output / "preparation.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = prepare(args.rootfs, args.output)
    print(json.dumps(dict(state="prepared", input_manifest_sha256=result["input_manifest_sha256"])))


if __name__ == "__main__":
    main()
