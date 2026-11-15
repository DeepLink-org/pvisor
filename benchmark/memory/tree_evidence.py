"""Read-only identity of retained userspace and image-store trees."""

import json
import os
from pathlib import Path

from scanner_evidence import digest


def tree_manifest(root):
    """Bind bytes, links and modes without following links or omitting errors."""
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


def verify_library_inputs(inputs, receipt):
    """Recheck the exact retained userspace/library bytes, modes and links."""
    if any(digest(inputs / name) != checksum for name, checksum in receipt["hashes"].items()):
        raise ValueError("frozen input bytes changed")
    retained = json.loads((inputs / "python-root-manifest.json").read_text())
    if tree_manifest(inputs / "python-root") != retained:
        raise ValueError("Python rootfs content/mode/link manifest changed")
    if (
        digest(inputs / "python-root/libraries/libpayload.so")
        != receipt["hashes"]["libraries/libpayload.so"]
    ):
        raise ValueError("rootfs and DAX library copies differ")
    if digest(inputs / "python-root/library-worker.py") != receipt["hashes"]["library_worker.py"]:
        raise ValueError("block and virtio-fs worker bytes differ")
