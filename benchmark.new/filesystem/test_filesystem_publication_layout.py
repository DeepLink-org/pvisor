"""Offline publication layout regressions; all writes use synthetic temp trees."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))


def load(name):
    spec = importlib.util.spec_from_file_location("layout_" + name, HERE / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


BASE = load("publish_guest_writeback")
TUNING = load("publish_guest_writeback_tuning")
CLI = load("publish_guest_writeback_cli")
IO = load("publish_io_curves")
WORKLOADS = load("publish_workloads_v2")


class PublicationLayoutTests(unittest.TestCase):
    def test_private_archive_is_readonly_when_checked_and_refuses_changed_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary) / "input"
            targets = BASE.archive_targets(
                batch, "synthetic", {"table.csv": b"value\n1.23456789\n"}, b"{}"
            )
            BASE.preflight_archive(targets)
            self.assertFalse(batch.exists())
            BASE.write_archive(targets)
            stamps = {path: path.stat().st_mtime_ns for path in targets}
            BASE.write_archive(targets)
            self.assertEqual(stamps, {path: path.stat().st_mtime_ns for path in targets})
            table = next(path for path in targets if path.suffix == ".csv")
            table.write_bytes(b"preserved external bytes")
            with self.assertRaisesRegex(ValueError, "different archived bytes"):
                BASE.write_archive(targets)
            self.assertEqual(table.read_bytes(), b"preserved external bytes")

    def test_private_archive_rejects_a_symlinked_publication_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            batch, outside = root / "batch", root / "outside"
            batch.mkdir()
            outside.mkdir()
            (batch / "publication").symlink_to(outside, target_is_directory=True)
            targets = BASE.archive_targets(batch, "synthetic", {"table.csv": b"value\n1\n"}, b"{}")
            with self.assertRaisesRegex(ValueError, "symlink"):
                BASE.write_archive(targets)
            self.assertEqual(list(outside.iterdir()), [])

    def test_collected_provenance_omits_pdfs_but_still_requires_local_pdf_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            timing, diagnostic = root / "timing", root / "diagnostic"
            analysis, times = timing / "analysis", timing / "analysis-times"
            analysis.mkdir(parents=True)
            times.mkdir()
            diagnostic.mkdir()
            for folder, names in (
                (analysis, ("summary.json",)),
                (times, ("summary.json",)),
                (
                    diagnostic,
                    (
                        "protocol.json",
                        "records.json",
                        "status.json",
                        "guest-writeback-diagnostics.json",
                        "in.json",
                        "in.bench.json",
                        "in.render.json",
                    ),
                ),
            ):
                for name in names:
                    (folder / name).write_text("{}\n")
            (times / "time_levels.csv").write_bytes(b"value\n1.23456789\n")
            plots = [
                f"family-{family}.{extension}"
                for family in BASE.FAMILIES
                for extension in ("svg", "pdf")
            ]
            plots += [
                f"{name}.{extension}"
                for name in ("write-requests", "write-payload-histogram")
                for extension in ("svg", "pdf")
            ]
            for name in plots:
                (times / name).write_bytes(b"%PDF-1.4" if name.endswith(".pdf") else b"<svg></svg>")
            protocol = dict(
                source_hashes={},
                condition_factors={},
                cells=[],
                artifacts={
                    name: "a" * 64
                    for name in (
                        "bin/subject",
                        "bin/payload",
                        "bin/libkrunfw.so",
                        "bin/libkrunfw-old.so",
                    )
                },
            )
            summary = dict(
                curves=[],
                comparisons=[dict(condition="guest-wb", value=1)],
                enablement_comparisons=[dict(value=1)],
                source=dict(analysis_sha256="b" * 64),
            )
            time_summary = dict(
                plots=plots,
                outputs={name: BASE.sha(times / name) for name in plots},
                diagnostic_inputs={path.name: BASE.sha(path) for path in diagnostic.iterdir()},
                comparisons=[dict(value=1)],
                postprocessor_sha256="c" * 64,
                statistical_scope={},
            )

            @contextlib.contextmanager
            def frozen(_):
                yield None, SimpleNamespace(flattened_summary=lambda _: [dict(value=1)])

            def cohort(_, profiling):
                return (diagnostic if profiling else timing, protocol, [], [dict(value=1)], {})

            arguments = SimpleNamespace(
                timing=timing,
                diagnostic=diagnostic,
                analysis=None,
                time_analysis=None,
                pipeline=None,
                correctness=None,
            )
            with (
                patch.object(BASE, "audit_cohort", side_effect=cohort),
                patch.object(BASE, "audit_analyses", return_value=(summary, time_summary)),
                patch.object(
                    BASE,
                    "diagnostic_tables",
                    return_value=(
                        dict(source_sha256="d" * 64),
                        [dict(value=1)],
                        [dict(value=1)],
                        [dict(value=1)],
                        [dict(value=1)],
                    ),
                ),
                patch.object(BASE, "frozen", frozen),
            ):
                files, manifest, encoded = BASE.collect(arguments)
                public = json.loads(encoded)
                self.assertEqual(set(public["files"]), set(files))
                self.assertEqual(public["files"], manifest["files"])
                self.assertEqual(sum(name.endswith(".svg") for name in files), 7)
                self.assertFalse(any(name.endswith(".pdf") for name in public["files"]))
                local_pdf = next(times.glob("*.pdf"))
                self.assertEqual(local_pdf.read_bytes(), b"%PDF-1.4")
                local_pdf.unlink()
                with self.assertRaises(FileNotFoundError):
                    BASE.collect(arguments)

    def test_tuning_provenance_and_returned_files_share_the_svg_csv_inventory(self):
        files = {
            "filesystem-guest-writeback-tuning-levels.csv": {"data": b"v\n1\n", "sha256": "a" * 64},
            "guest-writeback-block.svg": {"data": b"<svg></svg>", "sha256": "b" * 64},
            "guest-writeback-block.pdf": {"data": b"%PDF-1.4", "sha256": "c" * 64},
        }
        arguments = SimpleNamespace(
            pipeline=Path("synthetic-pipeline"), correctness=Path("synthetic-checks")
        )
        with patch.object(TUNING, "ORIGINAL_COLLECT", return_value=(files, {}, b"{}")):
            public_files, manifest, encoded = TUNING.collect(arguments)
        public = json.loads(encoded)
        self.assertEqual(set(public_files), set(manifest["files"]))
        self.assertEqual(set(public_files), set(public["files"]))
        self.assertEqual(len(public_files), 2)
        self.assertFalse(any(name.endswith(".pdf") for name in public["files"]))

    def test_writeback_publishers_share_csvs_and_exclude_pdf_destinations(self):
        for module, namespace in (
            (BASE, "filesystem-guest-writeback"),
            (TUNING.BASE, "filesystem-guest-writeback-tuning"),
        ):
            with self.subTest(namespace=namespace), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                name = namespace + "-levels.csv"
                files = {
                    name: {"data": b"full_precision\n1.23456789\n"},
                    "plot.svg": {"data": b"<svg></svg>"},
                    "plot.pdf": {"data": b"%PDF-1.4"},
                }
                assets = Path("docs/overrides/assets/benchmarks") / namespace
                with patch.object(module, "REPO", root), patch.object(module, "ASSETS", assets):
                    destinations = module.destinations(files, b"{}")
                self.assertEqual(len(destinations), 1)
                self.assertEqual(destinations[assets / "plot.svg"], files["plot.svg"]["data"])
                self.assertTrue(all(path.suffix == ".svg" for path in destinations))

    def test_writeback_entrypoints_archive_metadata_and_publish_only_their_svg(self):
        for entry, module, namespace in (
            (BASE, BASE, "filesystem-guest-writeback"),
            (TUNING, TUNING.BASE, "filesystem-guest-writeback-tuning"),
        ):
            with self.subTest(namespace=namespace), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                batch = root / "input"
                batch.mkdir()
                files = {
                    namespace + "-levels.csv": dict(data=b"value\n1.23456789\n"),
                    "plot.svg": dict(data=b"<svg></svg>"),
                }
                manifest = dict(namespace=namespace)
                encoded = json.dumps(manifest).encode()
                assets = Path("docs/overrides/assets/benchmarks") / namespace
                arguments = [
                    "publish",
                    "--timing",
                    str(batch),
                    "--diagnostic",
                    str(root / "diagnostic"),
                    "--pipeline",
                    str(root / "pipeline"),
                    "--correctness",
                    str(root / "correctness"),
                    "--publish",
                ]
                with (
                    patch.object(entry, "collect", return_value=(files, manifest, encoded)),
                    patch.object(module, "REPO", root),
                    patch.object(module, "ASSETS", assets),
                    patch.object(sys, "argv", arguments),
                    contextlib.redirect_stdout(io.StringIO()),
                ):
                    with patch.object(sys, "argv", [*arguments[:-1], "--check"]):
                        entry.main()
                    self.assertEqual(list(batch.iterdir()), [])
                    self.assertFalse((root / "docs").exists())
                    entry.main()
                public = [path for path in (root / "docs").rglob("*") if path.is_file()]
                self.assertEqual(public, [root / assets / "plot.svg"])
                archive = batch / "publication" / namespace
                self.assertEqual(
                    (archive / (namespace + "-levels.csv")).read_bytes(), b"value\n1.23456789\n"
                )
                self.assertEqual((archive / "provenance.json").read_bytes(), encoded)

    def test_cli_publication_archives_tables_without_public_downloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            files = {
                CLI.PREFIX + suffix + ".csv": b"value\n1.23456789\n"
                for suffix in ("-levels", "-comparisons", "-environments", "-counters")
            }
            with (
                patch.object(CLI, "REPO", root),
                patch.object(CLI, "project", return_value=(files, b"{}")),
                patch.object(sys, "argv", ["publish", "--batch", str(root / ".data"), "--publish"]),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                with patch.object(
                    sys, "argv", ["publish", "--batch", str(root / ".data"), "--check"]
                ):
                    CLI.main()
                self.assertEqual(list(root.iterdir()), [])
                CLI.main()
            self.assertEqual(len(list(root.rglob("*.csv"))), len(files))
            self.assertFalse((root / "docs").exists())
            for name, expected in files.items():
                self.assertEqual(
                    (root / ".data/publication" / CLI.PREFIX / name).read_bytes(), expected
                )

    def test_io_publication_archives_csv_and_provenance_and_publishes_only_svg(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            table = "filesystem-io-shape.csv"
            files = {
                table: {"data": b"value\n1.23456789\n", "sha256": ""},
                "curve.svg": {"data": b"<svg></svg>", "sha256": ""},
                "unused.svg": {"data": b"<svg>archived</svg>", "sha256": ""},
            }
            import hashlib

            for spec in files.values():
                spec["sha256"] = hashlib.sha256(spec["data"]).hexdigest()
            arguments = [
                "publish",
                "--shape",
                str(root / "shape"),
                "--parameters",
                str(root / "parameters"),
                "--confirm",
                str(root / "confirm"),
                "--cli",
                str(root / "cli"),
                "--publish",
            ]
            with (
                patch.object(IO, "REPO", root),
                patch.object(
                    IO,
                    "collect",
                    return_value=(
                        files,
                        {
                            "public_outputs": {
                                "docs/overrides/assets/benchmarks/filesystem-io/curve.svg": files[
                                    "curve.svg"
                                ]["sha256"]
                            }
                        },
                        b"{}",
                    ),
                ),
                patch.object(sys, "argv", arguments),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                with patch.object(sys, "argv", [*arguments[:-1], "--check"]):
                    IO.main()
                self.assertEqual(list(root.iterdir()), [])
                IO.main()
            self.assertEqual(
                (root / "shape/publication/filesystem-io" / table).read_bytes(),
                files[table]["data"],
            )
            self.assertEqual(len(list(root.rglob("*.csv"))), 1)
            self.assertEqual(
                (root / "shape/publication/filesystem-io/unused.svg").read_bytes(),
                files["unused.svg"]["data"],
            )
            self.assertFalse((root / "docs/src").exists())
            self.assertEqual(
                [p.suffix for p in (root / "docs").rglob("*") if p.is_file()], [".svg"]
            )
            self.assertFalse(list(root.rglob("*.pdf")))

    def test_workload_publication_keeps_tables_pdfs_and_provenance_local(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            batch = root / ".data/synthetic"
            analysis = batch / "analysis"
            analysis.mkdir(parents=True)
            for name in (
                "protocol.json",
                "records.json",
                "status.json",
                "in.json",
                "in.bench.json",
                "in.render.json",
                "fixture-manifest.json",
                "firmware-source/SOURCE.json",
                "analysis/summary.json",
            ):
                path = batch / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("{}\n")
            for name in WORKLOADS.FIGURES:
                (analysis / name).write_bytes(
                    b"%PDF-1.4" if name.endswith(".pdf") else b"<svg></svg>"
                )
            artifacts = {
                name: "a" * 64
                for name in (
                    "bin/subject",
                    "bin/payload",
                    "bin/libkrunfw.so",
                    "harness/workloads_v2_run.py",
                    "harness/workloads_v2_driver.rs",
                    "harness/workloads_v2_guest.rs",
                    "harness/workloads_v2_scenarios.rs",
                    "harness/workloads_v2_analysis.py",
                )
            }
            protocol = dict(
                budget={},
                timing={},
                cache={},
                conditions={},
                limitations=[],
                artifacts=artifacts,
                source_hashes={},
            )
            summary = dict(
                counts={},
                statistical_scope={},
                **{name: [dict(value=1.23456789)] for name in WORKLOADS.TABLES},
            )
            bound = dict(protocol_sha256="b" * 64, records_sha256="c" * 64)
            destination = root / "publication"
            with patch.object(WORKLOADS, "validate", return_value=(protocol, summary, bound)):
                WORKLOADS.publish(batch, destination_root=destination, check=True)
                self.assertFalse((batch / "publication").exists())
                self.assertFalse(destination.exists())
                receipt = WORKLOADS.publish(batch, destination_root=destination)
            self.assertEqual(receipt["files"], 4)
            self.assertEqual(len(list(destination.rglob("*.csv"))), 0)
            self.assertEqual(len(list(destination.rglob("*.svg"))), 4)
            self.assertFalse((destination / "docs/src").exists())
            self.assertFalse(list(destination.rglob("*.pdf")))
            self.assertFalse(list(destination.rglob("*.json")))
            for name in WORKLOADS.TABLES:
                target = (
                    batch
                    / "publication"
                    / WORKLOADS.PREFIX
                    / (WORKLOADS.PREFIX + "-" + name + ".csv")
                )
                self.assertIn(b"1.23456789", target.read_bytes())
            for name in WORKLOADS.FIGURES:
                self.assertTrue((analysis / name).is_file())


if __name__ == "__main__":
    unittest.main()
