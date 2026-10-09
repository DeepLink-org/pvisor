import copy
import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from publish_vm_memory import publish, validate_cohort


def cohort():
    phase = dict(
        cgroup="/owned",
        current_bytes=100,
        stat=dict(anon=50, file=40, kernel=10),
        cpu=dict(usage_usec=100),
        events=dict(oom=0, oom_kill=0),
    )
    report = dict(
        benchmark_id="B-VM-MEMORY",
        mechanism="current SDK whole-VM offload",
        arguments=dict(samples=30, warmups=3),
        failures=[],
        rows=[],
    )
    for trial in range(30):
        for pattern in ("repeated", "random"):
            for compressed in (False, True):
                row = dict(
                    active=copy.deepcopy(phase),
                    offloaded=copy.deepcopy(phase),
                    restored=copy.deepcopy(phase),
                    backed_bytes=256 * 1024**2,
                    offload_ms=1,
                    offload_resume_ms=1,
                    baseline_read_ms=1,
                    restored_read_ms=1,
                )
                proof = dict(
                    schema="pvisor-live-offload/v1",
                    correctness="passed",
                    pattern=pattern,
                    seed=20261006 + trial + 3,
                    compressed=compressed,
                    samples=1,
                    warmups=0,
                    guest_data_bytes=64 * 1024**2,
                    memory_mib=256,
                    cpus=2,
                    settle_ms=2000,
                    long_pause_seconds=0,
                    stress=False,
                    cancel_while=None,
                    rows=[copy.deepcopy(row)],
                )
                report["rows"].append(
                    row
                    | dict(
                        pattern=pattern,
                        compressed=compressed,
                        trial=trial,
                        correctness="passed",
                        report=proof,
                    )
                )
    return report


def guarded_cohort():
    report = cohort()
    report["arguments"]["warmups"] = 0
    report["protocol"] = {"host_guard": {"enabled": True}}
    report["attempts"] = []
    for row in report["rows"]:
        row["report"]["seed"] -= 3
        row["logs"] = f"trials/{row['trial']}-{row['pattern']}-{row['compressed']}"
        report["attempts"].append(
            dict(
                pattern=row["pattern"],
                compressed=row["compressed"],
                trial=row["trial"],
                logs=row["logs"],
                result=copy.deepcopy(row),
                correctness="passed",
                host_admitted=True,
                unit_quiescent=True,
                deadline=False,
                returncode=0,
                host_interference=[],
                host_guard_errors=[],
            )
        )
    return report


class MemoryPublicationTests(unittest.TestCase):
    def test_partial_or_duplicate_live_memory_cannot_be_published(self):
        report = cohort()
        assert validate_cohort(report)
        for mutation in (
            "missing",
            "duplicate",
            "failed",
            "seed",
            "timing",
            "memory",
            "transient_oom",
        ):
            changed = copy.deepcopy(report)
            if mutation == "missing":
                changed["rows"].pop()
            elif mutation == "duplicate":
                changed["rows"][-1] = copy.deepcopy(changed["rows"][0])
            elif mutation == "failed":
                changed["failures"].append(dict(error="failed trial"))
            elif mutation == "seed":
                changed["rows"][0]["report"]["seed"] = 0
            elif mutation == "timing":
                changed["rows"][0]["offload_ms"] = 0
            elif mutation == "memory":
                changed["rows"][0]["offloaded"]["current_bytes"] = 1
            else:
                changed["rows"][0]["samples"] = [dict(events=dict(oom=1, oom_kill=0))]
            with self.assertRaises(ValueError):
                validate_cohort(changed)

    def test_matching_sdk_proof_cannot_publish_invalid_numeric_metrics(self):
        for value in [float("nan"), float("inf"), -1, True]:
            with self.subTest(value=value):
                report = cohort()
                row = report["rows"][0]
                row["offload_ms"] = row["report"]["rows"][0]["offload_ms"] = value
                with self.assertRaises(ValueError):
                    validate_cohort(report)

    def test_memory_component_cannot_be_nonfinite_or_negative(self):
        for value in [float("nan"), float("inf"), -1]:
            with self.subTest(value=value):
                report = cohort()
                row = report["rows"][0]
                row["offloaded"]["stat"]["file"] = row["report"]["rows"][0]["offloaded"]["stat"][
                    "file"
                ] = value
                with self.assertRaises(ValueError):
                    validate_cohort(report)

    def test_guarded_cohort_requires_all_clean_attempts(self):
        for mutation in [
            "missing",
            "duplicate",
            "warmup",
            "interference",
            "guard_error",
            "admission",
            "quiescence",
            "stopped",
            "result",
            "missing_guard_status",
            "disabled",
        ]:
            with self.subTest(mutation=mutation):
                report = guarded_cohort()
                assert validate_cohort(report)
                attempt = report["attempts"][0]
                if mutation == "missing":
                    report["attempts"].pop()
                elif mutation == "duplicate":
                    report["attempts"][-1] = copy.deepcopy(attempt)
                elif mutation == "warmup":
                    report["arguments"]["warmups"] = 1
                elif mutation == "interference":
                    attempt["host_interference"] = [{"jobs": ["foreign VM"]}]
                elif mutation == "guard_error":
                    attempt["host_guard_errors"] = ["guard failed"]
                elif mutation == "admission":
                    attempt["host_admitted"] = False
                elif mutation == "quiescence":
                    attempt["unit_quiescent"] = False
                elif mutation == "stopped":
                    report["stopped"] = "interference"
                elif mutation == "result":
                    attempt["result"]["offload_ms"] = 2
                elif mutation == "missing_guard_status":
                    del attempt["host_guard_errors"]
                else:
                    report["protocol"]["host_guard"]["enabled"] = False
                with self.assertRaises(ValueError):
                    validate_cohort(report)

    def test_legacy_memory_does_not_acquire_new_guard_semantics(self):
        report = cohort()
        report["attempts"] = [dict(host_admitted=False, unit_quiescent=False)]
        assert validate_cohort(report)

    def test_thirty_sample_memory_gate_is_unchanged(self):
        report = cohort()
        report["arguments"]["samples"] = 29
        report["rows"] = [row for row in report["rows"] if row["trial"] < 29]
        with self.assertRaisesRegex(ValueError, ">=30"):
            validate_cohort(report)

    def test_sidecar_vetoes_all_passed_publication(self):
        for eligible in [False, "ineligible"]:
            with self.subTest(eligible=eligible):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    report = cohort()
                    assert validate_cohort(report)
                    path = tmp_path / "report.json"
                    path.write_text(json.dumps(report))
                    (tmp_path / "publication-eligibility.json").write_text(
                        json.dumps(
                            dict(
                                schema="pvisor-publication-eligibility/v1",
                                cohort=str(tmp_path),
                                eligible_for_public_causal_comparison=eligible,
                                reason="external VM overlap",
                            )
                        )
                    )
                    output = tmp_path / "public"
                    with self.assertRaisesRegex(
                        ValueError, "ineligible for public causal comparison"
                    ):
                        publish([path], output)
                    assert not output.exists()
                    assert not (tmp_path / "memory-output-evidence-audit.json").exists()
