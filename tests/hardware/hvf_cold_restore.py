"""Single-vCPU HVF CPU/RAM contracts; no Linux or complete-VM restore claim."""

import copy
import json
import os
import platform
import subprocess
import unittest

from tests.fixtures import workspace
from tests.hardware import support


class HvfColdRestoreTests(unittest.TestCase):
    def setUp(self):
        support.require_hvf()
        self.binary = support.binary()

    def test_save_restore_and_reject_invalid_snapshots(self):
        checks = []

        def run(name, mode, path, expected_error=None):
            result = subprocess.run(
                [str(self.binary), mode, str(path)],
                capture_output=True,
                text=True,
                timeout=20,
            )
            support.assert_response(self, result, expected_error)
            checks.append(
                {
                    "name": name,
                    "exit_code": result.returncode,
                    "stdout": result.stdout.strip(),
                    "stderr": result.stderr.strip(),
                }
            )

        with workspace() as (directory, _):
            for index in range(3):
                with self.subTest(round=index):
                    path = directory / f"snapshot-{index}.json"
                    run(f"save-{index}", "save", path)
                    run(f"restore-{index}", "restore", path)
            source = path.read_bytes()
            run("refuse-overwrite", "save", path, "File exists")
            self.assertEqual(path.read_bytes(), source, "failed save modified existing snapshot")
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
                    lambda s: s["cpu"]["features"][0].__setitem__(
                        1, s["cpu"]["features"][0][1] ^ 1
                    ),
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
                with self.subTest(corruption=name):
                    changed = copy.deepcopy(image)
                    mutate(changed)
                    bad = directory / f"{name}.json"
                    bad.write_text(json.dumps(changed))
                    run(name, "restore", bad, error)
            truncated = directory / "truncated.json"
            truncated.write_bytes(source[: len(source) // 2])
            run("truncated", "restore", truncated, "EOF")
        support.write_evidence(
            {
                "scope": "M0 single-vCPU bare-metal HVF CPU/RAM; no Linux, GIC, virtio or filesystem",
                "host": platform.platform(),
                "page_bytes": os.sysconf("SC_PAGE_SIZE"),
                "checks": checks,
            }
        )
