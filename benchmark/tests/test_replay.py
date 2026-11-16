"""Rust replay contracts and strict read-only historical publication audits."""

import copy
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from archive import (  # noqa: E402
    EXIT_FIELDS,
    READY_FIELDS,
    audit_command_row,
    save,
    summarize,
    summarize_pairs,
)


class ReplaySpecificationTests(unittest.TestCase):
    def test_native_replay_uses_registered_rust_contracts(self):
        source = (ROOT / "replay/replay.md").read_text()
        self.assertNotIn("<!--", source)
        self.assertIn("Criterion", source)
        module = ROOT / "criterion/src/replay"
        self.assertIn("pub fn register(criterion: &mut Criterion)", (module / "mod.rs").read_text())
        contracts = (module / "tests.rs").read_text()
        for test in (
            "native_fixture_documents_match_original_python_contract",
            "fidelity_rejects_wrong_source_adapter_boundary_and_arguments",
            "reset_guards_source_workspace_and_unaudited_outputs",
            "result_checks_reject_tool_execution_agent_start_missing_and_duplicate_results",
            "native_cli_all_seven",
        ):
            self.assertIn("fn " + test + "(", contracts)

    def test_display_and_archive_imports_never_start_processes(self):
        # A fresh process prevents pre-imported common/update from masking the
        # dependency. Importing display/audit helpers must have no process effects.
        script = """
import subprocess
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
def forbidden(*args, **kwargs):
    raise AssertionError('display/audit import started a process')
subprocess.Popen = forbidden
import common
import update
import docs
assert common.save.__module__ == 'archive'
assert update.audit_command_row.__module__ == 'archive'
"""
        subprocess.run([sys.executable, "-I", "-c", script, str(ROOT)], check=True)

    def test_new_clock_cannot_overwrite_historical_public_results(self):
        import common

        spec = importlib.util.spec_from_file_location("replay_documentation_gate", ROOT / "docs.py")
        docs = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(docs)
        inventory = json.loads((ROOT / "docs.json").read_text())
        self.assertEqual(
            inventory["bindings"]["replay/replay.md"]["metric_scope"],
            "direct subject command; hyperfine hooks and semspec excluded",
        )
        for scope in (common.METRIC_SCOPE, "direct Rust command invocation; Criterion"):
            with self.subTest(scope=scope), tempfile.TemporaryDirectory() as temporary:
                batch = Path(temporary)
                save(
                    batch / "manifest.json",
                    dict(
                        entry="replay/replay.md",
                        benchmark_id="B-REPLAY",
                        role="user-facing",
                        metric_scope=scope,
                    ),
                )
                with patch.object(docs, "validate_batch") as validate:
                    with self.assertRaisesRegex(ValueError, "measurement scope differs"):
                        docs.publish_docs(batch)
                    validate.assert_not_called()


class HistoricalCommandAuditTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.batch = Path(self.temporary.name)
        self.config = dict(
            argv=["/frozen/pvisor-replay", "--prepare-only"],
            env={"FIXTURE": "0"},
            prepare=["/frozen/hook", "prepare"],
            conclude=["/frozen/hook", "conclude"],
            cleanup=["/frozen/hook", "cleanup"],
            expected_stdout="prepared\n",
        )
        self.manifest = dict(warmup=1, runs=2)
        self.result = summarize([1_000_000, 3_000_000])
        self.record = dict(raw=self.result, value=self.result)
        save(self.batch / "commands.json", {"adapter": self.config})
        self.paths = []
        for phase, index, duration in (
            ("warmup", 0, 2_000_000),
            ("sample", 0, 1_000_000),
            ("sample", 1, 3_000_000),
        ):
            path = self.batch / "command-trials/adapter" / f"{phase}-{index:04d}.json"
            self.paths.append(path)
            save(
                path,
                dict(
                    condition="adapter",
                    phase=phase,
                    index=index,
                    command=self.config["argv"],
                    environment=self.config["env"],
                    exit_code=0,
                    start_ns=100,
                    end_ns=100 + duration,
                    duration_ns=duration,
                    stdout="prepared\n",
                    stderr="",
                    hooks=[
                        dict(action=action, command=self.config[action], exit_code=0)
                        for action in ("prepare", "conclude", "cleanup")
                    ],
                ),
            )
        save(
            self.batch / "adapter.samples.json",
            dict(
                engine="python-perf_counter_ns",
                unit="nanosecond",
                samples=[1_000_000, 3_000_000],
                result=self.result,
            ),
        )

    def audit(self):
        return audit_command_row(self.batch, self.manifest, "adapter", self.record, self.result)

    def test_original_statistics_and_complete_evidence_pass_without_execution(self):
        self.assertEqual(self.result["N"], 2)
        self.assertEqual(self.result["Median ms"], 2)
        self.assertEqual(self.result["Mean ms"], 2)
        self.assertAlmostEqual(self.result["SD ms"], 2**0.5)
        self.assertEqual(self.result["Min ms"], 1)
        self.assertEqual(self.result["Max ms"], 3)
        # Recorded commands deliberately do not exist; only evidence is read.
        self.assertEqual(self.audit()["statistics"], self.result)

    def test_every_warmup_and_sample_is_checked(self):
        for path in self.paths:
            original = json.loads(path.read_text())
            for key, bad in (
                ("condition", "other"),
                ("phase", "other"),
                ("index", 42),
                ("command", ["/altered"]),
                ("environment", {}),
                ("exit_code", 1),
                ("start_ns", 1.0),
                ("end_ns", 2),
                ("duration_ns", 0),
                ("error", "failed"),
                ("cleanup_error", "failed"),
                ("stdout", "wrong"),
                ("hooks", []),
            ):
                with self.subTest(path=path.name, key=key):
                    bad_record = original | {key: bad}
                    save(path, bad_record)
                    with self.assertRaises(ValueError):
                        self.audit()
                    save(path, original)
            for action_index in range(3):
                for key, bad in (("command", ["/altered"]), ("exit_code", 1)):
                    with self.subTest(path=path.name, action=action_index, key=key):
                        bad_record = copy.deepcopy(original)
                        bad_record["hooks"][action_index][key] = bad
                        save(path, bad_record)
                        with self.assertRaises(ValueError):
                            self.audit()
                        save(path, original)

    def test_missing_extra_trials_and_derived_values_are_rejected(self):
        path = self.paths[0]
        original = json.loads(path.read_text())
        path.unlink()
        with self.assertRaisesRegex(ValueError, "missing or extra"):
            self.audit()
        save(path, original)
        extra = path.with_name("sample-0042.json")
        save(extra, original)
        with self.assertRaisesRegex(ValueError, "missing or extra"):
            self.audit()
        extra.unlink()
        samples_path = self.batch / "adapter.samples.json"
        samples = json.loads(samples_path.read_text())
        for key, bad in (
            ("engine", "criterion"),
            ("unit", "millisecond"),
            ("samples", [1_000_000, 3_000_001]),
            ("result", self.result | {"N": 3}),
        ):
            with self.subTest(key=key):
                save(samples_path, samples | {key: bad})
                with self.assertRaisesRegex(ValueError, "resolved row or samples"):
                    self.audit()
                save(samples_path, samples)
        for key in ("raw", "value"):
            self.record[key] = self.result | {"Mean ms": 42}
            with self.assertRaisesRegex(ValueError, "resolved row or samples"):
                self.audit()
            self.record[key] = self.result
        self.result = dict(reversed(list(self.result.items())))
        with self.assertRaisesRegex(ValueError, "resolved row or samples"):
            self.audit()

    def test_invalid_duration_arrays_remain_rejected(self):
        for samples in ([], [1], [1, True], [1, 0], [1, -1], [1, 2.0]):
            with self.subTest(samples=samples), self.assertRaises(ValueError):
                summarize(samples)

    def test_paired_event_statistics_keep_historical_fields_and_validation(self):
        values = summarize_pairs(
            [
                dict(ready_ns=1_000_000, exit_ns=2_000_000),
                dict(ready_ns=3_000_000, exit_ns=5_000_000),
            ]
        )
        self.assertEqual(list(values), list(READY_FIELDS) + list(EXIT_FIELDS)[1:])
        self.assertEqual(values["Ready median ms"], 2)
        self.assertEqual(values["Exit median ms"], 3.5)
        for invalid in (
            dict(ready_ns=0, exit_ns=1),
            dict(ready_ns=2, exit_ns=1),
            dict(ready_ns=True, exit_ns=2),
            dict(ready_ns=1, exit_ns=2.0),
            dict(ready_ns=1, exit_ns=2, extra=3),
        ):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                summarize_pairs([invalid, dict(ready_ns=1, exit_ns=2)])


if __name__ == "__main__":
    unittest.main()
