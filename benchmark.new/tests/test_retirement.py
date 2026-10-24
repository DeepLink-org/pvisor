"""Retired campaigns must not be needed by current frozen image adapters."""

import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class RetiredHarnessTests(unittest.TestCase):
    def test_frozen_image_helpers_import_with_only_their_retained_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = root / "frozen/harness"
            harness.mkdir(parents=True)
            for name in ("lazy_image_v2.py", "lazy_startup.py"):
                shutil.copy2(ROOT / "support" / name, harness / name)
            code = """
import json, sys
sys.path[:] = [sys.argv[1], *[p for p in sys.path if p]]
import lazy_image_v2, lazy_startup
assert not hasattr(lazy_image_v2, 'main')
assert not hasattr(lazy_startup, 'main')
assert lazy_image_v2.prepared_store_manifest(__import__('pathlib').Path(sys.argv[2])) == {}
print(json.dumps(lazy_startup.workload('ubuntu-shell')[1].decode()))
"""
            empty = root / "empty"
            empty.mkdir()
            result = subprocess.run(
                [sys.executable, "-I", "-c", code, str(harness), str(empty)],
                cwd=root,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(json.loads(result.stdout), "LAZY_READY ubuntu 26.04\n")

    def test_anchor_is_required_only_when_preparing_a_new_cli_cohort(self):
        for name in ("curves_cli", "guest_writeback_cli"):
            with self.subTest(name=name):
                spec = importlib.util.spec_from_file_location(
                    "retirement_" + name, ROOT / "filesystem" / (name + ".py")
                )
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                with (
                    patch.object(sys, "argv", [name, "--prepare", "/unused", "--setup", "/unused"]),
                    self.assertRaisesRegex(ValueError, "explicit --template"),
                ):
                    module.main()
                with (
                    patch.object(sys, "argv", [name, "--config", "/unused", "--check"]),
                    patch.object(module, "load", return_value={}),
                    patch.object(module, "verify", return_value={}) as verify,
                    patch("builtins.print"),
                ):
                    module.main()
                    verify.assert_called_once()
