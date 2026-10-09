"""A purported FUSE control must prove both mounting and actual data service."""

import json
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest import mock

import filesystem_fuse_ab
from reference_baselines import digest, validate_passthrough_output


def evidence(stats=None, mount=" - fuse.pvisor-bench-native pvisor-bench-native rw"):
    values = {
        "lookup": 2048,
        "getattr": 2048,
        "read": 1024,
        "write": 256,
        "read_bytes": 64 * 1024 * 1024,
        "write_bytes": 256 * 64 * 1024,
    }
    values.update(stats or {})
    return (
        "PASSTHROUGH_MOUNT 123 1 0:1 / /mount rw"
        + mount
        + "\nPASSTHROUGH_STATS "
        + json.dumps(values)
    )


class FilesystemFuseAbTests(unittest.TestCase):
    def test_real_fuse_request_evidence_is_required(self):
        assert validate_passthrough_output(evidence())["lookup"] == 2048

    def test_bypass_or_partial_data_service_is_rejected(self):
        for fault in [
            "missing-mount",
            "not-fuse",
            "missing-stats",
            "duplicate-stats",
            "no-lookup",
            "no-read",
            "no-write",
            "short-read",
            "short-write",
        ]:
            with self.subTest(fault=fault):
                value = evidence()
                if fault == "missing-mount":
                    value = value.split("\n", 1)[1]
                elif fault == "not-fuse":
                    value = evidence(mount=" - ext4 /dev/loop0 rw")
                elif fault == "missing-stats":
                    value = value.split("\n", 1)[0]
                elif fault == "duplicate-stats":
                    value += "\n" + value.split("\n", 1)[1]
                else:
                    fields = {
                        "no-lookup": "lookup",
                        "no-read": "read",
                        "no-write": "write",
                        "short-read": "read_bytes",
                        "short-write": "write_bytes",
                    }
                    value = evidence({fields[fault]: 0})
                with self.assertRaises(ValueError):
                    validate_passthrough_output(value)

    def test_failed_control_retains_report_and_always_checks_final_inputs(self):
        for fault in ["preflight", "final-input"]:
            with self.subTest(fault=fault):
                with ExitStack() as resources:
                    tmp_path = Path(
                        resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
                    ).resolve()
                    assets = tmp_path / "assets"
                    (assets / "rootfs/bench/harness/v1").mkdir(parents=True)
                    (assets / "assets.json").write_text("{}")
                    for p in [
                        assets / "rootfs/bench/reference_workload.py",
                        assets / "rootfs/bench/harness/v1/workload.py",
                    ]:
                        p.write_text("fixture")
                    product = tmp_path / "product"
                    product.mkdir()
                    binary = product / "pvisor"
                    binary.write_bytes(b"product")
                    receipt = product / "build-receipt.json"
                    receipt.write_text("{}")
                    (product / "source-manifest.json").write_text("[]")
                    driver_root = tmp_path / "driver"
                    driver_root.mkdir()
                    driver = driver_root / "fuse-passthrough"
                    driver.write_bytes(b"driver")
                    driver_receipt = driver_root / "build-receipt.json"
                    driver_receipt.write_text("{}")
                    (driver_root / "source-manifest.json").write_text("[]")
                    (driver_root / "source").mkdir()
                    output = tmp_path / ".data/cohort"
                    checks = []
                    trials = []
                    inputs = dict(
                        binary_build=dict(pvisor_sha256=digest(binary)),
                        driver_build={},
                        reference={},
                    )

                    def verify(args):
                        checks.append(args.assets)
                        if len(checks) == 2 and fault == "final-input":
                            raise ValueError("changed final input")
                        return inputs

                    def run(args, metadata, backend, mode, trial):
                        trials.append((backend, trial))
                        if fault == "preflight" and backend == "pvisor-fuse":
                            raise ValueError("not actual FUSE")
                        return dict(
                            backend=backend,
                            trial=trial,
                            completion_ms=10,
                            ready_ms=1,
                            result={
                                "filesystem": {
                                    op: {"worker_ms": 2} for op in filesystem_fuse_ab.WORKLOADS
                                }
                            },
                        )

                    resources.enter_context(
                        mock.patch.object(filesystem_fuse_ab, "verified_inputs", verify)
                    )
                    resources.enter_context(
                        mock.patch.object(
                            filesystem_fuse_ab, "verify_driver_receipt", lambda *args: {}
                        )
                    )
                    resources.enter_context(mock.patch.object(filesystem_fuse_ab, "run_trial", run))
                    resources.enter_context(
                        mock.patch.object(
                            sys,
                            "argv",
                            [
                                "fuse-probe",
                                "--assets",
                                str(assets),
                                "--binary",
                                str(binary),
                                "--build-receipt",
                                str(receipt),
                                "--fuse-driver",
                                str(driver),
                                "--driver-build-receipt",
                                str(driver_receipt),
                                "--output",
                                str(output),
                                "--samples",
                                "1",
                                "--warmups",
                                "0",
                            ],
                        )
                    )
                    with self.assertRaises(SystemExit) as error:
                        filesystem_fuse_ab.main()
                    assert error.exception.code == 1 and len(checks) == 2
                    report = json.loads((output / "report.json").read_text())
                    assert report["state"] == "failed"
                    assert "summary" not in report and not (output / "summary.tsv").exists()
                    if fault == "preflight":
                        assert len(report["rows"]) == 0 and len(trials) == 4
                        assert report["input_final_verification"]["state"] == "passed"
                    else:
                        assert len(report["rows"]) == 4 and len(trials) == 8
                        assert report["input_final_verification"]["state"] == "failed"
