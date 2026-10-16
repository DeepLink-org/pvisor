"""Table execution publishes only complete audited results and owns temporary evidence."""

import importlib.util
import json
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location("benchmark_run", ROOT / "run.py")
RUN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN)


class TableRunnerContracts(unittest.TestCase):
    def execute(self, root, fail=False, output=None, row=None):
        template = root / "startup/timing.json"
        RUN.save(template, {"Docker": "$ docker", "Firecracker": "$ fc"})
        rendered = template.with_suffix(".render.json")
        previous = {"Docker": {"N": 12}, "Firecracker": {"N": 13}, "obsolete": {"N": 60}}
        RUN.save(rendered, previous)
        batches = []

        def prepare(template, destination, *args, row=None):
            destination.mkdir(parents=True)
            RUN.save(destination / "manifest.json", {"cwd": str(destination), "row": row})
            batches.append(destination)
            return destination

        def audit(batch):
            row = json.loads((batch / "manifest.json").read_text())["row"]
            if fail and row == "Firecracker":
                raise ValueError("correctness failed")
            return {}, {row: {"N": 60, "Ready median ms": 1.234}}, {}

        with (
            patch.object(RUN, "registered", return_value={"status": "prepared"}),
            patch.object(
                RUN, "load_prepare", return_value=SimpleNamespace(prepare_template=prepare)
            ),
            patch.object(RUN, "validate_batch", side_effect=audit),
            patch.object(RUN.subprocess, "run"),
        ):
            if fail:
                with self.assertRaisesRegex(ValueError, "correctness failed"):
                    RUN.run_table(template, "/semspec", output=output, row=row)
                self.assertEqual(json.loads(rendered.read_text()), previous)
            else:
                RUN.run_table(template, "/semspec", output=output, row=row)
                self.assertEqual(
                    json.loads(rendered.read_text()),
                    {
                        name: {"N": 60, "Ready median ms": "1.23"}
                        if row is None or name == RUN.resolve_row(template, row)
                        else previous[name]
                        for name in ("Docker", "Firecracker")
                    },
                )
        return batches

    def test_multirow_success_and_failure_clean_temporary_evidence(self):
        for fail in (False, True):
            with self.subTest(fail=fail), workspace() as (root, resources):
                batches = self.execute(root, fail=fail)
                self.assertEqual(len(batches), 2)
                self.assertTrue(all(not batch.exists() for batch in batches))

    def test_explicit_output_retains_failed_evidence(self):
        with workspace() as (root, resources):
            batches = self.execute(root, fail=True, output=root / ".data/retained")
            self.assertTrue(all(batch.exists() for batch in batches))

    def test_single_row_preserves_other_rows_and_failure_preserves_table(self):
        for fail in (False, True):
            with self.subTest(fail=fail), workspace() as (root, resources):
                batches = self.execute(root, fail=fail, row=" firecracker ")
                self.assertEqual(len(batches), 1)
                self.assertTrue(all(not batch.exists() for batch in batches))

    def test_selection_accepts_positional_row_and_keeps_multiple_tables(self):
        templates, row = RUN.resolve_selection(["startup/timing", "  pvisor   vm "])
        self.assertEqual(templates, [ROOT / "startup/timing.json"])
        self.assertEqual(row, "pVisor VM")
        self.assertEqual(
            RUN.resolve_selection(["filesystem/tools"], "NATIVE-READ"),
            ([ROOT / "filesystem/tools.json"], "native-read"),
        )
        templates, row = RUN.resolve_selection(["startup/timing", "filesystem/tools"])
        self.assertEqual(len(templates), 2)
        self.assertIsNone(row)
        for values, selected in (
            (["startup/timing", "missing"], None),
            (["startup/timing", "filesystem/tools"], "Docker"),
        ):
            with self.subTest(values=values), self.assertRaises(ValueError):
                RUN.resolve_selection(values, selected)

    def test_unknown_or_ambiguous_row_does_not_prepare(self):
        with workspace() as (root, resources):
            template = root / "startup/timing.json"
            RUN.save(template, {"VM": "$ vm", "vm": "$ other"})
            self.assertEqual(RUN.resolve_row(template, "VM"), "VM")
            with (
                patch.object(RUN, "registered", return_value={"status": "prepared"}),
                patch.object(RUN, "load_prepare") as prepare,
            ):
                for value in ("missing", "Vm"):
                    with self.assertRaisesRegex(ValueError, "available: VM, vm"):
                        RUN.run_table(template, "/semspec", row=value)
                prepare.assert_not_called()

    def test_entry_resolution_rejects_results_and_unknown_names(self):
        self.assertEqual(RUN.resolve_entry("startup/timing"), ROOT / "startup/timing.json")
        self.assertEqual(RUN.resolve_entry("docker_timing"), ROOT / "startup/docker_timing.json")
        for value in ("missing", "startup/timing.render"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                RUN.resolve_entry(value)
