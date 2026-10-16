"""Trust-boundary checks for the three-arm firmware experiment."""

import unittest
from unittest.mock import patch

import guest_cache_adaptive as adaptive


class ThreeArmTests(unittest.TestCase):
    def report(self):
        return {
            "cohorts": ["sdk"],
            "samples": 1,
            "rows": [
                {"round": 0, "cohort": "sdk", "arm": a, "times": {"test": 10 + a}}
                for a in adaptive.LABELS
            ],
        }

    def test_both_references_share_the_same_candidate(self):
        with patch.object(adaptive.bench, "analyze", return_value=[{}]) as analyze:
            result = adaptive.analyze(self.report())
            self.assertEqual([x["reference"] for x in result], ["eager", "scan_only"])
            self.assertEqual(
                [[x["times"]["test"] for x in c.args[0]["rows"]] for c in analyze.call_args_list],
                [[10, 12], [11, 12]],
            )

    def test_missing_duplicate_or_unknown_arm_is_rejected(self):
        for operation in ("missing", "duplicate", "unknown"):
            r = self.report()
            if operation == "missing":
                r["rows"].pop(1)
            elif operation == "duplicate":
                r["rows"].append(r["rows"][0])
            else:
                r["rows"][0]["arm"] = 3
            with self.subTest(operation=operation), self.assertRaises(ValueError):
                adaptive.analyze(r)


if __name__ == "__main__":
    unittest.main()
