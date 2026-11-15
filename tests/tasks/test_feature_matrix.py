"""Capability declarations must not turn CLI-only failures into hardware failures."""

import runpy
import tempfile
import unittest
from pathlib import Path

DISCOVER = runpy.run_path(
    str(Path(__file__).resolve().parents[2] / "scripts/tasks/feature-matrix.py")
)["discover"]


class FeatureMatrixTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)

    def add(self, feature, identifier="S-NEW-001"):
        directory = self.root / feature
        directory.mkdir(exist_ok=True)
        (directory / f"{identifier}.md").touch()

    def test_native_discovery_has_no_feature_or_id_allowlist(self):
        self.add("new-feature")
        self.add("config", "S-CONFIG-999")
        self.add("config", "S-CONFIG-1000")
        (self.root / "fixtures").mkdir()
        self.assertEqual(
            [row["feature"] for row in DISCOVER(self.root)],
            ["config", "new-feature"],
        )

    def test_cli_features_have_no_hardware_prerequisites(self):
        for feature in ("config", "docker-cli", "docker-compose", "replay", "session"):
            self.add(feature)
        for row in DISCOVER(self.root):
            self.assertFalse(any(value for key, value in row.items() if key != "feature"))

    def test_hardware_features_keep_their_required_capabilities(self):
        expected = {
            "checkpoint": {"userns", "fuse"},
            "container": {"userns", "oci"},
            "gateway": {"userns", "fuse"},
            "isolation": {"userns", "fuse"},
            "network": {"userns", "kvm"},
            "resources": {"userns", "fuse"},
            "stage": {"userns", "fuse"},
            "vm": {"userns", "fuse", "kvm", "vm_sdk"},
        }
        for feature in expected:
            self.add(feature)
        for row in DISCOVER(self.root):
            self.assertEqual(
                {key for key, value in row.items() if key != "feature" and value},
                expected[row["feature"]],
            )

    def test_empty_inventory_refuses(self):
        with self.assertRaisesRegex(ValueError, "empty"):
            DISCOVER(self.root)
