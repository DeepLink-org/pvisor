import copy
import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from publish_apply_concurrency import planned_attempts, publish
from reference_baselines import digest


def cohort():
    return dict(
        benchmark_id="B-APPLY",
        concurrent_apply_protocol=dict(files=10000, repetitions=3),
        rows=[
            dict(files=10000, trial=i, workload="conflict-during-target-writes", backend="staged")
            for i in range(3)
        ],
    )


def retained_cohort(tmp_path):
    root = tmp_path / "cohort"
    (root / "bin").mkdir(parents=True)
    (root / "bin/pvisor").write_bytes(b"synthetic test executable, never run")
    (root / "source-manifest.json").write_text("[]\n")
    receipt = dict(
        pvisor_sha256=digest(root / "bin/pvisor"),
        source_manifest_sha256=digest(root / "source-manifest.json"),
    )
    (root / "build-receipt.json").write_text(json.dumps(receipt))
    (root / "harness").mkdir()
    (root / "harness/fixture.py").write_text("synthetic test fixture\n")
    report = cohort()
    report.update(
        recorded_at="2026-10-06T00:00:00+00:00",
        binary_build=receipt,
        binary_sha256=receipt["pvisor_sha256"],
        cli_arguments=dict(cpu_affinity="0,1"),
        harness_sha256={"fixture.py": digest(root / "harness/fixture.py")},
    )
    report["concurrent_apply_protocol"]["files"] = 1000
    for row in report["rows"]:
        trial = root / "trials" / str(row["trial"])
        (trial / "workspace/files").mkdir(parents=True)
        (trial / "stage/upper/files").mkdir(parents=True)
        for index in range(1000):
            (trial / "workspace/files" / f"f{index:06d}").write_text(
                f"new-{index}\n" if index == 0 else f"old-{index}\n"
            )
            (trial / "stage/upper/files" / f"f{index:06d}").write_text(f"new-{index}\n")
        content = f"concurrent-host-edit-{row['trial']}\n"
        (trial / "workspace/files/f000999").write_text(content)
        row.update(
            files=1000,
            logs=str(trial),
            state_at_injection="prepared",
            injected_path="files/f000999",
            injected_content=content,
            already_applied_before_injection=1,
            exit_code=1,
            host_edit_preserved=True,
            final_injected_content=content,
            ledger_final="prepared",
            detected_conflict=True,
            correctness="passed",
        )
        (trial / "injection.json").write_text(json.dumps(row))
        (trial / "result.json").write_text(json.dumps(row))
        (trial / "stage/apply-ledger.json").write_text(
            json.dumps({"records": [{"state": "prepared"}]})
        )
        (trial / "apply.stderr").write_text(
            "target changed after staging at files/f000999; refusing to overwrite concurrent changes"
        )
    path = root / "report.json"
    path.write_text(json.dumps(report))
    return path, report


class ApplyConcurrencyPublicationTests(unittest.TestCase):
    def test_successes_and_failures_are_both_planned_attempts(self):
        report = cohort()
        failure = report["rows"].pop()
        report["capabilities"] = {"apply/concurrent-conflicts": {"failures": [failure]}}
        count, rows = planned_attempts(report)
        assert count == 10000 and {row["trial"] for row in rows} == {0, 1, 2}

    def test_incomplete_or_mismatched_probes_cannot_be_published(self):
        for mutation in [
            "missing",
            "duplicate",
            "wrong-count",
            "wrong-workload",
            "wrong-backend",
            "too-small",
            "too-few",
            "wrong-id",
        ]:
            with self.subTest(mutation=mutation):
                report = copy.deepcopy(cohort())
                if mutation == "missing":
                    report["rows"].pop()
                elif mutation == "duplicate":
                    report["rows"].append(report["rows"][0])
                elif mutation == "wrong-count":
                    report["rows"][0]["files"] = 1000
                elif mutation == "wrong-workload":
                    report["rows"][0]["workload"] = "conflict"
                elif mutation == "wrong-backend":
                    report["rows"][0]["backend"] = "native"
                elif mutation == "too-small":
                    report["concurrent_apply_protocol"]["files"] = 10
                elif mutation == "too-few":
                    report["concurrent_apply_protocol"]["repetitions"] = 1
                else:
                    report["benchmark_id"] = "B-FS-DIAG"
                with self.assertRaises(ValueError):
                    planned_attempts(report)

    def test_complete_retained_refusals_publish_counts_without_latency(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            path, _ = retained_cohort(tmp_path)
            result = publish(path, tmp_path / "output")
            assert (
                result["valid_injections"]
                == result["conflicts_detected"]
                == result["host_edits_preserved"]
                == 3
            )
            assert result["silent_overwrites"] == result["unknown_probes"] == 0
            assert "wall_ms" not in result

    def test_tampered_retained_evidence_cannot_publish_success(self):
        for mutation in [
            "target",
            "ledger",
            "injection",
            "outcome",
            "harness",
            "binary",
            "fake-success",
            "upper",
        ]:
            with self.subTest(mutation=mutation):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    path, report = retained_cohort(tmp_path)
                    row = report["rows"][0]
                    trial = Path(row["logs"])
                    if mutation == "target":
                        (trial / "workspace/files/f000999").write_text("tampered")
                    elif mutation == "ledger":
                        (trial / "stage/apply-ledger.json").write_text(
                            json.dumps({"records": [{"state": "committed"}]})
                        )
                    elif mutation == "injection":
                        (trial / "injection.json").write_text(
                            json.dumps(row | {"already_applied_before_injection": 0})
                        )
                    elif mutation == "outcome":
                        (trial / "result.json").write_text(json.dumps(row | {"exit_code": 0}))
                    elif mutation == "harness":
                        (path.parent / "harness/fixture.py").write_text("modified")
                    elif mutation == "binary":
                        (path.parent / "bin/pvisor").write_bytes(b"modified")
                    elif mutation == "upper":
                        (trial / "stage/upper/files/f000999").write_text("tampered")
                    else:
                        row.update(
                            exit_code=0,
                            host_edit_preserved=False,
                            final_injected_content="new-999\n",
                            ledger_final="committed",
                            detected_conflict=False,
                        )
                        (trial / "workspace/files/f000999").write_text("new-999\n")
                        (trial / "stage/apply-ledger.json").write_text(
                            json.dumps({"records": [{"state": "committed"}]})
                        )
                        (trial / "result.json").write_text(json.dumps(row))
                        (trial / "apply.stderr").write_text("")
                        path.write_text(json.dumps(report))
                    with self.assertRaises(ValueError):
                        publish(path, tmp_path / "output")
