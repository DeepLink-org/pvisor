"""Harness checks without mounting or performance sampling."""

import unittest

import host_read_journal_ab as bench


class ReadJournalTests(unittest.TestCase):
    def test_seeded_complete_pairs_for_each_mode(self):
        for mode in ("reads", "journal"):
            plan = bench.order(mode, 4207, 33)
            self.assertEqual(plan, bench.order(mode, 4207, 33))
            self.assertNotEqual(plan, bench.order(mode, 4208, 33))
            for cells in plan:
                self.assertEqual(
                    set(cells), {(c, a) for c in bench.conditions(mode) for a in bench.common.ARMS}
                )
                self.assertEqual(len(cells), 2 * len(bench.conditions(mode)))
                for index in range(0, len(cells), 2):
                    self.assertEqual(cells[index][0], cells[index + 1][0])

    def test_missing_duplicate_failed_and_missing_warmups_are_rejected(self):
        rows = [
            dict(
                condition=c,
                arm=a,
                round=i,
                correctness="passed",
                detached=True,
                first_journal_ms=10,
                journal_batch_ms=20,
                idle_journal_ms=5,
                hash_ms=30,
            )
            for c in bench.conditions("journal")
            for a in bench.common.ARMS
            for i in (-1, 0)
        ]
        self.assertEqual(len(bench.summarize(rows, "journal", 1, 1)), 8)
        for wrong in (rows[:-1], rows + [rows[0]], [r for r in rows if r["round"] >= 0]):
            with self.assertRaises(AssertionError):
                bench.summarize(wrong, "journal", 1, 1)
        rows[0]["correctness"] = "failed"
        with self.assertRaises(AssertionError):
            bench.summarize(rows, "journal", 1, 1)


if __name__ == "__main__":
    unittest.main()
