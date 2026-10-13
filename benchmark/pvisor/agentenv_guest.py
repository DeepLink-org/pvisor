"""Guest helper for the AgentENV filesystem capability preflight.

Runs the existing v1/workload.py unchanged. Input hashing deliberately warms
files before execution; these observations are not cold-cache measurements.
"""

import argparse
import hashlib
import json
import math
import os
import platform
import resource
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

TOOLS = ("metadata", "read", "write", "git", "rg", "cargo", "npm")
PREFIX = "PVISOR_AGENTENV_RESULT "


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def verify_inputs(manifest, root=Path("/"), after=False):
    for name, expected in manifest.items():
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("unsafe manifest path")
        if after and name == "work/_fs/tree/.git/index":
            # git status may refresh stat data; tracked content is checked too.
            continue
        path = root / relative
        if "symlink" in expected:
            if not path.is_symlink() or os.readlink(path) != expected["symlink"]:
                raise ValueError(f"input symlink changed: {name}")
        elif path.is_symlink() or not path.is_file() or digest(path) != expected["sha256"]:
            raise ValueError(f"input bytes changed: {name}")
        if path.lstat().st_mode & 0o7777 != expected["mode"]:
            raise ValueError(f"input mode changed: {name}")


def remove_generated(path):
    if path.is_symlink():
        raise ValueError(f"unexpected generated symlink: {path}")
    if path.is_dir():
        shutil.rmtree(path)
    elif path.exists():
        path.unlink()


def run_suite(workspace, worker, calls, scratch_base, child_timeout=90):
    workspace = workspace.resolve()
    fs = workspace / "_fs"
    configuration = json.loads((fs / "fixture.json").read_text())
    if configuration["toolchain"] != "/opt/toolchain":
        raise ValueError("fixture must use /opt/toolchain")
    scratch_parent = scratch_base / ".data"
    if scratch_parent.is_symlink():
        raise ValueError("scratch parent is a symlink")
    scratch_parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="pvisor-reference-", dir=scratch_parent))
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("NODE_", "PYTHON", "PVISOR_REFERENCE_"))
    }
    env.update(
        PATH="/opt/toolchain/bin:/usr/local/bin:/usr/bin:/bin",
        RUSTC="/opt/toolchain/bin/rustc",
        TMPDIR=str(scratch),
        HOME=str(scratch / "reference-home"),
        CARGO_HOME=str(scratch / "reference-cargo"),
        NODE_COMPILE_CACHE=str(scratch / "node-compile-cache"),
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONPYCACHEPREFIX="/__pvisor_reference_no_pyc__",
        PVISOR_REFERENCE_TOOL_SCRATCH="executor",
        LC_ALL="C",
    )
    rows = []
    generated = {
        "write": ("written",),
        "cargo": ("_cargo-target", "_cargo-home", "_tmp", "rust/Cargo.lock"),
        "npm": ("node/node_modules",),
    }
    for call in range(1, calls + 1):
        for tool in TOOLS:
            for name in ("reference-home", "reference-cargo", "node-compile-cache", ".npm"):
                remove_generated(scratch / name)
            for name in generated.get(tool, ()):
                remove_generated(fs / name)
            before = resource.getrusage(resource.RUSAGE_CHILDREN)
            start = time.perf_counter_ns()
            proc = subprocess.run(
                [sys.executable, "-B", str(worker), tool],
                cwd=fs,
                env=env,
                capture_output=True,
                text=True,
                timeout=child_timeout,
            )
            child_ms = (time.perf_counter_ns() - start) / 1e6
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
            if proc.returncode:
                raise RuntimeError(f"{tool} failed ({proc.returncode}): {proc.stderr[-4000:]}")
            record = json.loads(proc.stdout.strip())
            operation_ms = record["worker_ms"]
            if (
                record["workload"] != tool
                or not math.isfinite(operation_ms)
                or not 0 <= operation_ms <= child_ms
            ):
                raise ValueError("invalid worker result")
            if record["python_cache"] != dict(
                dont_write_bytecode=True, prefix="/__pvisor_reference_no_pyc__", prefix_exists=False
            ):
                raise ValueError("worker Python cache policy mismatch")
            if tool == "npm":
                dependencies = json.loads((fs / "node/package.json").read_text())["dependencies"]
                installed = fs / "node/node_modules"
                if {p.name for p in installed.iterdir() if p.is_dir()} != set(dependencies):
                    raise ValueError("npm package set mismatch")
                for name, location in dependencies.items():
                    if not location.startswith("file:local/"):
                        raise ValueError("nonlocal npm dependency")
                    source = fs / "node" / location[5:]
                    for item in source.rglob("*"):
                        if (
                            item.is_file()
                            and (installed / name / item.relative_to(source)).read_bytes()
                            != item.read_bytes()
                        ):
                            raise ValueError("npm package bytes mismatch")
            row = dict(
                tool=tool,
                call=call,
                operation_ms=operation_ms,
                child_ms=child_ms,
                correctness="passed",
                worker=record,
                stderr=proc.stderr,
                resources={
                    k: getattr(after, k) - getattr(before, k)
                    for k in (
                        "ru_utime",
                        "ru_stime",
                        "ru_minflt",
                        "ru_majflt",
                        "ru_nvcsw",
                        "ru_nivcsw",
                    )
                },
            )
            rows.append(row)
            print("PVISOR_AGENTENV_CALL " + json.dumps(row), flush=True)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--calls", type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    base = Path(__file__).resolve().parent
    raw = (base / "input-manifest.json").read_bytes()
    manifest = json.loads(raw)
    verify_inputs(manifest)
    rows = run_suite(
        Path("/work"), base / "workload.py", args.calls, Path(os.environ.get("TMPDIR", "/tmp"))
    )
    verify_inputs(manifest, after=True)
    tracked = subprocess.run(
        ["/usr/bin/git", "-c", "safe.directory=/work/_fs/tree", "status", "--porcelain"],
        cwd="/work/_fs/tree",
        capture_output=True,
        text=True,
        check=True,
    )
    if tracked.stdout:
        raise ValueError("fixture git tree changed")
    result = dict(
        schema=1,
        correctness="passed",
        calls=rows,
        input_manifest_sha256=hashlib.sha256(raw).hexdigest(),
        inputs_verified=True,
        guest_kernel=platform.release(),
        guest_cpus=os.cpu_count(),
        guest_meminfo=Path("/proc/meminfo").read_text(),
        mounts=Path("/proc/self/mountinfo").read_text(),
        cache_state="input-hash-warmed; template snapshot state retained; outputs/tool caches reset per tool",
    )
    print(PREFIX + json.dumps(result), flush=True)


if __name__ == "__main__":
    main()
