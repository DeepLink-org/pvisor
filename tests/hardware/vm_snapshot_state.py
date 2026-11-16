"""VMM-thread CPU/RAM/software-GIC contracts; not a complete Linux VM test."""

import copy
import hashlib
import json
import os
import platform
import subprocess
import unittest
from pathlib import Path

from tests.fixtures import workspace
from tests.hardware import support


class VmSnapshotStateTests(unittest.TestCase):
    def setUp(self):
        support.require_hvf()
        self.binary = support.binary()

    def test_vcpu_topology_ram_and_gic_restore_contracts(self):
        checks = []

        def run(mode, path, secondary="waiting", error=None):
            result = subprocess.run(
                [str(self.binary), mode, str(path)],
                capture_output=True,
                text=True,
                timeout=15,
                env={**os.environ, "PVISOR_CASE_SECONDARY": secondary},
            )
            support.assert_response(self, result, error)
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

        with workspace() as (directory, _):
            for secondary in ["waiting", "running", "pending"]:
                with self.subTest(secondary=secondary):
                    path = directory / f"{secondary}.json"
                    run("save", path, secondary)
                    # subprocess.run reaps the source before restore.
                    run("restore", path, secondary)
            path = directory / "waiting.json"
            original = path.read_bytes()
            run("save", path, error="File exists")
            self.assertEqual(path.read_bytes(), original)
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
                with self.subTest(corruption=name):
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
            str(Path(__file__).relative_to(support.ROOT)),
            "tests/hardware/support.py",
        ]
        support.write_evidence(
            {
                "scope": "VMM-thread bare-metal 2-vCPU RAM/software-GIC; no Linux/virtio-fs/console/vsock",
                "full_vm_restore": False,
                "host": platform.platform(),
                "checks": checks,
                "source_sha256": {
                    name: hashlib.sha256((support.ROOT / name).read_bytes()).hexdigest()
                    for name in sources
                },
                "binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            }
        )
