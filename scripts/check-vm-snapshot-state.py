#!/usr/bin/env python3
"""Validate VMM-thread CPU/RAM/GIC restore; this is not a complete Linux VM test."""

import argparse
import copy
import hashlib
import json
import os
import platform
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=Path("target"))
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("requires Apple Silicon macOS")
    root = Path(__file__).resolve().parents[1]
    target = args.target_dir.resolve()
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--offline",
            "-p",
            "pvisor-vm",
            "--example",
            "threaded_cold_restore_case",
            "--target-dir",
            str(target),
        ],
        cwd=root,
        check=True,
    )
    binary = target / "debug/examples/threaded_cold_restore_case"
    subprocess.run(
        [
            "codesign",
            "--force",
            "--sign",
            "-",
            "--entitlements",
            str(root / "crates/pvisor/macos-hypervisor.entitlements"),
            str(binary),
        ],
        check=True,
    )
    checks = []

    def run(mode, path, secondary="waiting", error=None):
        result = subprocess.run(
            [str(binary), mode, str(path)],
            capture_output=True,
            text=True,
            timeout=15,
            env={**os.environ, "PVISOR_CASE_SECONDARY": secondary},
        )
        if error is None:
            assert result.returncode == 0 and "panicked" not in result.stderr, result.stderr
        else:
            assert result.returncode != 0 and error in result.stderr, result.stderr
        checks.append(
            {
                "mode": mode,
                "case": path.stem,
                "secondary": secondary,
                "exit_code": result.returncode,
                "stdout": result.stdout,
                "stderr": result.stderr,
            }
        )

    with tempfile.TemporaryDirectory(prefix="pvisor-threaded-snapshot-") as directory:
        directory = Path(directory)
        for secondary in ["waiting", "running", "pending"]:
            path = directory / f"{secondary}.json"
            run("save", path, secondary)
            # subprocess.run waited for the source process to exit.
            run("restore", path, secondary)
        path = directory / "waiting.json"
        original = path.read_bytes()
        run("save", path, error="File exists")
        assert path.read_bytes() == original
        image = json.loads(original)
        cases = [
            ("version", "invalid image", lambda s: s.update(version=2)),
            ("cpu-id", "CPU topology", lambda s: s["cpus"][1].update(id=0)),
            ("psci", "CPU topology", lambda s: s["cpus"][0].update(pending_boot=1)),
            (
                "cpu-feature",
                "CPU feature mismatch",
                lambda s: s["cpus"][0]["cpu"]["features"][0].__setitem__(
                    1, s["cpus"][0]["cpu"]["features"][0][1] ^ 1
                ),
            ),
            ("ram", "RAM checksum mismatch", lambda s: s["ram"].__setitem__(1024, 99)),
            ("gic-layout", "GIC layout", lambda s: s["gic"]["properties"].__setitem__(0, 0)),
            (
                "irq",
                "invalid pending interrupt",
                lambda s: s["gic"]["pending"]["queues"][0].append(1023),
            ),
            (
                "gic-topology",
                "invalid pending interrupt",
                lambda s: s["gic"]["pending"]["queues"].pop(),
            ),
        ]
        for name, error, mutate in cases:
            changed = copy.deepcopy(image)
            mutate(changed)
            path = directory / f"invalid-{name}.json"
            path.write_text(json.dumps(changed))
            run("restore", path, error=error)
    sources = [
        "crates/pvisor-vm/src/vmm/macos/vstate.rs",
        "crates/pvisor-vm/src/devices/legacy/gicv3.rs",
        "crates/pvisor-vm/src/devices/legacy/vcpu.rs",
        "crates/pvisor-vm/src/devices/virtio/queue.rs",
        "crates/pvisor-vm/src/devices/virtio/mmio.rs",
        "crates/pvisor-vm/src/contract_tests/vm_snapshot_state.rs",
        "crates/pvisor-vm/src/probes/threaded_cold_restore_case.rs",
        str(Path(__file__).relative_to(root)),
    ]
    report = {
        "scope": "VMM-thread bare-metal 2-vCPU RAM/software-GIC; no Linux/virtio-fs/console/vsock",
        "full_vm_restore": False,
        "host": platform.platform(),
        "checks": checks,
        "source_sha256": {
            name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sources
        },
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    }
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(checks)} VMM thread checks passed; full Linux VM restore remains incomplete")


if __name__ == "__main__":
    main()
