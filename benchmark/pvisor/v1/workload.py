"""Identical stdlib-only filesystem workloads for native, staged, VM and OCI runs."""

import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path


def main():
    mode = sys.argv[1]
    configuration = json.loads(Path("fixture.json").read_text())
    started = time.perf_counter_ns()
    if mode == "metadata":
        files = list(Path("tree").rglob("*.txt"))
        total = sum(p.stat().st_size for p in files)
        assert len(files) == configuration["files"] and total == configuration["tree_bytes"]
        check = {"files": len(files), "bytes": total}
    elif mode == "read":
        with Path("payload.bin").open("rb") as source:
            value = hashlib.file_digest(source, "sha256").hexdigest()
        assert value == configuration["payload_sha256"]
        check = {"bytes": configuration["payload_bytes"], "sha256": value}
    elif mode == "write":
        Path("written").mkdir()
        pattern = b"pvisor-workload\n"
        chunk = (pattern * (64 * 1024 // len(pattern) + 1))[:64 * 1024]
        for i in range(256):
            Path(f"written/{i:04d}").write_bytes(chunk)
        value = sum(p.stat().st_size for p in Path("written").iterdir())
        assert value == len(chunk) * 256
        check = {"files": 256, "bytes": value}
    elif mode == "git":
        output = subprocess.check_output(
            ["git", "-c", "safe.directory=" + str(Path("tree").resolve()), "status", "--porcelain"],
            cwd="tree",
            text=True,
        )
        assert output == ""
        check = {"git_clean": True}
    elif mode == "rg":
        output = subprocess.check_output(["rg", "-l", "pvisor-fixture-needle", "tree"], text=True)
        assert len(output.splitlines()) == configuration["files"]
        check = {"matches": len(output.splitlines())}
    elif mode == "cargo":
        toolchain = Path(configuration["toolchain"])
        Path("_tmp").mkdir(exist_ok=True)
        env = (
            os.environ
            | {"TMPDIR": str(Path("_tmp").resolve())}
            | {
                "RUSTC": str(toolchain / "bin/rustc"),
                "CARGO_TARGET_DIR": str(Path("_cargo-target").resolve()),
                "CARGO_HOME": str(Path("_cargo-home").resolve()),
            }
        )
        subprocess.run(
            [
                str(toolchain / "bin/cargo"),
                "build",
                "--offline",
                "--release",
                "--manifest-path",
                "rust/Cargo.toml",
            ],
            env=env,
            check=True,
            stdout=subprocess.DEVNULL,
        )
        output = subprocess.check_output(["_cargo-target/release/fixture"], text=True).strip()
        assert output == str(sum(range(64)))
        check = {"modules": 64, "result": output}
    elif mode == "npm":
        subprocess.run(
            [
                "npm",
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
            ],
            cwd="node",
            check=True,
            stdout=subprocess.DEVNULL,
        )
        installed = [p for p in Path("node/node_modules").iterdir() if p.is_dir()]
        assert len(installed) == configuration["npm_packages"]
        check = {"local_packages": len(installed)}
    else:
        raise ValueError(mode)
    elapsed = (time.perf_counter_ns() - started) / 1e6
    if mode == "write":
        # Validate every byte after the operation timer. Completion still
        # includes this validation equally for every reference runtime.
        assert all(Path(f"written/{i:04d}").read_bytes() == chunk for i in range(256))
        check["file_sha256"] = hashlib.sha256(chunk).hexdigest()
    print(
        json.dumps(
            {
                "workload": mode,
                "worker_ms": elapsed,
                "check": check,
                "python": sys.version.split()[0],
                "python_cache": {
                    "dont_write_bytecode": sys.dont_write_bytecode,
                    "prefix": sys.pycache_prefix,
                    "prefix_exists": bool(sys.pycache_prefix and Path(sys.pycache_prefix).exists()),
                },
                "tool_cache": {name: os.environ.get(name) for name in (
                    "TMPDIR", "HOME", "CARGO_HOME", "NODE_COMPILE_CACHE", "NODE_DISABLE_COMPILE_CACHE", "NODE_OPTIONS"
                )},
            }
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()
