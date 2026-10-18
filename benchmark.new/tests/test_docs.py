"""Dynamic table publication and inline historical documentation tables."""

import csv
import importlib.util
import json
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

from tests.fixtures import workspace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
spec = importlib.util.spec_from_file_location("benchmark_docs", ROOT / "docs.py")
DOCS = importlib.util.module_from_spec(spec)
spec.loader.exec_module(DOCS)


class DocumentationTests(unittest.TestCase):
    def test_all_benchmark_articles_and_bound_data_are_consistent(self):
        pages, tables = DOCS.check_inventory()
        self.assertEqual(pages, 36)
        self.assertEqual(tables, 6)
        manifest = json.loads((ROOT / "docs.json").read_text())
        pairs = {}
        for page in manifest["pages"]:
            self.assertTrue(all(t["kind"] in ("published", "coverage") for t in page["tables"]))
            pairs.setdefault(page["page"], []).append(page)
        for pair in pairs.values():
            self.assertEqual(len(pair), 2)
            self.assertEqual(pair[0]["benchmark_ids"], pair[1]["benchmark_ids"])
            self.assertEqual(
                [t["rows"] for t in pair[0]["tables"]], [t["rows"] for t in pair[1]["tables"]]
            )
            self.assertEqual(
                [len(t["columns"]) for t in pair[0]["tables"]],
                [len(t["columns"]) for t in pair[1]["tables"]],
            )

    def test_inline_historical_tables_need_no_json_binding(self):
        with workspace() as (root, resources):
            pages = []
            for locale in ("zh", "en"):
                article = root / "docs/src" / locale / "benchmarks/example.md"
                article.parent.mkdir(parents=True)
                article.write_text(
                    "# Historical data\n\n| Backend | Median ms |\n| --- | ---: |\n| Native | 1.27 |\n"
                )
                pages.append(
                    dict(locale=locale, page="benchmarks/example.md", tables=[], source_csv=[])
                )
            inventory = root / "docs.json"
            inventory.write_text(json.dumps(dict(format=1, pages=pages)))
            with patch.object(DOCS, "REPO", root), patch.object(DOCS, "INVENTORY", inventory):
                self.assertEqual(DOCS.check_inventory(), (2, 0))

    def test_publication_writes_one_shared_source_and_preserves_raw_records(self):
        with workspace() as (root, resources):
            batch = root / ".data/replay-test"
            batch.mkdir(parents=True)
            manifest = dict(
                entry="replay/replay.json",
                batch=batch.name,
                benchmark_id="B-REPLAY",
                role="user-facing",
                metric_scope=DOCS.METRIC_SCOPE,
                runs=60,
                warmup=0,
                hyperfine_version="hyperfine 2.0.0",
                rows={"codex": "codex"},
                exports={"in.bench.json": "a" * 64},
                artifacts={
                    str(ROOT / "replay/replay_hooks.py"): "b" * 64,
                    str(batch / "pvisor-replay"): "c" * 64,
                    str(batch / "fixtures/codex/trajectory.json"): "d" * 64,
                },
                source_commit_scope="binary source is not attested",
            )
            raw_path = batch / "manifest.json"
            raw_path.write_text(json.dumps(manifest))
            original = raw_path.read_bytes()
            resolved = {
                "codex": dict(
                    zip(
                        ["N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms"],
                        [60, 1.2, 1.3, 0.1, 1.0, 1.6],
                    )
                )
            }
            pages = [
                dict(
                    locale=locale,
                    page="benchmarks/replay-fidelity.md",
                    tables=[
                        dict(
                            json="../../overrides/assets/benchmarks/replay-semspec.render.json",
                            kind="published",
                            rows=1,
                            columns=list(resolved["codex"]),
                        )
                    ],
                )
                for locale in ("zh", "en")
            ]
            inventory = root / "docs.json"
            inventory.write_text(
                json.dumps(
                    dict(
                        pages=pages,
                        bindings={
                            manifest["entry"]: dict(
                                benchmark_id="B-REPLAY",
                                page="benchmarks/replay-fidelity.md",
                                table="../../overrides/assets/benchmarks/replay-semspec.render.json",
                                csv="replay-semspec.csv",
                                show_batch=False,
                            )
                        },
                    )
                )
            )
            with (
                patch.object(DOCS, "REPO", root),
                patch.object(DOCS, "INVENTORY", inventory),
                patch.object(
                    DOCS,
                    "validate_batch",
                    return_value=(
                        manifest,
                        resolved,
                        {"codex": {"command": "pvisor-replay fixture"}},
                    ),
                ),
                patch("measurement_coverage.coverage"),
            ):
                targets = DOCS.publish_docs(batch)
            shared = root / "docs/overrides/assets/benchmarks/replay-semspec.provenance.json"
            provenance = json.loads(shared.read_text())
            self.assertEqual(provenance["manifest_sha256"], DOCS.digest(raw_path))
            self.assertEqual(provenance["exports"], manifest["exports"])
            self.assertEqual(provenance["source_commit_scope"], manifest["source_commit_scope"])
            self.assertEqual(
                provenance["artifacts"],
                {
                    "pvisor-replay": "c" * 64,
                    "fixtures/codex/trajectory.json": "d" * 64,
                },
            )
            self.assertEqual(raw_path.read_bytes(), original)
            self.assertEqual(
                targets, [root / "docs/overrides/assets/benchmarks/replay-semspec.render.json"]
            )
            self.assertTrue(DOCS.public_csv_path(root, "replay-semspec.csv").is_file())
            for locale in ("zh", "en"):
                self.assertFalse(
                    (root / f"docs/src/{locale}/benchmarks/replay-semspec.csv").exists()
                )
            for target in targets:
                self.assertFalse(target.with_suffix(".provenance.json").exists())

    def test_scope_and_fixture_mismatch_cannot_overwrite_documentation(self):
        binding = dict(
            benchmark_id="B-FS-TOOLS",
            metric_scope="expected timer",
            fixture_scope="expected workload",
        )
        metadata = dict(
            entry="filesystem/tools.json",
            benchmark_id="B-FS-TOOLS",
            role="user-facing",
            metric_scope="wrong timer",
            fixture_scope="wrong workload",
        )
        for scope, message in (
            ("wrong timer", "measurement scope"),
            ("expected timer", "workload configuration"),
        ):
            metadata["metric_scope"] = scope
            with (
                patch.object(
                    DOCS.Path,
                    "read_text",
                    side_effect=[
                        json.dumps(dict(bindings={metadata["entry"]: binding})),
                        json.dumps(metadata),
                    ],
                ),
                patch.object(DOCS, "validate_batch") as validate,
            ):
                with self.assertRaisesRegex(ValueError, message):
                    DOCS.publish_docs(Path("/not-published"))
                validate.assert_not_called()

    def test_live_bindings_declare_event_and_whole_command_scopes(self):
        inventory = json.loads((ROOT / "docs.json").read_text())
        for entry, binding in inventory["bindings"].items():
            if binding.get("kind") == "startup":
                self.assertIn("same Popen launch", binding["metric_scope"])
                self.assertEqual(binding["page"], "benchmarks/startup.md")
        self.assertNotIn("filesystem/tools.json", inventory["bindings"])
        self.assertFalse(inventory["bindings"]["replay/replay.json"]["show_batch"])

    def test_live_json_must_equal_latest_csv_and_sample_counts(self):
        with workspace() as (root, resources):
            base = root / "docs/src/en/benchmarks"
            base.mkdir(parents=True)
            columns = ["N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms"]
            header = ["condition", "role", "benchmark_id", "metric_scope", "runs", *columns]
            csv_path = DOCS.public_csv_path(root, "results.csv")
            csv_path.parent.mkdir(parents=True, exist_ok=True)
            with csv_path.open("w") as stream:
                writer = csv.writer(stream)
                writer.writerow(header)
                for name in ("native-metadata", "native-read"):
                    writer.writerow(
                        [
                            name,
                            "user-facing",
                            "B-FS-TOOLS",
                            DOCS.METRIC_SCOPE,
                            30,
                            30,
                            1.23456,
                            2.34567,
                            0.12345,
                            1.11111,
                            3.99999,
                        ]
                    )
            target = base / "results.render.json"
            expected = DOCS.csv_table(
                csv_path,
                {name: name for name in ("native-metadata", "native-read")},
                dict(zip(columns, columns)),
                DOCS.METRIC_SCOPE,
                "B-FS-TOOLS",
            )
            target.write_text(json.dumps(expected))
            inventory = dict(
                pages=[
                    dict(
                        locale="en",
                        page="benchmarks/filesystem.md",
                        tables=[
                            dict(
                                json="benchmarks/results.render.json",
                                kind="published",
                                columns=columns,
                            )
                        ],
                    )
                ],
                bindings={
                    "filesystem/tools.json": dict(
                        table="benchmarks/results.render.json",
                        csv="results.csv",
                        benchmark_id="B-FS-TOOLS",
                        conditions={name: name for name in ("native-metadata", "native-read")},
                    )
                },
            )
            with patch.object(DOCS, "REPO", root):
                DOCS.check_live_tables(inventory)
                expected["native-read"]["Median ms"] = "1.24"
                target.write_text(json.dumps(expected))
                with self.assertRaisesRegex(ValueError, "differs from latest audited CSV"):
                    DOCS.check_live_tables(inventory)
            with self.assertRaisesRegex(ValueError, "different metric"):
                DOCS.csv_table(csv_path, {}, {}, "wrong timer", "B-FS-TOOLS")
            csv_path.write_text(csv_path.read_text().replace(",30,30,", ",30,29,"))
            with self.assertRaisesRegex(ValueError, "sample count"):
                DOCS.csv_table(
                    csv_path,
                    {"native-read": "native-read"},
                    {"N": "N"},
                    DOCS.METRIC_SCOPE,
                    "B-FS-TOOLS",
                )

    def test_engineering_batch_cannot_publish_to_user_tables(self):
        with (
            patch.object(
                DOCS.Path,
                "read_text",
                return_value=json.dumps(
                    {
                        "format": 1,
                        "bindings": {},
                        "entry": "process/smoke.json",
                        "benchmark_id": "B-PROCESS",
                        "role": "engineering A/B",
                    }
                ),
            ),
            patch.object(DOCS, "validate_batch") as validate,
        ):
            with self.assertRaisesRegex(ValueError, "user-facing"):
                DOCS.publish_docs(Path("/unmeasured"))
            validate.assert_not_called()


if __name__ == "__main__":
    unittest.main()
