import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from publication import distribution, publish


class PublicationTests(unittest.TestCase):
    def test_small_cohort_has_no_tail_estimate(self):
        result = distribution([3, 1, 2])
        assert result["p50"] == 2 and result["p95_reference"] == ""
        assert "p99" not in result

    def test_separated_clusters_report_counts_instead_of_one_median(self):
        result = distribution([10] * 20 + [30] * 10)
        assert result["p50"] == ""
        assert (result["low_n"], result["low_p50"], result["high_n"], result["high_p50"]) == (
            20,
            10,
            10,
            30,
        )

    def test_invalid_or_incomplete_evidence_is_rejected(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            report = {
                "rows": [],
                "arguments": {"modes": "ready", "backends": "native", "samples": "30"},
                "capabilities": {"ready/native": {"state": "available"}},
                "benchmark_ids": {"ready": "B-STARTUP"},
            }
            raw = tmp_path / "report.json"
            raw.write_text(json.dumps(report))
            with self.assertRaisesRegex(ValueError, "incomplete"):
                publish(raw, tmp_path / "public")
            with self.assertRaises(ValueError):
                distribution([float("nan")])

    def test_explicit_selection_publishes_only_complete_workloads(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            report = {
                "rows": [
                    {
                        "mode": "ready",
                        "backend": "native",
                        "trial": 0,
                        "correctness": "passed",
                        "ready_ms": 1,
                        "result_ms": 1,
                        "completion_ms": 2,
                    }
                ],
                "arguments": {"modes": "ready,tools", "backends": "native", "samples": "1"},
                "capabilities": {
                    "ready/native": {"state": "available"},
                    "tools/native": {"state": "available"},
                },
                "benchmark_ids": {"ready": "B-STARTUP", "tools": "B-AGENT-TASK"},
            }
            raw = tmp_path / "report.json"
            raw.write_text(json.dumps(report))
            with self.assertRaisesRegex(ValueError, "incomplete"):
                publish(raw, tmp_path / "all")
            rows = publish(raw, tmp_path / "selected", ["ready"])
            assert {r["mode"] for r in rows} == {"ready"}
            assert all(r["n"] == 1 for r in rows)

    def test_ready_only_publishes_readiness_without_inventing_completion(self):
        for fault in [None, "uncontrolled", "normal", "wrong-backend", "mixed"]:
            with self.subTest(fault=fault):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    rows = [
                        dict(
                            mode="ready",
                            backend="fc-system",
                            trial=i,
                            correctness="passed",
                            ready_ms=1,
                            result_ms=2,
                            completion_ms=None,
                            fc_ready_policy="ready-only",
                            controlled_sigterm=True,
                        )
                        for i in range(2)
                    ]
                    backend = "native" if fault == "wrong-backend" else "fc-system"
                    for row in rows:
                        row["backend"] = backend
                    if fault == "uncontrolled":
                        rows[0]["controlled_sigterm"] = False
                    if fault == "mixed":
                        rows[0]["completion_ms"] = 3
                    report = dict(
                        rows=rows,
                        arguments=dict(
                            modes="ready",
                            backends=backend,
                            samples="2",
                            fc_ready_policy="normal" if fault == "normal" else "ready-only",
                        ),
                        capabilities={f"ready/{backend}": dict(state="available")},
                        benchmark_ids=dict(ready="B-STARTUP"),
                    )
                    path = tmp_path / "report.json"
                    path.write_text(json.dumps(report))
                    if fault:
                        with self.assertRaisesRegex(ValueError, "checked FC ready-only"):
                            publish(path, tmp_path / "public")
                    else:
                        result = publish(path, tmp_path / "public")
                        assert {r["metric"] for r in result} == {"ready_ms", "result_ms"}
                        assert all(r["n"] == 2 for r in result)
