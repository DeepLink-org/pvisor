"""Process Markdown profiles: inventory, preparation, execution and audit."""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import common as COMMON  # noqa: E402
from update import validate_batch  # noqa: E402

SEMSPEC = Path(os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec"))
SOURCE = ROOT / "process/smoke.md"


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


@unittest.skipUnless(SEMSPEC.is_file(), "built semspec required")
class ProcessSmokeContracts(unittest.TestCase):
    def test_markdown_is_the_only_source_and_preserves_existing_controls(self):
        config = COMMON.command_definition(SOURCE)
        previous = COMMON.command_definition(ROOT / "process/nightly.md")
        self.assertEqual((config["warmup"], config["runs"]), (2, 10))
        self.assertEqual(config["commands"], previous["commands"])
        self.assertEqual((previous["warmup"], previous["runs"]), (10, 50))
        canonical = json.dumps(previous, sort_keys=True, separators=(",", ":")).encode()
        self.assertEqual(
            hashlib.sha256(canonical).hexdigest(),
            "465f373ac167562e78a6f52e7e6aa965fd80539858851fc379ab2339994026ae",
        )
        self.assertFalse((ROOT / "process/nightly.json").exists())
        self.assertFalse((ROOT / "process/nightly.commands.json").exists())
        self.assertEqual(list(config["commands"]), ["native", "host-run", "status", "review"])
        self.assertEqual(list(COMMON.table_template(SOURCE)), list(config["commands"]))
        self.assertFalse(SOURCE.with_suffix(".json").exists())
        self.assertFalse(SOURCE.with_suffix(".commands.json").exists())
        self.assertEqual(COMMON.registered(SOURCE)["benchmark_id"], "B-PROCESS")
        self.assertEqual(COMMON.registered(SOURCE)["role"], "engineering A/B")

    def test_smoke_and_nightly_sampling_cannot_be_compared(self):
        compare = module("process_profile_compare", ROOT / "process/compare.py")
        from unittest.mock import patch

        with patch.object(
            compare,
            "read",
            side_effect=[
                ({"native": 1.0}, {"runs": 50, "warmup": 10}),
                ({"native": 1.0}, {"runs": 10, "warmup": 2}),
            ],
        ):
            with self.assertRaisesRegex(ValueError, "incompatible measurement configuration: runs"):
                compare.compare(Path("candidate/in.bench.json"), Path("baseline/in.bench.json"), 15)

    def test_inventory_is_read_only_and_rejects_inconsistent_sampling(self):
        with workspace() as (root, resources):
            source = root / "experiment.md"
            source.write_text(
                "# Experiment\n<!-- semspec: bench id=first -->\n```toml\n"
                'argv = ["touch", "executed"]\nruns = 2\n```\n'
            )
            config = COMMON.command_definition(source)
            self.assertEqual(config["commands"]["first"]["argv"], ["touch", "executed"])
            self.assertFalse((root / "executed").exists())
            source.write_text(
                source.read_text()
                + '<!-- semspec: bench id=second -->\n```toml\nargv = ["true"]\nruns = 3\n```\n'
            )
            with self.assertRaisesRegex(ValueError, "one runs/warmup"):
                COMMON.command_definition(source)
            source.write_text(source.read_text().replace("runs = 3", 'runs = "typo"'))
            with self.assertRaises(subprocess.CalledProcessError):
                COMMON.command_definition(source)
            self.assertFalse((root / "executed").exists())

    def test_complete_protocol_retains_all_trials_and_rejects_altered_evidence(self):
        # A protocol fixture proves data flow without claiming product performance.
        with workspace() as (root, resources):
            binary = root / "it's a fake pvisor"
            binary.write_text(
                "#!" + sys.executable + "\n"
                "import json,os,pathlib,sys,uuid\n"
                "if sys.argv[1:] == ['--help']:\n"
                " print('Commands:\\n  spawn  Run a Job')\n"
                "elif sys.argv[1] == 'spawn':\n"
                " d=pathlib.Path(os.environ['PVISOR_RUN_HOME'])/('run-'+uuid.uuid4().hex);d.mkdir(parents=True)\n"
                " (d/'run-bundle.json').write_text(json.dumps({'run':{'state':'completed','exit_code':0}}))\n"
                "elif sys.argv[1] in ('status','review'):\n"
                " print((pathlib.Path(sys.argv[-1])/'run-bundle.json').read_text())\n"
                "else: sys.exit(9)\n"
            )
            binary.chmod(0o700)
            prepare = module("process_smoke_prepare", ROOT / "process/prepare.py")
            batch = prepare.prepare_template(SOURCE, binary, root / ".data/measurement")
            manifest = json.loads((batch / "manifest.json").read_text())
            self.assertEqual((manifest["warmup"], manifest["runs"]), (2, 10))
            self.assertIn("process-smoke.md", manifest["harness"])
            self.assertIn("semspec-runtime.py", manifest["harness"])
            self.assertIn(str(SEMSPEC.resolve()), manifest["artifacts"])
            self.assertFalse(
                any(name.endswith("smoke.commands.json") for name in manifest["harness"])
            )
            subprocess.run(
                [str(SEMSPEC), "bench", "--cwd", manifest["cwd"], str(batch / "in.json")],
                check=True,
                capture_output=True,
                text=True,
            )
            manifest, table, results = validate_batch(batch)
            self.assertEqual(manifest["status"], "passed")
            self.assertEqual(set(results), {"native", "host-run", "status", "review"})
            self.assertEqual(len(list((batch / "command-trials").glob("*/*.json"))), 48)
            self.assertEqual(len(list((batch / "runs").glob("run-*/run-bundle.json"))), 13)
            for condition in results:
                self.assertEqual(table[condition]["N"], 10)
                for phase, count in (("warmup", 2), ("sample", 10)):
                    paths = sorted((batch / "command-trials" / condition).glob(phase + "-*.json"))
                    self.assertEqual(len(paths), count)
                    self.assertTrue(
                        all(json.loads(path.read_text())["exit_code"] == 0 for path in paths)
                    )
            compare = module("process_smoke_compare", ROOT / "process/compare.py")
            report, failed = compare.compare(batch / "in.bench.json", None, 15)
            self.assertEqual(failed, 0)
            self.assertIn("candidate-only", report)
            trial = batch / "command-trials/host-run/sample-0000.json"
            record = json.loads(trial.read_text())
            record["duration_ns"] += 1
            COMMON.save(trial, record)
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                validate_batch(batch)
