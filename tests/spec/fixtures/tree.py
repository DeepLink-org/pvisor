"""Canonical filesystem oracle: content, modes, links; never follow symlinks."""

import hashlib
import json
import os
import stat
import sys
from pathlib import Path


def quote(path):
    # Preserve Unix non-UTF8 names without replacement-character collisions.
    raw = os.fsencode(path)
    try:
        return json.dumps(raw.decode("utf-8"), ensure_ascii=False)
    except UnicodeDecodeError:
        return '"' + "".join(f"\\u{byte:04x}" for byte in raw) + '"'


def tree_state(root):
    root = Path(root)
    info = root.lstat()
    if not stat.S_ISDIR(info.st_mode):
        raise ValueError("tree-state requires a real directory")
    rows = [(b"", f'dir {stat.S_IMODE(info.st_mode):04o} "."')]

    def walk(directory):
        for path in directory.iterdir():
            info = path.lstat()
            relative = os.fsencode(path.relative_to(root))
            name = quote(relative)
            mode = stat.S_IMODE(info.st_mode)
            if stat.S_ISLNK(info.st_mode):
                row = f"link {name} -> {quote(os.readlink(path))}"
            elif stat.S_ISDIR(info.st_mode):
                row = f"dir {mode:04o} {name}"
            elif stat.S_ISREG(info.st_mode):
                digest = hashlib.sha256()
                with path.open("rb") as stream:
                    for chunk in iter(lambda: stream.read(65536), b""):
                        digest.update(chunk)
                row = f"file {mode:04o} {name} {digest.hexdigest()}"
            else:
                row = f"other {stat.S_IFMT(info.st_mode):o} {name}"
            rows.append((relative, row))
            if stat.S_ISDIR(info.st_mode):
                walk(path)

    walk(root)
    return "".join(row + "\n" for _, row in sorted(rows))


if __name__ == "__main__":
    print(tree_state(sys.argv[1]), end="")
