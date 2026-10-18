"""Observed crossing brackets must preserve sampling limits and censoring."""

import unittest

from plot_pool import select_arms, sustained_drop


class DropObservationTests(unittest.TestCase):
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
