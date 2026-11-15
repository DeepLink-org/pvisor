"""Observed crossing brackets must preserve sampling limits and censoring."""

import tempfile
import unittest
from pathlib import Path

from plot_pool import (
    audit_publication_health,
    physical_publication_health,
    select_arms,
    sustained_drop,
)


class DropObservationTests(unittest.TestCase):
    def test_publication_audit_rejects_missing_changed_and_disabled_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audit_publication_health(root, {}, required=False)
            with self.assertRaisesRegex(ValueError, "logs incomplete"):
                audit_publication_health(root, {}, required=True)
            for instance in range(1, 5):
                (root / f"vm{instance}.stderr").write_text("")
            row = dict(publication_health=physical_publication_health(root))
            audit_publication_health(root, row, required=True)
            with self.assertRaisesRegex(ValueError, "missing or changed"):
                audit_publication_health(root, {}, required=True)
            (root / "vm1.stderr").write_text("modified log\n")
            with self.assertRaisesRegex(ValueError, "missing or changed"):
                audit_publication_health(root, row, required=True)
            (root / "vm1.stderr").write_text(
                "pvisor-physical-pool publication_disabled=true accounting_scope=local-pins\n"
            )
            row["publication_health"] = physical_publication_health(root)
            with self.assertRaisesRegex(ValueError, "missing or changed"):
                audit_publication_health(root, row, required=True)

    def test_historical_preflight_projection_preserves_original_report(self):
        report = dict(arms=["firecracker", "retired-control", "pvisor-ksm"])
        selected = select_arms(report, {"firecracker", "pvisor-ksm"}, ["firecracker", "pvisor-ksm"])
        self.assertEqual(report["arms"], ["firecracker", "retired-control", "pvisor-ksm"])
        self.assertEqual(selected["arms"], ["firecracker", "pvisor-ksm"])
        self.assertEqual(selected["selection"]["excluded"], ["retired-control"])
        with self.assertRaises(ValueError):
            select_arms(report, {"firecracker", "pvisor-ksm"}, None)
        for arms in ([], ["firecracker", "firecracker"], ["unknown"], ["retired-control"]):
            with self.assertRaises(ValueError):
                select_arms(report, {"firecracker", "pvisor-ksm"}, arms)
        with self.assertRaisesRegex(ValueError, "formal"):
            select_arms(report, {"firecracker", "pvisor-ksm"}, ["firecracker"], formal=True)

    def test_bracket_is_after_the_last_rebound_above_the_target(self):
        row = sustained_drop([(0, 400), (1, 250), (2, 300), (3, 250), (60, 240)], 400)
        self.assertTrue(row["reached"])
        self.assertEqual(row["crossing_interval_seconds"], [2, 3])

    def test_unreached_drop_is_censored_instead_of_imputed_as_sixty(self):
        row = sustained_drop([(0, 400), (30, 250), (60, 300)], 400)
        self.assertFalse(row["reached"])
        self.assertIsNone(row["crossing_interval_seconds"])

    def test_post_write_points_do_not_establish_an_idle_drop(self):
        row = sustained_drop([(-1, 300), (0, 400), (60, 300), (61, 200)], 400)
        self.assertFalse(row["reached"])


if __name__ == "__main__":
    unittest.main()
