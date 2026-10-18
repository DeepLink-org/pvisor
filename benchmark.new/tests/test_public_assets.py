"""Public downloads share bytes and keep translated historical tables distinct."""

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from public_assets import PUBLIC_CSV, check_public_assets, csv_path, is_public_csv  # noqa: E402 - import after module-loading setup


class PublicAssetTests(unittest.TestCase):
    def test_numeric_csv_has_one_location_across_languages(self):
        repo = Path("/repository")
        a = csv_path(repo, "filesystem-layered-references-levels.csv", "zh")
        self.assertEqual(a, csv_path(repo, a.name, "en"))
        self.assertEqual(a.parent.name, "layered-references")
        self.assertTrue(a.is_relative_to(repo / "docs/overrides/assets/benchmarks/data"))

    def test_translated_csv_preserves_both_languages(self):
        repo = Path("/repository")
        self.assertNotEqual(
            csv_path(repo, "supervision-cost.csv", "zh"),
            csv_path(repo, "supervision-cost.csv", "en"),
        )
        with self.assertRaises(ValueError):
            csv_path(repo, "supervision-cost.csv")

    def test_experiment_statistics_stay_together(self):
        repo = Path("/repository")
        for base in ("filesystem-cli-auto-flush", "filesystem-negative-entry-cache"):
            paths = [
                csv_path(repo, base + suffix + ".csv").parent
                for suffix in ("-summary", "-comparisons", "-counters-provenance")
            ]
            self.assertEqual(len(set(paths)), 1)

    def test_destination_cannot_escape_the_data_tree(self):
        for name in ("../result.csv", "/result.csv", "plots/result.csv", "plot.pdf"):
            with self.assertRaises(ValueError):
                csv_path(Path("/repository"), name)

    def test_intermediate_and_historical_csv_are_not_public_downloads(self):
        self.assertEqual(len(PUBLIC_CSV), 7)
        for name in (
            "filesystem-io-shape.csv",
            "filesystem-layered-references-rows.csv",
            "filesystem-layered-references-environments.csv",
            "measurement-coverage.csv",
        ):
            self.assertFalse(is_public_csv(name))

    def test_public_inventory_rejects_intermediate_json_and_duplicate_csv(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            path = csv_path(repo, "startup-timing.csv")
            path.parent.mkdir(parents=True)
            path.write_text("public data")
            check_public_assets(repo)
            extra = path.with_name("intermediate.json")
            extra.write_text("{}")
            with self.assertRaisesRegex(ValueError, "unneeded public JSON"):
                check_public_assets(repo)
            extra.unlink()
            extra = path.parent.parent / path.name
            extra.write_bytes(path.read_bytes())
            with self.assertRaisesRegex(ValueError, "duplicate public CSV"):
                check_public_assets(repo)

    def test_article_inventory_checks_ignored_evidence_directories(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            articles = repo / "docs/src/zh/benchmarks"
            articles.mkdir(parents=True)
            (articles / "measurement-coverage.render.json").write_text("{}")
            check_public_assets(repo)
            hidden = articles / "vm-memory/assets/.data"
            hidden.mkdir(parents=True)
            for name in ("decision.json", "decision.csv", "decision.pdf"):
                with self.subTest(name=name):
                    extra = hidden / name
                    extra.write_text("local evidence")
                    with self.assertRaisesRegex(ValueError, "outside articles"):
                        check_public_assets(repo)
                    extra.unlink()
