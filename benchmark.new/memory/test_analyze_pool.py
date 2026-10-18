"""Formal cohort and paired-round inference contracts, without VM measurements."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

import analyze_pool as subject


def cases():
    result = []
    for n in range(5):
        for pattern in subject.PATTERNS:
            for arm in subject.ARMS:
                offset = -20 if arm == "pvisor-physical" else 0
                cost = 100 + n * 10 + offset
                row = dict(
                    round=n,
                    arm=arm,
                    pattern=pattern,
                    idle_cpu_seconds=cost,
                    first_full_read_ms=cost,
                    global_ksmd_cpu_seconds=cost,
                )
                row.update(
                    whole_condition_peak_mib=cost,
                    observed_idle_peak_mib=cost,
                    post_write_cpu_seconds=cost,
                )
                row["startup_seconds_until_all_ready"] = cost
                for role in ("vm", "control", "pool", "runtime"):
                    for kind in ("user", "system"):
                        row[f"{role}_{kind}_cpu_seconds"] = cost
                for phase in [f"idle-{s}" for s in subject.BOUNDARIES] + [
                    "readback",
                    "cow-25",
                    "cow-100",
                    "post-write-30",
                ]:
                    row[phase] = dict(charged_mib=cost, pss_mib=cost)
                    row[phase].update(
                        {
                            f"{kind}_mib": cost
                            for kind in ("anon", "file", "kernel", "pagetables", "slab")
                        }
                    )
                result.append(row)
    return result


class FormalAnalysisTests(unittest.TestCase):
    def test_crossing_brackets_keep_censoring_and_sampling_uncertainty(self):
        rows = [
            dict(
                round=n,
                pattern="same",
                arm="pool",
                sustained_drop_128_mib={
                    kind: dict(reached=True, crossing_interval_seconds=[2, 2.05])
                    for kind in ("charged", "pss")
                },
            )
            for n in range(5)
        ]
        result = subject.crossing_distributions(rows, 5)
        self.assertEqual(result[0]["median_crossing_bracket_seconds"], [2, 2.05])
        self.assertEqual(result[0]["conservative_bootstrap_95_seconds"], [2, 2.05])
        rows[0]["sustained_drop_128_mib"]["charged"] = dict(
            reached=False, crossing_interval_seconds=None
        )
        result = subject.crossing_distributions(rows, 5)
        self.assertEqual(result[0]["censored"], 1)
        self.assertIsNone(result[0]["median_crossing_bracket_seconds"])
        self.assertIsNone(result[0]["conservative_bootstrap_95_seconds"])
        rows[0]["sustained_drop_128_mib"]["charged"]["crossing_interval_seconds"] = [60, 60]
        with self.assertRaisesRegex(ValueError, "imputed"):
            subject.crossing_distributions(rows, 5)

    def test_library_rounds_are_separate_from_static_content_matrix(self):
        from library_protocol import VARIANTS

        originals = [
            r for r in cases() if r["pattern"] == "random-shared" and r["arm"] == "pvisor-off"
        ]
        rows = []
        for row in originals:
            for variant in VARIANTS:
                rows.append({**copy.deepcopy(row), "arm": variant})
        _, comparisons = subject.summarize(rows, 5, campaign="library")
        self.assertTrue(comparisons)
        self.assertTrue(all(r["paired_bootstrap_95"] == [0, 0] for r in comparisons))
        with self.assertRaises(ValueError):
            subject.summarize(rows, 5)
        with self.assertRaises(ValueError):
            subject.summarize(cases(), 5, campaign="library")

    def test_paired_crossing_comparison_never_imputes_a_missed_target(self):
        rows = cases()
        for row in rows:
            bounds = [2, 3] if row["arm"] == "pvisor-physical" else [20, 21]
            row["sustained_drop_128_mib"] = {
                kind: dict(reached=True, crossing_interval_seconds=bounds)
                for kind in ("charged", "pss")
            }
        result = subject.crossing_comparisons(rows, 5)
        self.assertTrue(result)
        self.assertTrue(all(r["median_difference_bracket_seconds"] == [-19, -17] for r in result))
        self.assertTrue(
            all(r["conservative_paired_bootstrap_95_seconds"] == [-19, -17] for r in result)
        )
        next(r for r in rows if r["arm"] == "pvisor-physical")["sustained_drop_128_mib"][
            "charged"
        ] = {"reached": False, "crossing_interval_seconds": None}
        result = subject.crossing_comparisons(rows, 5)
        censored = [r for r in result if r["censored_pairs"]]
        self.assertTrue(censored)
        self.assertTrue(all(r["median_difference_bracket_seconds"] is None for r in censored))
        self.assertTrue(
            all(r["conservative_paired_bootstrap_95_seconds"] is None for r in censored)
        )

    def test_bootstrap_preserves_pairs_instead_of_mixing_round_noise(self):
        _, comparisons = subject.summarize(cases(), 5)
        selected = [r for r in comparisons if r["arm"] == "pvisor-physical"]
        self.assertTrue(selected)
        for row in selected:
            self.assertEqual(row["paired_differences"], [-20] * 5)
            self.assertEqual(row["paired_bootstrap_95"], [-20, -20])

    def test_missing_duplicate_or_invalid_observations_cannot_be_aggregated(self):
        for failure in ("missing", "duplicate", "negative", "nan", "few"):
            with self.subTest(failure=failure):
                rows = cases()
                if failure == "missing":
                    rows.pop()
                if failure == "duplicate":
                    rows[-1] = copy.deepcopy(rows[0])
                if failure == "negative":
                    rows[0]["idle_cpu_seconds"] = -1
                if failure == "nan":
                    rows[0]["idle_cpu_seconds"] = float("nan")
                with self.assertRaises(ValueError):
                    subject.summarize(rows, 4 if failure == "few" else 5)

    def test_preflight_and_incomplete_campaigns_cannot_become_formal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for complete, preflight, samples in (
                (False, False, 5),
                (True, True, 1),
                (True, False, 4),
            ):
                with self.subTest(complete=complete, preflight=preflight, samples=samples):
                    (root / "report.json").write_text(
                        json.dumps(
                            dict(
                                complete=complete,
                                preflight=preflight,
                                samples=samples,
                                arms=subject.ARMS,
                                patterns=subject.PATTERNS,
                            )
                        )
                    )
                    with self.assertRaises(ValueError):
                        subject.audit(root, formal=True)


if __name__ == "__main__":
    unittest.main()
