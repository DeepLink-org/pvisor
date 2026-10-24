"""Shared linux cold runtime contracts for active JSON benchmarks.

Extracted from the retired runners; no historical campaign entry point.
"""

import hashlib
import os
import subprocess
import time
from pathlib import Path


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def inventory(root):
    entries = []
    for path in [root] + sorted(root.rglob("*")):
        row = dict(path=str(path.relative_to(root)), mode=path.lstat().st_mode)
        if path.is_symlink():
            row.update(kind="symlink", target=os.readlink(path))
            if path.is_file():
                row["resolved_sha256"] = digest(path)
        elif path.is_file():
            row.update(kind="file", bytes=path.stat().st_size, sha256=digest(path))
        elif path.is_dir():
            row["kind"] = "directory"
        else:
            raise ValueError("unsupported input type: " + str(path))
        entries.append(row)
    return entries


def competing_jobs(group):
    """Read-only same-user process/FD guard; inaccessible processes are explicit."""
    jobs, errors = [], []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if proc.stat().st_uid != os.getuid():
                continue
            membership = proc.joinpath("cgroup").read_text()
            own = str(group).removeprefix("/sys/fs/cgroup")
            if any(
                line.removeprefix("0::").rstrip() == own
                or line.removeprefix("0::").rstrip().startswith(own + "/")
                for line in membership.splitlines()
            ):
                continue
            args = proc.joinpath("cmdline").read_bytes().split(b"\0")
            if args and Path(os.fsdecode(args[0])).name in ("cargo", "rustc", "cargo-nextest"):
                jobs.append(
                    dict(pid=int(proc.name), kind="build/test", args=[os.fsdecode(a) for a in args])
                )
            for fd in proc.joinpath("fd").iterdir():
                try:
                    target = os.readlink(fd)
                except FileNotFoundError:
                    continue
                if target in ("/dev/kvm", "anon_inode:kvm-vm", "anon_inode:kvm-vcpu"):
                    jobs.append(
                        dict(
                            pid=int(proc.name), kind="other VM", args=[os.fsdecode(a) for a in args]
                        )
                    )
                    break
        except (FileNotFoundError, ProcessLookupError):
            continue
        except OSError as error:
            errors.append(dict(pid=int(proc.name), error=str(error)))
    return dict(time_ns=time.time_ns(), jobs=jobs, inspection_errors=errors)


def settle_unit(unit, logs):
    """Only touch our UUID-owned unit, and never wait indefinitely for cleanup."""
    try:
        stopped = subprocess.run(
            ["systemctl", "--user", "stop", unit], capture_output=True, timeout=15
        )
        (logs / "stop.stdout").write_bytes(stopped.stdout)
        (logs / "stop.stderr").write_bytes(stopped.stderr)
        shown = subprocess.run(
            ["systemctl", "--user", "show", unit, "--property=ActiveState", "--property=LoadState"],
            capture_output=True,
            timeout=10,
        )
        (logs / "unit-final.txt").write_bytes(shown.stdout + shown.stderr)
        return (
            b"ActiveState=inactive" in shown.stdout
            or b"ActiveState=failed" in shown.stdout
            or b"LoadState=not-found" in shown.stdout
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        (logs / "cleanup-error.txt").write_text(str(error))
        return False
