"""Disk inventories must neither double-count links nor hide unreadable trees."""

import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from tests.fixtures import workspace

SCRIPT = Path(__file__).resolve().parents[1] / "storage/native_docker.py"
SPEC = importlib.util.spec_from_file_location("native_docker_contract", SCRIPT)
SUBJECT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECT)


class InventoryContracts(unittest.TestCase):
    def test_links_count_inode_once_and_never_follow_outside_store(self):
        with workspace() as (root, _):
            tree = root / "tree"
            subdir = tree / "nested"
            subdir.mkdir(parents=True)
            (root / "outside").write_bytes(b"outside" * 4096)
            payload = tree / "payload"
            payload.write_bytes(b"contents")
            os.link(payload, subdir / "alias")
            link = tree / "link"
            link.symlink_to(root / "outside")
            result = subprocess.run(
                [sys.executable, "-c", SUBJECT.SCANNER, str(tree)],
                capture_output=True,
                text=True,
                check=True,
            )
            inventory = json.loads(result.stdout)
            self.assertEqual(inventory["inodes"], 4)
            expected = [tree.lstat(), subdir.lstat(), payload.lstat(), link.lstat()]
            self.assertEqual(inventory["apparent"], sum(s.st_size for s in expected))
            self.assertEqual(inventory["allocated"], sum(s.st_blocks * 512 for s in expected))
            self.assertNotIn("outside", {row["path"] for row in inventory["rows"]})

    @unittest.skipIf(os.geteuid() == 0, "root bypasses the unreadable-directory precondition")
    def test_unreadable_subtree_fails_instead_of_reporting_partial_capacity(self):
        with workspace() as (root, _):
            locked = root / "locked"
            locked.mkdir()
            (locked / "hidden").write_bytes(b"must not silently omit")
            locked.chmod(0)
            try:
                result = subprocess.run(
                    [sys.executable, "-c", SUBJECT.SCANNER, str(root)],
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("PermissionError", result.stderr)
                self.assertEqual(result.stdout, "")
            finally:
                locked.chmod(0o700)
