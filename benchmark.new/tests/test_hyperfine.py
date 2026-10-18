"""JSON registry, one-run/many-column extraction, audit and publication contracts."""

import csv
import importlib.util
import io
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.append(str(ROOT / "startup"))
import common as COMMON  # noqa: E402
from update import display_table, publish, validate_batch  # noqa: E402

SEMSPEC = Path(os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec"))
LIVE = shutil.which("hyperfine") and SEMSPEC.is_file()


class RegistryTests(unittest.TestCase):
    def test_every_active_id_has_a_json_template_and_retired_ids_do_not(self):
        registry = (ROOT.parent / "benchmark/README.md").read_text()
        expected = {
            line.split("：")[0].removeprefix("### ")
            for line in registry.splitlines()
            if line.startswith("### B-")
        }
        expected -= {"B-OVERVIEW / B-METHOD", "B-COMPARE", "B-CLUSTER"}
        with (ROOT / "registry.csv").open() as stream:
            rows = list(csv.DictReader(stream))
        self.assertEqual({row["benchmark_id"] for row in rows}, expected)
        for row in rows:
            path = ROOT / row["entry"]
            self.assertEqual(path.suffix, ".json")
            table = json.loads(path.read_text())
            self.assertTrue(table)
            if row["status"].startswith("prepared-python-ready"):
                from subjects import LABELS

                self.assertEqual(row["benchmark_id"], "B-STARTUP")
                backends = {label: backend for backend, label in LABELS.items()}
                for label, command in table.items():
                    backend = backends[label]
                    script = backend.replace("-", "_") + "_timing"
                    self.assertEqual(
                        command, f"$ python3 @REPO@/benchmark.new/startup/{script}.py @BATCH@"
                    )
                    self.assertTrue(path.with_name(script + ".py").is_file())
                continue
            if row["status"] == "prepared-python-filesystem":
                self.assertEqual(row["benchmark_id"], "B-FS-TOOLS")
                self.assertEqual(
                    table,
                    {
                        "native-"
                        + mode: f"$ python3 @REPO@/benchmark.new/filesystem/{mode}_timing.py @BATCH@"
                        for mode in ("metadata", "read")
                    },
                )
                continue
            if row["status"] == "explicit-python-paired-dax":
                self.assertEqual(row["benchmark_id"], "B-FS-ENG")
                self.assertEqual(row["role"], "engineering A/B")
                self.assertEqual(row["entry"], "filesystem/dax_ab.json")
                self.assertEqual(
                    table,
                    {"immutable-share-paired": "$ python3 @HARNESS@/dax_ab.py --measure @BATCH@"},
                )
                self.assertTrue(path.with_suffix(".py").is_file())
                continue
            if row["status"] == "explicit-python-paired-memory":
                self.assertEqual(row["benchmark_id"], "B-MEMORY-SCALE")
                self.assertEqual(row["role"], "engineering A/B")
                self.assertEqual(row["entry"], "memory/scanner_compare.json")
                self.assertEqual(
                    table,
                    {
                        "paired-memory": "$ python3 @REPO@/benchmark.new/memory/scanner_compare.py --build @BUILD@ "
                        "--rootfs @ROOTFS@ --firmware @FIRMWARE@ --output @OUTPUT@ --samples @SAMPLES@"
                    },
                )
                self.assertTrue(path.with_suffix(".py").is_file())
                continue
            existing_python_tables = {
                "explicit-python-paired-prepared-images": (
                    "B-LAZY-STARTUP",
                    "startup/image_compare.json",
                    "warm-prepared-paired",
                    "$ python3 @HARNESS@/image_compare.py --measure @BATCH@",
                ),
                "explicit-python-paired-cold-warm-images": (
                    "B-LAZY-STARTUP",
                    "startup/image_lazy_compare.json",
                    "cold-warm-client-paired",
                    "$ python3 @HARNESS@/image_lazy_compare.py --measure @BATCH@",
                ),
                "explicit-python-paired-same-staged-executor": (
                    "B-LAZY-STARTUP",
                    "startup/stage_lazy_compare.json",
                    "same-stage-cold-warm-paired",
                    "$ python3 @HARNESS@/stage_lazy_compare.py --measure @BATCH@",
                ),
                "explicit-python-layered-nine-mode-cross-runtime": (
                    "B-FS-TOOLS",
                    "filesystem/user_workloads.json",
                    "filesystem-user-layered",
                    "$ python3 @BATCH@/harness/user_workloads.py --measure @BATCH@",
                ),
            }
            if row["status"] == "explicit-python-paired-pool-curves":
                self.assertEqual(row["benchmark_id"], "B-MEMORY-SCALE")
                self.assertEqual(row["role"], "engineering A/B")
                self.assertEqual(row["entry"], "memory/pool_compare.json")
                self.assertEqual(
                    table,
                    {
                        "pool-curves": "$ python3 @REPO@/benchmark.new/memory/pool_compare.py "
                        "--inputs @INPUTS@ --docker-inputs @DOCKER_INPUTS@ --output @OUTPUT@ --samples 5"
                    },
                )
                self.assertTrue(path.with_suffix(".py").is_file())
                continue
            if row["status"] == "explicit-python-paired-library-curves":
                self.assertEqual(row["benchmark_id"], "B-MEMORY-SCALE")
                self.assertEqual(row["role"], "engineering A/B")
                self.assertEqual(row["entry"], "memory/library_compare.json")
                self.assertEqual(
                    table,
                    {
                        "library-curves": "$ python3 @REPO@/benchmark.new/memory/library_compare.py "
                        "--inputs @INPUTS@ --docker-inputs @DOCKER_INPUTS@ --output @OUTPUT@ --samples @SAMPLES@"
                    },
                )
                self.assertTrue(path.with_suffix(".py").is_file())
                continue
            if row["status"] in existing_python_tables:
                benchmark_id, entry, label, command = existing_python_tables[row["status"]]
                self.assertEqual(row["benchmark_id"], benchmark_id)
                self.assertEqual(row["role"], "user-facing")
                self.assertEqual(row["entry"], entry)
                self.assertEqual(table, {label: command})
                self.assertTrue(path.with_suffix(".py").is_file())
                continue
            if isinstance(next(iter(table.values())), str):
                for command in table.values():
                    self.assertTrue(command.startswith("$ hyperfine "))
                    argv = shlex.split(command[2:])
                    self.assertNotIn("|", argv)
                    self.assertIn("--runs", argv)
                    self.assertIn("--warmup", argv)
                continue
            first = set(next(iter(table.values())))
            for cells in table.values():
                self.assertEqual(first, set(cells))
                for value in cells.values():
                    self.assertFalse(isinstance(value, (dict, list)))
                    if isinstance(value, str) and value.startswith("$ "):
                        self.assertEqual(shlex.split(value[2:])[0], "hyperfine")
                    if row["status"] == "requires-single-condition-command":
                        self.assertIsNone(value)

    def test_no_executable_template_duplicates_commands_across_statistic_cells(self):
        with (ROOT / "registry.csv").open() as stream:
            entries = list(csv.DictReader(stream))
        for entry in entries:
            table = json.loads((ROOT / entry["entry"]).read_text())
            if entry["status"] == "requires-single-condition-command":
                self.assertTrue(
                    all(
                        isinstance(row, dict) and all(value is None for value in row.values())
                        for row in table.values()
                    )
                )
                with self.assertRaisesRegex(ValueError, "no prepared single-condition command"):
                    COMMON.materialize(ROOT / entry["entry"], ROOT / ".data/never-created")
            else:
                self.assertTrue(
                    all(isinstance(row, str) and row.startswith("$ ") for row in table.values()),
                    entry["entry"],
                )

    def test_path_substitution_preserves_nested_subject_and_hook_quoting(self):
        variables = {"BINARY": "/a path/it's-pvisor", "BATCH": "/data/a path"}
        expression = "$ " + shlex.join(
            [
                "hyperfine",
                "--runs",
                "2",
                "--warmup",
                "0",
                "--prepare",
                "python3 @BINARY@ @BATCH@/input",
                "--command-name",
                "test",
                "@BINARY@ run @BATCH@/input",
            ]
        )
        argv = shlex.split(COMMON.expand(expression, variables)[2:])
        self.assertEqual(
            shlex.split(argv[argv.index("--prepare") + 1]),
            ["python3", variables["BINARY"], variables["BATCH"] + "/input"],
        )
        self.assertEqual(
            shlex.split(argv[-1]),
            [variables["BINARY"], "run", variables["BATCH"] + "/input"],
        )

    def test_display_format_changes_only_presentation(self):
        table = {"native": {"N": 60, "Median ms": 1.234567, "note": "unchanged", "Min ms": None}}
        self.assertEqual(
            display_table(table),
            {"native": {"N": 60, "Median ms": "1.23", "note": "unchanged", "Min ms": None}},
        )
        self.assertEqual(table["native"]["Median ms"], 1.234567)

    def test_preparation_rejects_legacy_cell_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            entry = Path(directory) / "fixture.json"
            command = "$ hyperfine --runs 2 --warmup 0 --command-name checked /bin/true"
            for table in (
                {"checked": {"N": command}},
                {"checked": command + " | json /results/0/count * 1"},
            ):
                entry.write_text(json.dumps(table))
                with patch.object(COMMON, "registered", return_value={}):
                    with self.assertRaisesRegex(ValueError, "per row|obsolete"):
                        COMMON.materialize(entry, Path(directory) / "batch")


class RowEvidenceTests(unittest.TestCase):
    """Audit row data even on hosts without hyperfine installed."""

    def test_valid_row_and_invalid_measurements_or_mapping(self):
        from update import FIELDS

        with tempfile.TemporaryDirectory() as directory:
            batch = Path(directory).resolve()
            command = "hyperfine --runs 2 --warmup 1 --command-name checked /bin/true"
            summary = {field: 2 if field == "count" else 0.001 for field in FIELDS.values()}
            summary["unit"] = "second"
            resolved = {
                "checked": {
                    field: summary[key] * (1 if field == "N" else 1000)
                    for field, key in FIELDS.items()
                }
            }
            template = {"checked": "$ " + command}
            COMMON.save(batch / "in.json", template)
            manifest = dict(
                status="prepared",
                metric_scope=COMMON.METRIC_SCOPE,
                benchmark_id="B-PROCESS",
                entry="process/fixture.json",
                input_sha256=COMMON.digest(batch / "in.json"),
                artifacts={},
                harness={},
                cwd=str(batch),
                runs=2,
                warmup=1,
                rows={"checked": "checked"},
                conditions=["checked"],
                hyperfine_version="hyperfine 2.0.0",
            )
            evidence = dict(
                format=1,
                cwd=str(batch),
                template=str(batch / "in.json"),
                cells=[],
                commands=[
                    dict(
                        command=command,
                        kind="hyperfine-row",
                        value=resolved["checked"],
                        raw=dict(
                            schema_version=2,
                            metadata={"hyperfine_version": "2.0.0"},
                            results=[
                                dict(
                                    name="checked",
                                    command="/bin/true",
                                    measurements=[{"exit_code": 0}, {"exit_code": 0}],
                                    summary={"time_wall_clock": summary},
                                )
                            ],
                        ),
                    )
                ],
                rows=[
                    dict(row="checked", command=command, expression=command, columns=list(FIELDS))
                ],
            )
            COMMON.save(batch / "in.render.json", resolved)
            for damage in (None, "exit", "statistic", "mapping", "cell"):
                with self.subTest(damage=damage):
                    changed = json.loads(json.dumps(evidence))
                    result = changed["commands"][0]["raw"]["results"][0]
                    if damage == "exit":
                        result["measurements"][0]["exit_code"] = 7
                    elif damage == "statistic":
                        result["summary"]["time_wall_clock"]["median"] = 9
                    elif damage == "mapping":
                        changed["rows"][0]["row"] = "other"
                    elif damage == "cell":
                        changed["cells"] = [{"row": "checked", "column": "N"}]
                    COMMON.save(batch / "manifest.json", manifest)
                    COMMON.save(batch / "in.bench.json", changed)
                    if damage:
                        with self.assertRaises(ValueError):
                            validate_batch(batch)
                    else:
                        checked, table, results = validate_batch(batch)
                        self.assertEqual(checked["status"], "passed")
                        self.assertEqual(table, resolved)
                        self.assertEqual(set(results), {"checked"})


@unittest.skipUnless(LIVE, "real hyperfine 2.0+ and built semspec required")
class HyperfineTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="semspec benchmark ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.entry = self.root / "fixture.json"
        self.batch = self.root / ".data" / "batch"
        self.csv = self.root / "results.csv"
        self.md = self.root / "results.md"

    def measure(self, code="pass", extra=(), artifacts=(), names=("checked",)):
        argv = [
            "hyperfine",
            "--shell=none",
            "--runs",
            "2",
            "--warmup",
            "1",
            "--style",
            "basic",
            *extra,
        ]
        table = {
            name: "$ "
            + shlex.join([*argv, "--command-name", name, shlex.join([sys.executable, "-c", code])])
            for name in names
        }
        self.entry.write_text(json.dumps(table))
        metadata = dict(
            benchmark_id="B-PROCESS",
            role="engineering A/B",
            entry="process/smoke.json" if len(names) == 4 else "process/fixture.json",
        )
        with patch.object(COMMON, "registered", return_value=metadata):
            batch = COMMON.create_batch(self.entry, self.batch)
            COMMON.materialize(self.entry, batch, artifacts=artifacts, cwd=self.root)
        return subprocess.run(
            [str(SEMSPEC), "bench", "--cwd", str(self.root), str(batch / "in.json")],
            capture_output=True,
            text=True,
        )

    def test_all_statistics_share_one_export_and_csv_uses_exact_selected_values(self):
        calls = self.root / "calls"
        code = f'from pathlib import Path; p=Path({str(calls)!r}); p.write_text(p.read_text()+"run\\n" if p.exists() else "run\\n")'
        result = self.measure(code)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls.read_text(), "run\n" * 3)
        manifest, resolved, results = validate_batch(self.batch)
        evidence = json.loads((self.batch / "in.bench.json").read_text())
        self.assertEqual(len(evidence["commands"]), 1)
        self.assertEqual(evidence["cells"], [])
        self.assertEqual(resolved["checked"]["N"], 2)
        self.assertEqual(
            resolved["checked"]["Median ms"],
            results["checked"]["summary"]["time_wall_clock"]["median"] * 1000,
        )
        publish(self.batch, self.csv, self.md)
        exported = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        self.assertEqual(float(exported[0]["Median ms"]), resolved["checked"]["Median ms"])
        self.assertIn('{{ table("results.render.json") }}', self.md.read_text())
        original = self.csv.read_bytes(), self.md.read_bytes()
        publish(self.batch, self.csv, self.md)
        self.assertEqual(original, (self.csv.read_bytes(), self.md.read_bytes()))

    def test_one_hyperfine_row_runs_once_and_all_columns_share_the_raw_export(self):
        calls = self.root / "calls"
        code = f'from pathlib import Path; p=Path({str(calls)!r}); p.write_text(p.read_text()+"run\\n" if p.exists() else "run\\n")'
        result = self.measure(code)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls.read_text(), "run\n" * 3)
        manifest, resolved, results = validate_batch(self.batch)
        evidence = json.loads((self.batch / "in.bench.json").read_text())
        self.assertEqual(len(evidence["commands"]), 1)
        self.assertEqual(evidence["commands"][0]["kind"], "hyperfine-row")
        self.assertEqual(evidence["cells"], [])
        self.assertEqual(len(evidence["rows"]), 1)
        self.assertEqual(list(resolved["checked"]), list(__import__("update").FIELDS))
        publish(self.batch, self.csv, self.md)
        before = self.csv.read_bytes()
        data = json.loads((self.batch / "in.render.json").read_text())
        data["checked"]["SD ms"] += 10
        (self.batch / "in.render.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "checksum"):
            publish(self.batch, self.csv, self.md)
        self.assertEqual(self.csv.read_bytes(), before)

    def test_process_threshold_gate_accepts_one_command_per_condition(self):
        spec = importlib.util.spec_from_file_location(
            "process_compare_rows", ROOT / "process/compare.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        result = self.measure(names=("native", "host-run", "status", "review"))
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.batch / "in.bench.json"
        table, regressions = module.compare(path, path, 15)
        self.assertEqual(regressions, 0)
        self.assertEqual(table.count("within threshold"), 4)
        evidence = json.loads(path.read_text())
        self.assertEqual(len(evidence["commands"]), 4)
        self.assertEqual(len(evidence["rows"]), 4)
        self.assertEqual(evidence["cells"], [])

    def test_failed_payload_and_failed_correctness_hook_cannot_publish(self):
        for index, (code, extra) in enumerate(
            [("raise SystemExit(7)", ()), ("pass", ("--conclude", "exit 1"))]
        ):
            self.batch = self.root / ".data" / f"failure-{index}"
            result = self.measure(code, extra)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((self.batch / "in.render.json").exists())
            self.assertTrue(list((self.batch / "in.bench.logs").glob("*.log")))
            with self.assertRaises((ValueError, OSError)):
                publish(self.batch, self.csv, self.md)
            self.assertFalse(self.csv.exists())

    def test_prepare_and_conclude_cover_warmups_and_samples(self):
        evidence = self.root / "state"

        def command(code):
            return shlex.join([sys.executable, "-c", code])

        prepare = command(
            f'from pathlib import Path; Path({str(evidence)!r}).write_text("prepared")'
        )
        conclude = command(
            f'from pathlib import Path; assert Path({str(evidence)!r}).read_text()=="checked"'
        )
        result = self.measure(
            f'from pathlib import Path; p=Path({str(evidence)!r}); assert p.read_text()=="prepared"; p.write_text("checked")',
            ("--prepare", prepare, "--conclude", conclude),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        publish(self.batch, self.csv, self.md)

    def test_artifact_change_is_rejected_before_publication(self):
        artifact = self.root / "artifact"
        artifact.write_text("before")
        result = self.measure(
            f'from pathlib import Path; Path({str(artifact)!r}).write_text("after")',
            artifacts=[artifact],
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        with self.assertRaisesRegex(ValueError, "artifact changed"):
            publish(self.batch, self.csv, self.md)
        self.assertFalse(self.csv.exists())

    def test_tampered_resolved_cell_or_export_is_rejected(self):
        self.assertEqual(self.measure().returncode, 0)
        path = self.batch / "in.render.json"
        original = path.read_bytes()
        table = json.loads(original)
        table["checked"]["Median ms"] = 0
        path.write_text(json.dumps(table))
        with self.assertRaisesRegex(ValueError, "resolved cell"):
            publish(self.batch, self.csv, self.md)
        path.write_bytes(original)
        publish(self.batch, self.csv, self.md)
        with (self.batch / "in.bench.json").open("a") as stream:
            stream.write(" ")
        with self.assertRaisesRegex(ValueError, "checksum"):
            publish(self.batch, self.csv, self.md)

    def test_schema_mismatch_preserves_public_results_and_raw_cannot_be_overwritten(self):
        self.assertEqual(self.measure().returncode, 0)
        self.csv.write_text("legacy,value\nold,1\n")
        self.md.write_text("historical\n")
        original = self.csv.read_bytes(), self.md.read_bytes()
        with self.assertRaisesRegex(ValueError, "schema differs"):
            publish(self.batch, self.csv, self.md)
        self.assertEqual(original, (self.csv.read_bytes(), self.md.read_bytes()))
        with self.assertRaisesRegex(ValueError, "outside raw evidence"):
            publish(self.batch, self.root / "other.csv", self.batch / "in.json")
        with (
            self.assertRaises(FileExistsError),
            patch.object(COMMON, "registered", return_value={}),
        ):
            COMMON.create_batch(self.entry, self.batch)

    def test_new_batch_appends_without_rewriting_previous_rows(self):
        self.assertEqual(self.measure().returncode, 0)
        publish(self.batch, self.csv, self.md)
        first = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        self.batch = self.batch.with_name("second")
        self.assertEqual(self.measure().returncode, 0)
        publish(self.batch, self.csv, self.md)
        combined = list(csv.DictReader(io.StringIO(self.csv.read_text())))
        self.assertEqual(combined[: len(first)], first)
        self.assertEqual(len(combined), 2)

    def test_process_gate_requires_all_conditions_and_rejects_failed_manifests(self):
        spec = importlib.util.spec_from_file_location(
            "process_compare", ROOT / "process/compare.py"
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        result = self.measure(names=("native", "host-run", "status", "review"))
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.batch / "in.bench.json"
        table, regressions = module.compare(path, path, 15)
        self.assertEqual(regressions, 0)
        self.assertEqual(table.count("within threshold"), 4)
        manifest_path = self.batch / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["status"] = "failed"
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "successful"):
            module.compare(path, None, 15)


if __name__ == "__main__":
    unittest.main()
