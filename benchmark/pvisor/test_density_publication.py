import copy
import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest import mock

from publish_density import (
    recover_last_samples,
    summarize,
    verify_execution_identities,
    verify_harness,
)
from reference_baselines import digest


def cohort():
    rows = []
    for trial in range(5):
        outcomes = [
            dict(
                correctness="passed",
                result=dict(
                    integrity="passed", bytes=32 * 1024**2, changes=4, token=f"{trial}-{i}"
                ),
            )
            for i in range(2)
        ]
        point = dict(current_bytes=100, peak_bytes=200, events=dict(oom=0, oom_kill=0))
        rows.append(
            dict(
                backend="vm",
                workload="useful",
                concurrency=2,
                trial=trial,
                correctness="passed",
                outcomes=outcomes,
                completed=2,
                ready=2,
                failed=0,
                all_ready=True,
                wall_ms=100,
                barrier=copy.deepcopy(point),
                after=copy.deepcopy(point),
                samples=[],
                budget_bytes=2048 * 1024**2,
            )
        )
    return dict(
        benchmark_id="B-DENSITY",
        arguments=dict(
            samples=5,
            backends="vm",
            workloads="useful",
            concurrencies="2",
            budget_mib=2048,
            cpu_affinity="0,1",
        ),
        rows=rows,
        failures=[],
    )


def execution_cohort(tmp_path, resources):
    resources.enter_context(
        mock.patch("publish_density.validate_bundle_execution", lambda *args: None)
    )
    report = cohort()
    for row in report["rows"]:
        for index, outcome in enumerate(row["outcomes"]):
            root = tmp_path / f"{row['trial']}-{index}"
            (root / "stage").mkdir(parents=True)
            outcome["logs"] = str(root)
            outcome["result"]["checksum"] = "verified"
            outcome["result"]["token"] = "same-timestamp"
            result = outcome["result"]
            ready = {key: result[key] for key in ("token", "bytes", "checksum")}
            (root / "stdout.log").write_text(
                "PVISOR_DENSITY_READY "
                + json.dumps(ready)
                + "\nPVISOR_DENSITY_RESULT "
                + json.dumps(result)
                + "\n"
            )
            (root / "stage/run-bundle.json").write_text(
                json.dumps(
                    dict(
                        run=dict(
                            state="completed",
                            exit_code=0,
                            run_id=f"run-{row['trial']}-{index}",
                            attempt_id=f"attempt-{row['trial']}-{index}",
                        )
                    )
                )
            )
    return report


class DensityPublicationTests(unittest.TestCase):
    def test_lost_reporter_remains_unknown_and_cannot_establish_capacity(self):
        report = cohort()
        lost = report["rows"].pop()
        report["failures"].append(
            {key: lost[key] for key in ("backend", "workload", "concurrency", "trial")}
            | dict(correctness="failed", error="reporter lost")
        )
        row = summarize(report)[0]
        assert row["validated_completed_tasks"] == 8 and row["unknown_tasks"] == 2
        assert row["known_failed_tasks"] == 0 and not row["observed_all_rounds_succeeded"]
        assert row["resource_evidence_unknown_batches"] == 1

    def test_transient_oom_invalidates_reliable_occupancy_even_if_payloads_finish(self):
        report = cohort()
        report["rows"][0]["samples"] = [
            dict(current_bytes=100, peak_bytes=300, events=dict(oom=1, oom_kill=1))
        ]
        row = summarize(report)[0]
        assert row["oom_batches"] == 1 and row["full_valid_no_oom_batches"] == 4
        assert row["observed_peak_max_mib"] == 300 / 1024**2

    def test_last_sampler_recovers_oom_but_cannot_invent_task_results(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            report = cohort()
            lost = report["rows"].pop()
            root = tmp_path / "failed"
            root.mkdir()
            sample = dict(
                current_bytes=100, peak_bytes=300, events=dict(oom=1, oom_kill=1), stat={}, cpu={}
            )
            (root / "last-memory.json").write_text(json.dumps(sample))
            report["failures"].append(
                {key: lost[key] for key in ("backend", "workload", "concurrency", "trial")}
                | dict(correctness="failed", logs=str(root))
            )
            assert "failed/last-memory.json" in recover_last_samples(report, tmp_path)
            row = summarize(report)[0]
            assert row["oom_batches"] == 1 and row["unknown_tasks"] == 2
            assert (
                row["resource_evidence_unknown_batches"] == 1
                and row["full_valid_no_oom_batches"] == 4
            )

    def test_changed_or_incomplete_retained_harness_cannot_be_published(self):
        for mutation in ["changed", "missing", "extra", "outside"]:
            with self.subTest(mutation=mutation):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    root = tmp_path / "harness"
                    root.mkdir()
                    worker = root / "worker.py"
                    worker.write_text("original worker\n")
                    report = dict(harness_sha256={"worker.py": digest(worker)})
                    verify_harness(report, tmp_path)
                    if mutation == "changed":
                        worker.write_text("different worker\n")
                    elif mutation == "missing":
                        worker.unlink()
                    elif mutation == "extra":
                        (root / "extra.py").write_text("different inventory\n")
                    else:
                        outside = tmp_path / "outside.py"
                        outside.write_text(worker.read_text())
                        worker.unlink()
                        worker.symlink_to(outside)
                    with self.assertRaises(ValueError):
                        verify_harness(report, tmp_path)

    def test_inconsistent_density_evidence_cannot_be_published(self):
        for mutation in ["missing", "duplicate", "token", "completion", "budget"]:
            with self.subTest(mutation=mutation):
                report = cohort()
                if mutation == "missing":
                    report["rows"].pop()
                elif mutation == "duplicate":
                    report["rows"][-1] = copy.deepcopy(report["rows"][0])
                elif mutation == "token":
                    report["rows"][0]["outcomes"][1]["result"]["token"] = "0-0"
                elif mutation == "completion":
                    report["rows"][0]["completed"] = 1
                else:
                    report["rows"][0]["budget_bytes"] = 4096 * 1024**2
                with self.assertRaises(ValueError):
                    summarize(report)

    def test_timestamp_collision_requires_unique_retained_executions(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            report = execution_cohort(tmp_path, resources)
            with self.assertRaisesRegex(ValueError, "duplicate completed-task token"):
                summarize(report)
            assert len(verify_execution_identities(report, tmp_path)) == 20
            assert summarize(report)[0]["validated_completed_tasks"] == 10

    def test_execution_identity_does_not_accept_mismatched_evidence(self):
        for mutation in [
            "duplicate-run",
            "different-result",
            "duplicate-marker",
            "failed-run",
            "outside",
        ]:
            with self.subTest(mutation=mutation):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    report = execution_cohort(tmp_path, resources)
                    first, second = report["rows"][0]["outcomes"]
                    root = Path(second["logs"])
                    bundle = root / "stage/run-bundle.json"
                    if mutation == "duplicate-run":
                        bundle.write_bytes(
                            (Path(first["logs"]) / "stage/run-bundle.json").read_bytes()
                        )
                    elif mutation == "different-result":
                        second["result"]["changes"] = 99
                    elif mutation == "duplicate-marker":
                        stdout = root / "stdout.log"
                        stdout.write_text(stdout.read_text() + stdout.read_text())
                    elif mutation == "failed-run":
                        value = json.loads(bundle.read_text())
                        value["run"]["exit_code"] = 1
                        bundle.write_text(json.dumps(value))
                    else:
                        outside = tmp_path.parent / (tmp_path.name + "-outside.json")
                        outside.write_bytes(bundle.read_bytes())
                        bundle.unlink()
                        bundle.symlink_to(outside)
                    with self.assertRaises(ValueError):
                        verify_execution_identities(report, tmp_path)
