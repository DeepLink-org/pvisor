"""Publication rejects incomplete or incorrectly reviewed paired results."""

import copy
import unittest

from publish_supervision import summarize


def report():
    rows = []
    for trial in range(30):
        for backend in ("staged", "git-worktree"):
            extra = {"selection_ms": 1.0, "check_ms": 1.0} if backend == "git-worktree" else {}
            rows.append(
                dict(
                    backend=backend,
                    trial=trial,
                    correctness="passed",
                    content_review_complete=True,
                    files_reviewed=20,
                    files_applied=10,
                    files_dropped=10,
                    review_ms=1.0 + trial / 1000,
                    apply_ms=2.0,
                    drop_ms=1.0,
                    wall_ms=4.0 + trial / 1000 + sum(extra.values()),
                    **extra,
                )
            )
    return dict(
        benchmark_id="B-SUPERVISION",
        cli_arguments=dict(suites="supervision", samples=30, warmups=3),
        capabilities={},
        rows=rows,
    )


class PublishSupervisionTests(unittest.TestCase):
    def test_full_pairing_keeps_selected_extraction_and_check_in_total(self):
        rows, comparisons = summarize(report())
        by_key = {(r["backend"], r["metric"]): r for r in rows}
        assert by_key["staged", "selected_apply_ms"]["p50"] == 2.0
        assert by_key["git-worktree", "selected_apply_ms"]["p50"] == 4.0
        assert all(r["n"] == 30 for r in comparisons)
        total = next(r for r in comparisons if r["metric"] == "total_ms")
        self.assertAlmostEqual(total["difference_ms"], -2.0)

    def test_unsupported_cohorts_are_rejected(self):
        for fault in [
            "missing",
            "duplicate",
            "failed",
            "incomplete-review",
            "wrong-selection",
            "capability-failure",
            "no-warmups",
        ]:
            with self.subTest(fault=fault):
                value = report()
                if fault == "missing":
                    value["rows"].pop()
                elif fault == "duplicate":
                    value["rows"].append(copy.deepcopy(value["rows"][0]))
                elif fault == "failed":
                    value["rows"][0]["correctness"] = "failed"
                elif fault == "incomplete-review":
                    value["rows"][0]["content_review_complete"] = False
                elif fault == "wrong-selection":
                    value["rows"][0]["files_applied"] = 20
                elif fault == "capability-failure":
                    value["capabilities"]["supervision/staged"] = {"state": "failed"}
                else:
                    value["cli_arguments"]["warmups"] = 0
                with self.assertRaises(ValueError):
                    summarize(value)
