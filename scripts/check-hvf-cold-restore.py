#!/usr/bin/env python3
"""Run the M0 CPU/RAM cold-restore check on real HVF, never a Linux VM claim."""

import argparse
import copy
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
        parser.error("requires Apple Silicon macOS with Hypervisor entitlement")
    root = Path(__file__).resolve().parents[1]
    target = args.target_dir.resolve()
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "-p",
            "pvisor",
            "--example",
            "hvf_cold_restore_case",
            "--target-dir",
            str(target),
        ],
        cwd=root,
        check=True,
        timeout=300,
    )
    binary = target / "debug/examples/hvf_cold_restore_case"
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
        timeout=30,
    )
    results = []

    def run(name, mode, path, expected_error=None):
        result = subprocess.run(
            [str(binary), mode, str(path)],
            capture_output=True,
            text=True,
            timeout=20,
        )
        if expected_error is None:
            if result.returncode:
                raise RuntimeError(f"{name}: {result.stderr}")
        elif result.returncode == 0 or expected_error not in result.stderr:
            raise RuntimeError(f"{name}: expected {expected_error!r}: {result.stderr}")
        results.append(
            {
                "name": name,
                "exit_code": result.returncode,
                "stdout": result.stdout.strip(),
                "stderr": result.stderr.strip(),
            }
        )

    with tempfile.TemporaryDirectory(prefix="pvisor-cold-m0-") as directory:
        directory = Path(directory)
        for index in range(3):
            path = directory / f"snapshot-{index}.json"
            run(f"save-{index}", "save", path)
            run(f"restore-{index}", "restore", path)
        source = path.read_bytes()
        run("refuse-overwrite", "save", path, "File exists")
        assert path.read_bytes() == source, "failed save modified existing snapshot"
        image = json.loads(source)
        mutations = [
            ("schema", "invalid vCPU schema", lambda s: s["cpu"].update(version=999)),
            ("missing-register", "invalid vCPU schema", lambda s: s["cpu"]["registers"].pop()),
            (
                "system-order",
                "invalid system register list",
                lambda s: s["cpu"]["system"].reverse(),
            ),
            (
                "feature-mismatch",
                "CPU feature mismatch",
                lambda s: s["cpu"]["features"][0].__setitem__(1, s["cpu"]["features"][0][1] ^ 1),
            ),
            (
                "unsupported-SME",
                "not supported",
                lambda s: s["cpu"]["features"][3].__setitem__(
                    1, s["cpu"]["features"][3][1] | (1 << 24)
                ),
            ),
            (
                "invalid-MMIO",
                "invalid deferred MMIO",
                lambda s: s["cpu"]["mmio_read"].update(len=3),
            ),
            ("RAM-length", "invalid RAM layout", lambda s: s["ram"].pop()),
            ("RAM-corrupt", "RAM checksum mismatch", lambda s: s["ram"].__setitem__(1024, 1)),
            ("unknown-field", "unknown field", lambda s: s["cpu"].update(unrecognized=True)),
        ]
        for name, error, mutate in mutations:
            changed = copy.deepcopy(image)
            mutate(changed)
            bad = directory / f"{name}.json"
            bad.write_text(json.dumps(changed))
            run(name, "restore", bad, error)
        truncated = directory / "truncated.json"
        truncated.write_bytes(source[: len(source) // 2])
        run("truncated", "restore", truncated, "EOF")
    report = {
        "scope": "M0 single-vCPU bare-metal HVF CPU/RAM; no Linux, GIC, virtio or filesystem",
        "host": platform.platform(),
        "page_bytes": os.sysconf("SC_PAGE_SIZE"),
        "checks": results,
    }
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(results)} M0 checks passed; full pVisor VM restore is not implemented")


if __name__ == "__main__":
    main()
