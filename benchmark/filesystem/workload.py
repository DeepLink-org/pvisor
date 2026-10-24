#!/usr/bin/env python3
"""Deterministic native metadata/read subjects and their shared input fixture."""

import argparse
import hashlib
import json
from pathlib import Path

FILES = 2048
DIRECTORIES = 32
PAYLOAD_MIB = 64
PATTERN = b"pvisor-fixture-needle\n"
CHUNK = bytes(range(256)) * 4096
MODES = ("metadata", "read")


def expected_fixture():
    checksum = hashlib.sha256()
    for _ in range(PAYLOAD_MIB):
        checksum.update(CHUNK)
    return dict(
        directories=DIRECTORIES,
        files=FILES,
        tree_bytes=FILES * len(PATTERN),
        payload_bytes=PAYLOAD_MIB * len(CHUNK),
        payload_sha256=checksum.hexdigest(),
    )


def prepare_fixture(root):
    """Create one fixed tree and payload, without allocating a full payload in RAM."""
    root.mkdir(parents=True, exist_ok=False)
    tree = root / "tree"
    tree.mkdir()
    for directory in range(DIRECTORIES):
        (tree / f"d{directory:02d}").mkdir()
    for index in range(FILES):
        (tree / f"d{index % DIRECTORIES:02d}" / f"f{index:06d}.txt").write_bytes(PATTERN)
    with (root / "payload.bin").open("wb") as stream:
        for _ in range(PAYLOAD_MIB):
            stream.write(CHUNK)
    (root / "fixture.json").write_text(json.dumps(expected_fixture()) + "\n")


def validate_fixture(root):
    """Check the exact inventory, file contents and payload against fixed inputs."""
    if {path.name for path in root.iterdir()} != {"tree", "payload.bin", "fixture.json"}:
        raise ValueError("filesystem fixture inventory changed")
    if (root / "fixture.json").is_symlink() or not (root / "fixture.json").is_file():
        raise ValueError("filesystem fixture configuration changed")
    expected = expected_fixture()
    if json.loads((root / "fixture.json").read_text()) != expected:
        raise ValueError("filesystem fixture configuration changed")
    tree = root / "tree"
    names = {f"d{i:02d}" for i in range(DIRECTORIES)} | {
        f"d{i % DIRECTORIES:02d}/f{i:06d}.txt" for i in range(FILES)
    }
    paths = list(tree.rglob("*"))
    if (
        root.is_symlink()
        or tree.is_symlink()
        or {p.relative_to(tree).as_posix() for p in paths} != names
    ):
        raise ValueError("filesystem tree inventory changed")
    for path in paths:
        if path.is_symlink():
            raise ValueError("filesystem tree contains a symlink")
        if "/" not in path.relative_to(tree).as_posix():
            if not path.is_dir():
                raise ValueError("filesystem directory changed")
        elif not path.is_file() or path.read_bytes() != PATTERN:
            raise ValueError("filesystem tree content changed")
    payload = root / "payload.bin"
    checksum = hashlib.sha256()
    if (
        payload.is_symlink()
        or not payload.is_file()
        or payload.stat().st_size != expected["payload_bytes"]
    ):
        raise ValueError("filesystem payload changed")
    with payload.open("rb") as stream:
        for chunk in iter(lambda: stream.read(len(CHUNK)), b""):
            checksum.update(chunk)
    if checksum.hexdigest() != expected["payload_sha256"]:
        raise ValueError("filesystem payload changed")
    return expected


def execute(mode, root):
    """Run one whole-command probe, including its result validation."""
    fixture = json.loads((root / "fixture.json").read_text())
    if mode == "metadata":
        paths = list((root / "tree").rglob("*.txt"))
        if (
            len(list((root / "tree").iterdir())) != fixture["directories"]
            or len(paths) != fixture["files"]
            or sum(path.stat().st_size for path in paths) != fixture["tree_bytes"]
        ):
            raise ValueError("metadata differs from fixture")
    elif mode == "read":
        payload = (root / "payload.bin").read_bytes()
        if (
            len(payload) != fixture["payload_bytes"]
            or hashlib.sha256(payload).hexdigest() != fixture["payload_sha256"]
        ):
            raise ValueError("payload differs from fixture")
    else:
        raise ValueError("unknown filesystem probe")
    return dict(workload=mode, correctness="passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=MODES)
    parser.add_argument("fixture", type=Path)
    args = parser.parse_args()
    print(json.dumps(execute(args.mode, args.fixture)))
