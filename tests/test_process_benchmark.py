"""Persistent benchmark fixtures keep Bundle validation across CLI layouts."""

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
PROCESS = ROOT / "benchmark/process"


def load_prepare():
    spec = importlib.util.spec_from_file_location("process_prepare", PROCESS / "prepare.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ProcessBenchmarkTests(unittest.TestCase):
    def test_selects_persistent_host_entry_without_selecting_legacy_vm_stage(self):
        prepare = load_prepare()
        for help_text, stage_help, expected in [
            ("  spawn  temporary\n  stage  persistent\n", "  --executor <EXECUTOR>\n", "stage"),
            ("  spawn  job\n  stage  host-root VM\n", "  --network <NETWORK>\n", "spawn"),
            ("  run  job\n", None, "run"),
        ]:
            with self.subTest(expected=expected), workspace() as (_, resources):
                replies = [subprocess.CompletedProcess([], 0, help_text)]
                if stage_help is not None:
                    replies.append(subprocess.CompletedProcess([], 0, stage_help))
                resources.enter_context(
                    mock.patch.object(prepare.subprocess, "run", side_effect=replies)
                )
                self.assertEqual(prepare.job_command(Path("/fixture/pvisor")), expected)

    def test_new_bundle_hook_accepts_both_layouts_but_requires_a_new_completed_bundle(self):
        for layout in ("run-one/run-bundle.json", "run-one/attempts/initial/run-bundle.json"):
            with self.subTest(layout=layout), workspace() as (root, _):
                bundle = root / layout
                bundle.parent.mkdir(parents=True)
                bundle.write_text(json.dumps({"run": {"state": "completed", "exit_code": 0}}))
                command = [sys.executable, str(PROCESS / "check.py"), "new-bundle", str(root)]
                first = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(first.returncode, 0, first.stderr)
                checked = (root / ".checked.json").read_bytes()
                repeated = subprocess.run(command, capture_output=True, text=True)
                self.assertNotEqual(repeated.returncode, 0)
                self.assertIn("exactly one new completed", repeated.stderr)
                self.assertEqual((root / ".checked.json").read_bytes(), checked)

                failed = root / "run-two/attempts/initial/run-bundle.json"
                failed.parent.mkdir(parents=True)
                failed.write_text(json.dumps({"run": {"state": "failed", "exit_code": 7}}))
                rejected = subprocess.run(command, capture_output=True, text=True)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("successful completion", rejected.stderr)
                self.assertEqual((root / ".checked.json").read_bytes(), checked)
