"""Synthetic evidence contracts for the independent post-measurement auditor."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "fs_v2_audit_for_tests", HERE / "workloads_v2_audit.py"
)
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


class AuditContracts(unittest.TestCase):
    def test_actual_budget_requires_all_declared_controls(self):
        declared = dict(cpus=[0, 1], quota="200000 100000", memory_bytes=2 << 30, swap_bytes=0)
        state = {
            "affinity": [0, 1],
            "cpu.max": "200000 100000",
            "memory.max": str(2 << 30),
            "memory.swap.max": "0",
        }
        AUDIT.budget(state, declared)
        for key, altered in [
            ("affinity", [0, 1, 2]),
            ("cpu.max", "max 100000"),
            ("memory.max", "max"),
            ("memory.swap.max", "max"),
        ]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                AUDIT.budget({**state, key: altered}, declared)

    def test_row_uses_frozen_validator_and_rejects_nonphysical_clock_types(self):
        row = dict(
            id="M06",
            round=1,
            correctness=True,
            setup_ns=0,
            work_ns=10,
            final_sync_ns=5,
            completed_ns=15,
            validation_ns=1,
            foreground_read_ns=0,
            bytes_written=4 << 20,
            bytes_read=0,
            write_ops=4096,
            read_ops=0,
            metadata_ops=0,
            periodic_fsyncs=0,
            final_fsyncs=2,
            checked_closes=2,
            files_verified=1,
            bytes_verified=4 << 20,
        )
        observed = []

        def frozen_validator(value, case):
            observed.append((value, case))
            AUDIT.require(
                value["completed_ns"] == value["work_ns"] + value["final_sync_ns"], "partition"
            )

        AUDIT.row_contract(row, "M06", frozen_validator)
        self.assertEqual(observed, [(row, "M06")])
        for key, value in [
            ("work_ns", True),
            ("work_ns", -1),
            ("completed_ns", 16),
            ("final_fsyncs", 1),
        ]:
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                AUDIT.row_contract({**row, key: value}, "M06", frozen_validator)

    def test_full_workflow_contract_keeps_stream_and_publish_barriers(self):
        row = dict(
            round=3,
            write_ops=128 + 32768,
            bytes_written=32 * (1 << 20) + 128 * 4096,
            periodic_fsyncs=0,
            final_fsyncs=1188,
        )
        AUDIT.scenario_contract(row, "S03")
        for key, value in [("write_ops", 128), ("bytes_written", 128 * 4096), ("final_fsyncs", 0)]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                AUDIT.scenario_contract({**row, key: value}, "S03")

    def test_only_explicit_kernel_failures_are_classified(self):
        for line in (
            "Kernel panic - not syncing",
            "BUG: unable to handle",
            "worker: page allocation failure: order:2",
            "WORKLOADS_V2_ERROR: failed",
        ):
            self.assertIsNotNone(AUDIT.FATAL.search(line))
        for line in (
            'WORKLOADS_V2_FIRMWARE "/firmware.so"',
            "warning: unused variable",
            "error_injection_disabled=true",
            "no kernel errors found",
        ):
            self.assertIsNone(AUDIT.FATAL.search(line))

    def semspec_fixture(self, batch):
        command = f"/usr/bin/python3 {batch}/harness/workloads_v2_run.py --measure {batch}"
        save(batch / "in.json", {"filesystem-v2-shallow": "$ " + command})
        raw = dict(trials=2, failed=1)
        log = batch / "in.bench.logs/0001.log"
        save(log, raw)
        export = dict(
            format=1,
            template=str(batch / "in.json"),
            cwd=str(batch),
            commands=[dict(command=command, kind="script-row", raw=raw, value=raw, log=str(log))],
            rows=[dict(row="filesystem-v2-shallow", command=command, expression=command)],
        )
        save(batch / "in.bench.json", export)
        return export

    def test_semspec_binding_retains_failed_count_and_does_not_invent_exitcode(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            self.semspec_fixture(batch)
            result = AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])
            self.assertIsNone(result["explicit_returncode"])
            self.assertTrue(result["success_inferred_from_checked_script_row_export"])
            save(
                batch / "semspec-execution.json",
                dict(
                    command=[
                        "/tools/semspec",
                        "bench",
                        "--cwd",
                        str(batch),
                        str(batch / "in.json"),
                    ],
                    cwd="/workspace",
                    returncode=0,
                ),
            )
            self.assertEqual(
                AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])[
                    "explicit_returncode"
                ],
                0,
            )
            save(
                batch / "semspec-execution.json",
                dict(
                    command=["/tools/semspec", "bench", str(batch / "in.json")],
                    cwd="/workspace",
                    returncode=1,
                ),
            )
            with self.assertRaises(ValueError):
                AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])

    def test_semspec_rejects_other_command_or_silent_failure_zero(self):
        for modification in ("command", "raw-failed-zero"):
            with (
                self.subTest(modification=modification),
                tempfile.TemporaryDirectory() as temporary,
            ):
                batch = Path(temporary)
                export = self.semspec_fixture(batch)
                if modification == "command":
                    export["commands"][0]["command"] += " --other-batch"
                else:
                    export["commands"][0]["raw"]["failed"] = 0
                save(batch / "in.bench.json", export)
                with self.assertRaises(ValueError):
                    AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])

    def test_literal_relative_semspec_arguments_resolve_against_actual_invocation_cwd(self):
        with tempfile.TemporaryDirectory() as temporary:
            invocation_cwd = Path(temporary)
            batch = invocation_cwd / "evidence/batch"
            self.semspec_fixture(batch)
            receipt = dict(
                command=[
                    "taskset",
                    "-c",
                    "2,3",
                    "target/debug/semspec",
                    "bench",
                    "--cwd",
                    "evidence/batch",
                    "evidence/batch/in.json",
                ],
                cwd=str(invocation_cwd),
                returncode=0,
            )
            save(batch / "semspec-execution.json", receipt)
            result = AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])
            self.assertEqual(result["explicit_returncode"], 0)
            self.assertEqual(json.loads((batch / "semspec-execution.json").read_text()), receipt)
            receipt["command"][-1] = "evidence/different/in.json"
            save(batch / "semspec-execution.json", receipt)
            with self.assertRaises(ValueError):
                AUDIT.semspec_evidence(batch, [dict(valid=True), dict(valid=False)])

    def test_relative_identity_paths_cannot_escape_their_root(self):
        root = Path("/frozen/batch")
        self.assertEqual(AUDIT.checked_path(root, "harness/source.rs"), root / "harness/source.rs")
        for relative in ("../outside", "/outside", "harness/../../outside"):
            with self.subTest(relative=relative), self.assertRaises(ValueError):
                AUDIT.checked_path(root, relative)

    def test_one_failed_case_retains_other_complete_case_and_actual_rows(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            state = {
                "affinity": [0, 1],
                "cpu.max": "200000 100000",
                "memory.max": str(2 << 30),
                "memory.swap.max": "0",
            }
            declared = dict(
                cpus=[0, 1],
                quota="200000 100000",
                memory_bytes=2 << 30,
                swap_bytes=0,
                guest_mib=512,
                guest_vcpus=2,
            )
            common_env = {
                "PVISOR_VM_FS_HOST_WRITEBACK": "0",
                "PVISOR_VM_FS_LOWER_DATA_CACHE": "0",
                "PVISOR_FS_PROFILE": "0",
            }
            conditions = {
                arm: dict(
                    env={**common_env, "PVISOR_VM_FS_WRITEBACK": str(int(arm == "candidate"))}
                )
                for arm in AUDIT.ARMS
            }
            protocol = dict(
                benchmark_id="B-FS-ENG",
                role="engineering A/B",
                comparison="guest-writeback",
                diagnostic=False,
                samples=3,
                resident_rounds=3,
                conditions=conditions,
                budget=declared,
                quiet_gate=False,
                interference_gate=False,
                inventory_gate=False,
                artifacts={},
                source_hashes={},
                rootfs="/prepared/rootfs",
                rootfs_tools={},
                catalog=[dict(id=case, layer="mechanism") for case in ("M06", "M07")],
                inventory=dict(selected_cell_ids=["M06", "M07"]),
            )
            save(batch / "fixture-manifest.json", {})
            protocol["fixture_manifest_sha256"] = AUDIT.sha(batch / "fixture-manifest.json")
            save(batch / "source-before.json", {})
            save(batch / "budget-before.json", state)
            save(batch / "budget-after.json", state)
            (batch / "bin").mkdir()
            (batch / "bin/libkrunfw.so").write_bytes(b"frozen firmware")
            save(
                batch / "firmware-source/SOURCE.json",
                dict(firmware_sha256=AUDIT.sha(batch / "bin/libkrunfw.so")),
            )
            for path in ("driver/src/main.rs", "harness/workloads_v2_driver.rs"):
                target = batch / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("identical frozen driver")
            records = []
            for case in ("M06", "M07"):
                for arm in AUDIT.ARMS:
                    for pair in range(3):
                        folder = batch / "trials" / f"{case}-{pair:03}-{arm}"
                        folder.mkdir(parents=True)
                        rows = [
                            dict(
                                id=case,
                                round=r,
                                correctness=True,
                                setup_ns=0,
                                work_ns=10,
                                final_sync_ns=5,
                                completed_ns=15,
                                validation_ns=1,
                                foreground_read_ns=0,
                                bytes_written=4 << 20,
                                bytes_read=0,
                                write_ops=4096 if case == "M06" else 4,
                                read_ops=0,
                                metadata_ops=0,
                                periodic_fsyncs=0,
                                final_fsyncs=2,
                                checked_closes=2,
                                files_verified=1,
                                bytes_verified=4 << 20,
                            )
                            for r in range(1, 4)
                        ]
                        ready = dict(actual_guest_memtotal_kib=528412)
                        host = dict(id=case, final_round=3, ok=True, verified=True)
                        record = dict(
                            case_id=case,
                            pair=pair,
                            condition=arm,
                            valid=True,
                            host_verified=True,
                            returncode=0,
                            timed_out=False,
                            ready_ms=0.1,
                            job_ms=1,
                            exit_after_done_ms=0.1,
                            ready_record=ready,
                            rows=rows,
                            host_verification=host,
                            command=[
                                str(batch / "bin/subject"),
                                protocol["rootfs"],
                                str(batch / "fixture-layer"),
                                str(batch / "bin/libkrunfw.so"),
                                str(batch / "bin/payload"),
                                str(folder / "stage"),
                                case,
                                "3",
                                "512",
                            ],
                            environment={
                                **conditions[arm]["env"],
                                "PVISOR_STARTUP_TIMING": "0",
                                "PVISOR_STARTUP_PROFILE": "0",
                            },
                            resource_before=state,
                            resource=dict(
                                before=state,
                                after=state,
                                cpu_usage_usec_delta=10,
                                sampled_memory_current_peak=1024,
                            ),
                        )
                        if case == "M06" and arm == "baseline" and pair == 0:
                            record.update(
                                valid=False, host_verified=False, error="retained I/O failure"
                            )
                        save(folder / "result.json", record)
                        lines = ["WORKLOADS_V2_READY " + json.dumps(ready)]
                        lines += ["WORKLOADS_V2_RESULT " + json.dumps(row) for row in rows]
                        lines += [
                            "WORKLOADS_V2_DONE "
                            + json.dumps(dict(id=case, rounds=3, correctness=True))
                        ]
                        (folder / "stdout.log").write_text("\n".join(lines) + "\n")
                        (folder / "stderr.log").write_text("")
                        (folder / "host-verification.log").write_text(
                            "WORKLOADS_V2_VERIFIED " + json.dumps(host)
                        )
                        records.append(record)
            save(batch / "protocol.json", protocol)
            save(batch / "records.json", records)
            export = self.semspec_fixture(batch)
            raw = dict(trials=12, failed=1)
            export["commands"][0].update(raw=raw, value=raw)
            save(batch / "in.bench.json", export)
            save(batch / "in.bench.logs/0001.log", raw)
            with patch.object(AUDIT, "load_validator", return_value=lambda row, case: None):
                receipt = AUDIT.audit(batch, source_root=batch)
            self.assertFalse(receipt["valid"])
            self.assertEqual(receipt["global_errors"], [])
            self.assertEqual(receipt["counts"]["observed_round_rows"], 36)
            self.assertEqual(receipt["counts"]["verified_round_rows"], 33)
            self.assertEqual(receipt["counts"]["failed_reported"], 1)
            self.assertEqual(receipt["counts"]["usable_cases"], 1)
            self.assertEqual(
                [(case["case_id"], case["usable"]) for case in receipt["cases"]],
                [("M06", False), ("M07", True)],
            )


if __name__ == "__main__":
    unittest.main()
