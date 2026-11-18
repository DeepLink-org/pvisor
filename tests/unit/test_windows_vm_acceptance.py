from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import unittest

from tests.fixtures import ROOT, workspace

SCRIPT = ROOT / "scripts/ci/windows_vm_acceptance.py"
spec = importlib.util.spec_from_file_location("windows_vm_acceptance", SCRIPT)
acceptance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(acceptance)
SHA = "a" * 40


def fixture(root):
    candidate = root / "inputs"
    evidence = root / "evidence"
    evidence.mkdir()
    for name in acceptance.INPUTS:
        path = candidate / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(f"frozen {name}".encode())
    frozen = acceptance.inputs(candidate, SHA)
    for suite in acceptance.COVERAGE:
        directory = evidence / f"windows-vm-whp-{suite}"
        directory.mkdir()
        (directory / "inputs.json").write_text(json.dumps(frozen))
        (directory / "remaining-processes.json").write_text("[]")
        if suite == "smoke":
            (directory / "smoke.log").write_text("\n".join(
                f"WHP_SMOKE cpus={cpus} exit={code} PASS"
                for cpus, code in [(1, "abort"), (1, "0"), (2, "0"), (2, "7"), (1, "255")]
            ))
        else:
            reports = ["vm-cli"] if suite == "vm-cli" else ["containers-1", "containers-2"]
            for report in reports:
                path = directory / report / "report.json"
                path.parent.mkdir()
                path.write_text(json.dumps({
                    "passed": True,
                    "binary_sha256": frozen["inputs_sha256"]["pvisor.exe"],
                    "firmware_sha256" if suite == "vm-cli" else "libkrunfw.dll_sha256": frozen["inputs_sha256"]["libkrunfw.dll"],
                    "guest_sha256" if suite == "vm-cli" else "pvisor-rootfs.bin.gz_sha256": frozen["inputs_sha256"]["vm-guest" if suite == "vm-cli" else "pvisor-rootfs.bin.gz"],
                    "cases" if suite == "vm-cli" else "commands": [{"passed": True}],
                }))
        receipt = acceptance.record(candidate, directory, SHA, suite)
        (directory / "acceptance.json").write_text(json.dumps(receipt))
    return candidate, evidence


class WindowsVmAcceptanceTests(unittest.TestCase):
    def test_all_suites_bind_the_original_candidate_bytes(self):
        with workspace() as (root, _):
            candidate, evidence = fixture(root)
            result = acceptance.qualify(candidate, evidence, SHA)
            self.assertEqual(result["status"], "PASS")
            self.assertEqual(result["coverage"], acceptance.COVERAGE)
            for name in acceptance.INPUTS:
                with self.subTest(input=name):
                    path = candidate / name
                    original = path.read_bytes()
                    path.write_bytes(original + b"tampered")
                    with self.assertRaises(ValueError):
                        acceptance.qualify(candidate, evidence, SHA)
                    with self.assertRaises(ValueError):
                        acceptance.record(candidate, evidence / "windows-vm-whp-smoke", SHA, "smoke")
                    path.write_bytes(original)

    def test_receipt_sha_suite_status_coverage_and_hashes_are_required(self):
        with workspace() as (root, _):
            candidate, evidence = fixture(root)
            for suite in acceptance.COVERAGE:
                path = evidence / f"windows-vm-whp-{suite}" / "acceptance.json"
                original = json.loads(path.read_text())
                for field, value in [
                    ("source_sha", "b" * 40), ("suite", "other"), ("status", "FAIL"),
                    ("coverage", "compile-only"), ("inputs_sha256", {}),
                ]:
                    with self.subTest(suite=suite, field=field):
                        path.write_text(json.dumps({**original, field: value}))
                        with self.assertRaises(ValueError):
                            acceptance.qualify(candidate, evidence, SHA)
                path.unlink()
                with self.assertRaises(FileNotFoundError):
                    acceptance.qualify(candidate, evidence, SHA)
                path.write_text(json.dumps(original))

    def test_suite_failure_or_remaining_process_prevents_receipt(self):
        with workspace() as (root, _):
            candidate, evidence = fixture(root)
            for suite in acceptance.COVERAGE:
                directory = evidence / f"windows-vm-whp-{suite}"
                cleanup = directory / "remaining-processes.json"
                cleanup.write_text('[{"ProcessId":42}]')
                with self.assertRaises(ValueError):
                    acceptance.record(candidate, directory, SHA, suite)
                cleanup.write_text("[]")
                if suite == "smoke":
                    (directory / "smoke.log").write_text("WHP_SMOKE cpus=1 exit=0 PASS")
                else:
                    report = directory / ("vm-cli" if suite == "vm-cli" else "containers-2") / "report.json"
                    report.write_text(json.dumps({"passed": True, "commands": [], "cases": []}))
                with self.assertRaises(ValueError):
                    acceptance.record(candidate, directory, SHA, suite)

    def test_driver_report_cannot_claim_another_binary_was_tested(self):
        with workspace() as (root, _):
            candidate, evidence = fixture(root)
            for suite in ["vm-cli", "containers"]:
                directory = evidence / f"windows-vm-whp-{suite}"
                path = directory / ("vm-cli" if suite == "vm-cli" else "containers-1") / "report.json"
                report = json.loads(path.read_text())
                report["binary_sha256"] = "0" * 64
                path.write_text(json.dumps(report))
                with self.assertRaises(ValueError):
                    acceptance.record(candidate, directory, SHA, suite)

    def test_failed_command_withdraws_a_stale_pass_receipt(self):
        with workspace() as (root, _):
            candidate, evidence = fixture(root)
            output = root / "qualification.json"
            output.write_text('{"status":"PASS"}')
            result = subprocess.run([
                sys.executable, str(SCRIPT), "qualify", "--inputs", str(candidate),
                "--evidence", str(evidence), "--source-sha", "b" * 40, "--output", str(output),
            ], capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_workflow_records_only_after_cleanup_and_requires_all_jobs(self):
        workflow = (ROOT / ".github/workflows/windows-vm.yml").read_text()
        self.assertLess(workflow.index("Record diagnostics and stop"), workflow.index("Confirm suite, cleanup"))
        self.assertIn("if: ${{ success() }}\n        shell: pwsh\n        run: |\n          python scripts/ci/windows_vm_acceptance.py record", workflow)
        self.assertIn("needs: [native, firmware, whp]", workflow)
        self.assertIn("name: windows-vm-qualified-candidate", workflow)
