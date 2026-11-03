"""Offline coverage for isolated CLI configuration and strict profile inventory."""

import importlib.util
import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "logical_directory_ab", Path(__file__).with_name("logical_directory_ab.py")
)
SUBJECT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECT)


class LogicalDirectoryContracts(unittest.TestCase):
    def test_macos_rejects_linux_only_data_before_creating_batch(self):
        with patch.object(SUBJECT.sys, "platform", "darwin"):
            with self.assertRaisesRegex(ValueError, "unsupported on macOS"):
                SUBJECT.prepare(SimpleNamespace(experiment="logical-data"))

    def test_each_followup_changes_only_its_requested_mechanism(self):
        for experiment, variable in [
            ("lazy-nodes", "PVISOR_VM_FS_REUSE_LAZY_NODES"),
            ("read-sink", "PVISOR_VM_FS_STREAM_LAZY_READS"),
            ("logical-data", "PVISOR_VM_FS_LOGICAL_DATA_CACHE"),
            ("direct-copy", "PVISOR_VM_FS_DIRECT_LAZY_COPY"),
        ]:
            left = SUBJECT.environment(Path("/owned/batch"), 0, experiment=experiment)
            right = SUBJECT.environment(Path("/owned/batch"), 1, experiment=experiment)
            delta = [(a, b) for a, b in zip(left, right, strict=True) if a != b]
            self.assertEqual(delta, [(f"{variable}=0", f"{variable}=1")])
            self.assertIn("PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE=1", left)

    def test_only_logical_cache_varies_between_timed_arms(self):
        left = SUBJECT.environment(Path("/owned/batch"), 0)
        right = SUBJECT.environment(Path("/owned/batch"), 1)
        differences = [(a, b) for a, b in zip(left, right, strict=True) if a != b]
        self.assertEqual(
            differences,
            [("PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE=0", "PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE=1")],
        )
        self.assertIn("-i", left)
        self.assertIn("PVISOR_VM_FS_HOST_WRITEBACK=0", left)

    def test_profile_inventory_rejects_missing_extra_and_duplicate_finals(self):
        item = {"pid": 12, "component": "test", "instance": 1}
        start = "pvisor-fs-profile-start " + json.dumps(item)
        final = "pvisor-fs-profile " + json.dumps({**item, "final_record": True})
        self.assertEqual(len(SUBJECT.profile_inventory([start, final])), 1)
        for lines in [[], [start], [final], [start, final, final]]:
            with self.assertRaises(ValueError):
                SUBJECT.profile_inventory(lines)
        extra = "pvisor-fs-profile " + json.dumps({**item, "instance": 2, "final_record": True})
        with self.assertRaises(ValueError):
            SUBJECT.profile_inventory([start, final, extra])


if __name__ == "__main__":
    unittest.main()
