"""Small correctness checks for the B-FS-ENG hot-path harness."""

import tempfile
import unittest
from pathlib import Path

import host_hotpath_ab as subject


class HotpathTests(unittest.TestCase):
    def test_fixture_inventory_detects_byte_changes(self):
        with tempfile.TemporaryDirectory() as root:
            lower = Path(root) / "lower"
            paths = subject.fixture(lower)
            self.assertEqual(len(paths), 512)
            before = subject.inventory(lower)
            self.assertEqual(len(before), 768)
            (lower / paths[0]).write_bytes(b"changed")
            self.assertNotEqual(subject.inventory(lower), before)

    def test_summary_preserves_conditions_pairs_and_excludes_warmup(self):
        rows = []
        for condition in subject.CONDITIONS:
            for pair in range(4):
                for arm, value in (("candidate", 8.0), ("baseline", 10.0)):
                    rows.append(
                        dict(
                            condition=condition,
                            arm=arm,
                            warmup=pair == 0,
                            milliseconds={"operation": 1000 if pair == 0 else value},
                        )
                    )
        result = subject.summary(rows)
        self.assertEqual(len(result), 2)
        for item in result:
            self.assertEqual(item["baseline"]["n"], 3)
            self.assertEqual(item["candidate"]["p50"], 8)
            self.assertAlmostEqual(item["percent_change"], -20)
            self.assertAlmostEqual(item["ci95_percent"][0], -20)


if __name__ == "__main__":
    unittest.main()
