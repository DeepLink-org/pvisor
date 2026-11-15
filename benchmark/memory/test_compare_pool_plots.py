import tempfile
import unittest
from pathlib import Path

from compare_pool_plots import ARMS, index_rows, points, published_levels
from pool_compare import PATTERNS


def row(pattern="repeated", arm="pvisor-off"):
    summary = {
        "pattern": pattern,
        "arm": arm,
        "post_write_offset_seconds": 70,
        "startup_seconds_until_all_ready": 2,
        "before_start": {"charged_mib": 10, "pss_mib": 11},
        "idle-0": {"actual_seconds_after_ready": 0.2, "charged_mib": 100, "pss_mib": 101},
    }
    return (
        summary,
        [(-1, 20), (0, 30), (0.1, 40), (60.1, 50), (70, 60), (100, 70), (101, 80)],
        [(0, 5)],
    )


class ComparisonPlotsTest(unittest.TestCase):
    def test_windows_preserve_raw_values_and_never_impute_endpoints(self):
        data = row()
        self.assertEqual(points(data, "charged_mib", "idle"), [(0, 30), (0.1, 40)])
        self.assertEqual(points(data, "charged_mib", "post-write"), [(0, 60), (30, 70)])
        self.assertEqual(points(data, "pss_mib", "idle"), [(0, 5)])
        with self.assertRaisesRegex(ValueError, "unknown"):
            points(data, "charged_mib", "other")
        with self.assertRaisesRegex(ValueError, "metric"):
            points(data, "other", "idle")

    def test_startup_has_its_own_origin_and_actual_readiness_endpoint(self):
        self.assertEqual(
            points(row(), "charged_mib", "startup"),
            [(0, 10), (1, 20), (2, 30), (2.1, 40), (2.2, 100)],
        )

    def test_comparison_rejects_missing_duplicate_or_mixed_phase_cells(self):
        rows = [row(p, a) for p in PATTERNS for a in ARMS]
        self.assertEqual(len(index_rows(rows)), 6)
        with self.assertRaisesRegex(ValueError, "all six"):
            index_rows(rows[:-1])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            index_rows(rows + rows[:1])
        del rows[0][0]["post_write_offset_seconds"]
        with self.assertRaisesRegex(ValueError, "post-write"):
            index_rows(rows)

    def test_published_table_is_only_a_level_reference_not_a_curve_source(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "reference.md"
            source = "### Static {#pool-static-results}\n| pvisor-off | 1 | 2 | 3 |\n| pvisor-physical | 4 | 5 | 6 |\n"
            path.write_text(source + "### Readback {#pool-readback}\n| pvisor-off | 7 | 8 | 9 |\n")
            levels = published_levels(path)
            self.assertEqual(levels["pvisor-physical"]["random-shared"], 5)
            path.write_text(source + "| pvisor-off | 7 | 8 | 9 |\n")
            with self.assertRaisesRegex(ValueError, "ambiguous"):
                published_levels(path)


if __name__ == "__main__":
    unittest.main()
