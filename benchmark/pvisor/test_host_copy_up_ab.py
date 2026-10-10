"""Conventional host-copy-up harness tests; no hardware or approval ledgers."""

import tempfile
import unittest
from pathlib import Path

import host_copy_up_ab as bench


class HostCopyUpTests(unittest.TestCase):
    def test_order_is_seeded_and_keeps_complete_pairs(self):
        plan = bench.order(4207, 33)
        self.assertEqual(plan, bench.order(4207, 33))
        self.assertNotEqual(plan, bench.order(4208, 33))
        for cells in plan:
            self.assertEqual(set(cells), {(c, a) for c in bench.CONDITIONS for a in bench.ARMS})
            self.assertEqual(len(cells), 4)
            self.assertEqual(cells[0][0], cells[1][0])
            self.assertEqual(cells[2][0], cells[3][0])

    def test_full_fixture_and_changed_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            expected = bench.fixture(root / "lower", 1024 * 1024)
            self.assertEqual(bench.file_digest(root / "lower/slow"), expected["source_sha256"])
            upper = root / "upper"
            upper.write_bytes((root / "lower/slow").read_bytes())
            with upper.open("r+b") as stream:
                stream.write(b"changed")
            self.assertEqual(bench.file_digest(upper), expected["upper_sha256"])
            self.assertEqual(len(list((root / "lower/probe").iterdir())), 64)

    def test_copy_progress_supports_both_layouts_and_rejects_complete_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            old = work / ".wh..pvisor-copyup-old"
            old.write_bytes(b"old")
            new = work / ".wh..pvisor-copyup-new"
            new.mkdir()
            (new / "partial").write_bytes(b"copy")
            (new / "empty").touch()
            (new / "complete").write_bytes(b"12345678")
            self.assertEqual(
                {Path(item["path"]).name for item in bench.copying_paths(work, 8)},
                {".wh..pvisor-copyup-old", "partial"},
            )

    def test_incomplete_or_duplicate_cohorts_cannot_receive_statistics(self):
        rows = [
            dict(
                condition=c,
                arm=a,
                round=0,
                correctness="passed",
                detached=True,
                first_stat_ms=10,
                copying_metadata_ms=20,
                open_ms=30,
                idle_metadata_ms=5,
            )
            for c in bench.CONDITIONS
            for a in bench.ARMS
        ]
        self.assertEqual(len(bench.summarize(rows, 1)), 8)
        for wrong in (rows[:-1], rows + [rows[0]]):
            with self.assertRaises(AssertionError):
                bench.summarize(wrong, 1)
        rows[0]["correctness"] = "failed"
        with self.assertRaises(AssertionError):
            bench.summarize(rows, 1)


if __name__ == "__main__":
    unittest.main()
