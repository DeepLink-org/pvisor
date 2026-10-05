#!/usr/bin/env python3
"""Diagnostic controls for cache expiry, concurrent metadata and guest tmpfs.

Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role diagnostic.
Motivation: tell whether VM overhead comes from round trips, cache expiry,
request concurrency or the guest itself.
Conclusion sought: which kernel-path change would move B-FS-TOOLS the most.
Design: targeted controls against a guest tmpfs lower bound; never used as
acceptance timing.

These workloads are deliberately separate from filesystem_ab.py acceptance.
They do not replace a staged workspace with an unrecorded temporary filesystem.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

from reference_baselines import digest, validate_bundle_execution

PAYLOAD = r'''
import concurrent.futures, hashlib, json, os, shutil, tempfile, time
from pathlib import Path

configuration = json.loads(Path('_fs/fixture.json').read_text())
tree = Path('_fs/tree')
files = sorted(tree.rglob('*.txt'))
assert len(files) == configuration['files']

def timed(call):
    started = time.perf_counter_ns()
    value = call()
    return {'elapsed_ms': (time.perf_counter_ns()-started)/1e6, 'check': value}

def metadata(root):
    values = list(root.rglob('*.txt'))
    count = sum(p.stat().st_size for p in values)
    assert len(values) == configuration['files'] and count == configuration['tree_bytes']
    return {'files': len(values), 'bytes': count}

rows = []
for trial in range(3):
    # Initial rglob also populates directory/attribute caches through READDIRPLUS.
    # Expire those before the first measured pass too.
    time.sleep(1.2)
    for case in ['after_expiry', 'immediate_repeat']:
        rows.append({'case': case, 'trial': trial, **timed(lambda: metadata(tree))})

# Independent file partitions avoid measuring duplicate concurrent stats.
partitions = [files[index::4] for index in range(4)]
def partition(paths):
    return sum(p.stat().st_size for p in paths)
for trial in range(3):
    for workers in [1, 4]:
        time.sleep(1.2)
        def concurrent_metadata():
            with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
                total = sum(pool.map(partition, partitions))
            assert total == configuration['tree_bytes']
            return {'files': len(files), 'bytes': total}
        rows.append({'case': 'partitioned_stat', 'workers': workers, 'trial': trial,
                     **timed(concurrent_metadata)})

def write_partial(root):
    root.mkdir()
    block = b'p' * 1024
    for index in range(64):
        with (root/str(index)).open('wb', buffering=0) as target:
            for _ in range(8):
                assert target.write(block) == len(block)
    for path in root.iterdir():
        assert path.read_bytes() == block * 8
    return {'files': 64, 'bytes': 64*8192,
            'sha256': hashlib.sha256(block*8).hexdigest()}

rows.append({'case': 'partial_writes_staged', **timed(lambda: write_partial(Path('_probe')))})
mounts = Path('/proc/mounts').read_text().splitlines()
assert any(line.split()[1:3] == ['/dev/shm', 'tmpfs'] for line in mounts)
local = Path(tempfile.mkdtemp(prefix='pvisor-kernel-probe-', dir='/dev/shm'))
try:
    # Reproduce exactly the same tree sizes; fixture creation is outside timings.
    for source in files:
        destination = local/'tree'/source.relative_to(tree)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(b'x' * source.stat().st_size)
    for trial in range(3):
        rows.append({'case': 'local_tmpfs_metadata', 'trial': trial,
                     **timed(lambda: metadata(local/'tree'))})
    rows.append({'case': 'partial_writes_local_tmpfs',
                 **timed(lambda: write_partial(local/'written'))})
finally:
    shutil.rmtree(local)
print('KERNEL_PROBE_RESULT '+json.dumps({'correctness': 'passed', 'rows': rows}), flush=True)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--binary", action="append", required=True, help="label=path")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cpu-affinity", default="0,1")
    parser.add_argument("--profile", action="store_true")
    args = parser.parse_args()
    args.assets, args.firmware, args.output = (
        path.resolve() for path in (args.assets, args.firmware, args.output)
    )
    args.output.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, args.output / "harness.py")
    report = {
        "schema": "pvisor-filesystem-kernel-probe/v1",
        "scope": "diagnostic; separate workloads, no acceptance performance claim",
        "profiled": args.profile,
        "host_kernel": os.uname().release,
        "load_before": os.getloadavg(),
        "cpu_affinity": args.cpu_affinity,
        "vm_vcpus": 2,
        "vm_memory_mib": 16384,
        "payload_sha256": hashlib.sha256(PAYLOAD.encode()).hexdigest(),
        "harness_sha256": digest(Path(__file__)),
        "assets_manifest_sha256": digest(args.assets / "assets.json"),
        "fixture_sha256": digest(args.assets / "rootfs/work/_fs/fixture.json"),
        "firmware_sha256": digest(args.firmware / "libkrunfw.so.5"),
        "rows": [],
    }

    def save():
        report["load_after"] = os.getloadavg()
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    save()
    try:
        binaries = [value.split("=", 1) for value in args.binary]
        for label, binary in binaries:
            if not label.replace("-", "").isalnum():
                raise ValueError("binary label must contain letters, digits or hyphens")
            directory = args.output / label
            directory.mkdir()
            pinned = directory / "pvisor"
            shutil.copy2(Path(binary).resolve(), pinned)
            for backend in (["native"] if label == binaries[0][0] else []) + [
                "pvisor-staged", "pvisor-vm"
            ]:
                root = directory / backend
                root.mkdir()
                work, stage = root / "workspace", root / "stage"
                subprocess.run(
                    ["cp", "--reflink=auto", "-a", str(args.assets / "rootfs/work"), str(work)],
                    check=True,
                )
                command = ["/usr/bin/python3", "-c", PAYLOAD]
                if backend != "native":
                    command = [
                        str(pinned), "run", "--no-agent-defaults", "--overlaynet", "off",
                        "--stdio", "inherit", "--timeout", "120s", "--stage", str(stage),
                    ] + (
                        ["--vm", "--rootfs", str(args.assets / "rootfs"), "--cpu", "2",
                         "--memory", "16384MiB", "--vm-library-dir", str(args.firmware)]
                        if backend == "pvisor-vm" else []
                    ) + ["--", "/usr/bin/python3", "-c", PAYLOAD]
                command = ["taskset", "--cpu-list", args.cpu_affinity, *command]
                env = {
                    key: value for key, value in os.environ.items()
                    if key.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")
                }
                env.update(PVISOR_RUN_HOME=str(root / "runs"), XDG_CONFIG_HOME=str(root / "config"),
                           PVISOR_FS_PROFILE="1" if args.profile else "0", PVISOR_STARTUP_TIMING="0")
                env.pop("PVISOR_TEST_ALLOW_NO_USERNS", None)
                env.pop("PVISOR_VM_FS_WORKERS", None)
                result = subprocess.run(command, cwd=work, env=env, text=True,
                                        capture_output=True, timeout=150)
                (root / "stdout.log").write_text(result.stdout)
                (root / "stderr.log").write_text(result.stderr)
                (root / "command.json").write_text(json.dumps({"argv": command, "exit": result.returncode}, indent=2))
                if result.returncode or "Kernel panic" in result.stdout:
                    raise RuntimeError(f"{label}/{backend} failed; inspect {root}")
                values = [json.loads(line.removeprefix("KERNEL_PROBE_RESULT "))
                          for line in result.stdout.splitlines() if line.startswith("KERNEL_PROBE_RESULT ")]
                if len(values) != 1 or values[0]["correctness"] != "passed":
                    raise ValueError("missing successful diagnostic result")
                if backend != "native":
                    bundles = list((root / "runs").glob("*/run-bundle.json")) + list(
                        stage.glob("run-bundle.json")
                    )
                    if len(bundles) != 1:
                        raise ValueError("expected one Run Bundle")
                    validate_bundle_execution(json.loads(bundles[0].read_text()), backend, "rootless_process")
                    if (work / "_probe").exists():
                        raise ValueError("diagnostic writes reached lower workspace")
                    written = list((stage / "upper/_probe").iterdir())
                    if {path.name for path in written} != {str(index) for index in range(64)}:
                        raise ValueError("missing staged partial-write files")
                    if any(path.read_bytes() != b"p" * 8192 for path in written):
                        raise ValueError("staged partial-write contents differ")
                report["rows"].append({"label": label, "backend": backend,
                                       "binary_sha256": digest(pinned), "result": values[0]})
                save()
                shutil.rmtree(work)
                if stage.exists():
                    shutil.rmtree(stage / "upper")
    except BaseException as error:
        report["failure"] = str(error) or type(error).__name__
        save()
        raise
    save()
    print(f"diagnostic report: {args.output / 'report.json'}")


if __name__ == "__main__":
    main()
