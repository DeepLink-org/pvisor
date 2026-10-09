import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from publish_reference_campaign import paired_comparison


def values(value):
    return [dict(trial=i, value=value) for i in range(30)]


class ReferenceCampaignTests(unittest.TestCase):
    def test_matching_rounds_support_a_direction_with_confidence(self):
        result = paired_comparison(values(10), values(20), iterations=100)
        assert result["difference_ms"] == -10
        assert result["ci95_high_ms"] < 0 and result["conclusion"] == "candidate faster"

    def test_duplicate_or_missing_pairs_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "matching trial IDs"):
            paired_comparison(values(10), values(20)[:-1])
        duplicate = values(10)
        duplicate[-1]["trial"] = 0
        with self.assertRaisesRegex(ValueError, "unique"):
            paired_comparison(duplicate, values(20))

    def test_separated_distribution_does_not_receive_one_median_ranking(self):
        separated = [dict(trial=i, value=10 if i < 20 else 40) for i in range(30)]
        result = paired_comparison(separated, values(20), iterations=100)
        assert result["difference_ms"] == "" and "separated" in result["conclusion"]

    def test_input_verification_failure_cannot_publish_successful_timings(self):
        for change in [
            "failed",
            "absent-final",
            "different-digest",
            "missing-retained",
            "altered-retained",
        ]:
            with self.subTest(change=change):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    import json

                    from publish_reference_campaign import verify_input_records

                    initial = dict(input_manifest_sha256="same-input-digest", files=9)
                    expected = dict(state="passed", **initial)
                    report = dict(
                        input_verification=initial,
                        input_final_verification=dict(expected),
                        input_manifest_sha256="same-input-digest",
                    )
                    for name in ("input-verification.json", "input-final-verification.json"):
                        (tmp_path / name).write_text(json.dumps(expected))
                    verify_input_records(tmp_path / "report.json", report)
                    if change == "failed":
                        report["input_final_verification"]["state"] = "failed"
                    elif change == "absent-final":
                        report.pop("input_final_verification")
                    elif change == "different-digest":
                        report["input_manifest_sha256"] = "changed-input-digest"
                    elif change == "missing-retained":
                        (tmp_path / "input-final-verification.json").unlink()
                    else:
                        (tmp_path / "input-final-verification.json").write_text(
                            json.dumps(dict(state="failed"))
                        )
                    with self.assertRaises(ValueError):
                        verify_input_records(tmp_path / "report.json", report)

    def test_publication_rechecks_measured_stock_kernel_bytes(self):
        for fault in [None, "artifact", "row"]:
            with self.subTest(fault=fault):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    from firecracker_kernels import verify_kernel_receipt
                    from publish_reference_campaign import verify_kernel_records
                    from test_firecracker_kernels import prepare

                    receipt = prepare(tmp_path, "fc-system", True)
                    identity = verify_kernel_receipt(receipt, "fc-system")
                    report = dict(
                        arguments=dict(
                            fc_system_receipt=str(receipt), qemu_system_receipt=str(receipt)
                        ),
                        reference_kernels={
                            b: identity for b in ("fc-system", "qemu", "qemu-microvm")
                        },
                        rows=[
                            dict(backend=b, kernel_provenance=identity)
                            for b in ("fc-system", "qemu", "qemu-microvm")
                        ],
                    )
                    if fault == "artifact":
                        (receipt.parent / "vmlinuz").write_bytes(b"changed stock input")
                    if fault == "row":
                        report["rows"][1]["kernel_provenance"] = None
                    if fault:
                        with self.assertRaises(ValueError):
                            verify_kernel_records(report)
                    else:
                        verify_kernel_records(report)
