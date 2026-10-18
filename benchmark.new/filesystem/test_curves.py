"""Offline protocol tests for B-FS-ENG plans and B-FS-DIAG evidence.

No compiler, VM, systemd service or benchmark workload is started by these tests.
The observer is exercised with synthetic process output and real temporary logs.
"""

import copy
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CURVES = load("filesystem_curves_contract", "curves.py")
DIAG = load("filesystem_curves_diagnostics_contract", "curves_diagnostics.py")


def encoded(prefix, record):
    return prefix + json.dumps(record) + "\n"


def cell():
    return dict(
        id="stream",
        family="block",
        x=4,
        workload="stream",
        block_bytes=4096,
        workers=1,
        bytes_per_worker=8192,
        sync_every_ops=0,
    )


def row(round_):
    plan = cell()
    hash_ = 0xCBF29CE484222325
    for byte in plan["id"].encode():
        hash_ = ((hash_ ^ byte) * 0x100000001B3) % 2**64
    seed = hash_ ^ 0x9E3779B97F4A7C15 ^ ((round_ * 0xD1B54A32D192ED03) % 2**64)
    return dict(
        schema=1,
        id=plan["id"],
        workload="stream",
        round=round_,
        block_bytes=4096,
        workers=1,
        bytes_per_worker=8192,
        sync_every_ops=0,
        bytes=8192,
        write_ops=2,
        read_ops=0,
        bytes_written=8192,
        bytes_read=0,
        files_created_in_clock=0,
        accepted_closes=0,
        periodic_fsyncs=0,
        periodic_fsync_worker_ns_sum=0,
        applied_barriers=1,
        final_fsyncs=1,
        final_closes=1,
        directory_fsyncs=5,
        accepted_ns=1,
        applied_ns=2,
        fsync_ns=3,
        close_ns=4,
        dirsync_ns=5,
        total_ns=15,
        preparation_ns=1,
        verification_ns=1,
        output_dir=f"curves-output/stream/r{round_:02}",
        files_per_worker=1,
        pattern="fnv1a64-id-splitmix64-le-v1",
        pattern_round=round_,
        pattern_seeds=[seed],
        applied_boundary="dup_checked_close_flush_prefix",
        fsync_boundary="fsync_original_open_file",
        profile_interval="work_through_directory_fsync_excludes_fixture_and_verification",
        correctness=True,
    )


class FakeProcess:
    def __init__(self, stdout):
        self.stdout = io.StringIO(stdout)
        self.returncode = 0

    def wait(self):
        return self.returncode

    def poll(self):
        return self.returncode

    def kill(self):
        self.returncode = -9


class CurvesPlanContracts(unittest.TestCase):
    def test_all_plan_cells_are_unique_bounded_and_serialize_without_lost_bytes(self):
        for suite, count in (("shape", 18), ("confirm", 4), ("parameters", 4)):
            with self.subTest(suite=suite):
                cells = CURVES.cells(suite)
                self.assertEqual(len(cells), count)
                self.assertEqual(len({c["id"] for c in cells}), count)
                tsv = CURVES.plan_text(cells).splitlines()
                self.assertEqual(len(tsv), count)
                for planned, line in zip(cells, tsv):
                    self.assertEqual(
                        line.split("\t"),
                        [
                            str(planned[key])
                            for key in (
                                "id",
                                "workload",
                                "block_bytes",
                                "workers",
                                "bytes_per_worker",
                                "sync_every_ops",
                            )
                        ],
                    )
                    self.assertRegex(planned["id"], r"^[A-Za-z0-9_-]{1,64}$")
                    self.assertIn(planned["workload"], ("stream", "smallfiles", "read"))
                    self.assertGreater(planned["block_bytes"], 0)
                    self.assertGreater(planned["workers"], 0)
                    self.assertLessEqual(planned["workers"], 64)
                    self.assertGreater(planned["bytes_per_worker"], 0)
                    self.assertLessEqual(
                        planned["bytes_per_worker"] * planned["workers"], 512 * CURVES.MIB
                    )
                    self.assertEqual(planned["bytes_per_worker"] % planned["block_bytes"], 0)
                    if planned["workload"] == "read":
                        self.assertEqual(planned["sync_every_ops"], 0)
        with self.assertRaises(ValueError):
            CURVES.cells("unregistered-suite")

    def test_shape_axes_change_one_declared_quantity_and_confirm_uses_same_cells(self):
        shape = CURVES.cells("shape")
        self.assertEqual(
            [c["x"] for c in shape if c["family"] == "block"], [1, 4, 16, 64, 256, 1024]
        )
        workers = [c for c in shape if c["family"] == "workers"]
        self.assertEqual([c["workers"] for c in workers], [1, 2, 4])
        self.assertEqual({c["workers"] * c["bytes_per_worker"] for c in workers}, {4 * CURVES.MIB})
        barriers = [c for c in shape if c["family"] == "barrier"]
        self.assertEqual([c["sync_every_ops"] for c in barriers], [1, 16, 256, 0])
        self.assertEqual({c["bytes_per_worker"] for c in barriers}, {CURVES.MIB})
        shape_by_id = {c["id"]: c for c in shape}
        for confirm in CURVES.cells("confirm"):
            self.assertEqual(confirm, shape_by_id[confirm["id"]])
        pressure = [
            c["bytes_per_worker"] for c in CURVES.cells("parameters") if c["family"] == "pressure"
        ]
        self.assertEqual(pressure, [mib * CURVES.MIB for mib in (8, 64, 128)])

    def test_ablations_have_only_the_intended_private_configuration_changes(self):
        arms = CURVES.conditions("shape")
        self.assertEqual(
            set(arms), {"sync", "async", "native", "async-old", "unmerged", "owned-sync"}
        )
        self.assertEqual(arms["native"]["env"], {})
        self.assertEqual(arms["sync"]["env"], {"PVISOR_VM_FS_HOST_WRITEBACK": "0"})
        self.assertEqual(arms["async"]["env"], {"PVISOR_VM_FS_HOST_WRITEBACK": "1"})
        for name, configuration in (
            ("async-old", {"qualification_lock": True}),
            ("unmerged", {"chunks": 1, "delay_us": 0}),
            ("owned-sync", {"synchronous": True}),
        ):
            environment = arms[name]["env"]
            self.assertEqual(environment["PVISOR_VM_FS_HOST_WRITEBACK"], "1")
            self.assertEqual(
                json.loads(environment["PVISOR_VM_FS_HOST_WRITEBACK_CONFIG"]), configuration
            )
        self.assertEqual(set(CURVES.conditions("confirm")), {"sync", "async"})
        parameters = CURVES.conditions("parameters")
        self.assertEqual(len(parameters), 10)
        for name, condition in parameters.items():
            if name == "async":
                continue
            configuration = json.loads(condition["env"]["PVISOR_VM_FS_HOST_WRITEBACK_CONFIG"])
            self.assertEqual(len(configuration), 1, "sensitivity arms must change one factor")
            key, value = next(iter(configuration.items()))
            if key == "capacity_mib":
                self.assertIn(value, (4, 16, 256))
            elif key == "batch_kib":
                self.assertIn(value, (64, 256))
            else:
                self.assertEqual(key, "delay_us")
                self.assertIn(value, (0, 100, 1000, 50000))

    def test_frozen_artifact_changes_are_rejected_before_sampling(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            artifact = root / "payload"
            artifact.write_bytes(b"frozen payload")
            protocol = dict(artifacts={"payload": CURVES.sha(artifact)})
            CURVES.verify_artifacts(root, protocol)
            artifact.write_bytes(b"changed payload")
            with self.assertRaisesRegex(ValueError, "frozen artifact changed"):
                CURVES.verify_artifacts(root, protocol)


class CurvesObserverContracts(unittest.TestCase):
    def run_fake_trial(
        self,
        root,
        *,
        rows=None,
        done=None,
        verification=None,
        native=False,
        duplicate_done=False,
        before_spawn=None,
    ):
        root = Path(root)
        condition = "native" if native else "async"
        protocol = dict(
            rootfs=str(root / "prepared-rootfs"),
            profiling=False,
            resident_rounds=3,
            cells=[cell()],
            conditions=CURVES.conditions("shape"),
        )
        (root / "plan-000.tsv").write_text(CURVES.plan_text(protocol["cells"]))
        rows = [row(round_) for round_ in (1, 2, 3)] if rows is None else rows
        done = dict(schema=1, rows=3, verified=True) if done is None else done
        stdout = encoded("CURVES_READY ", dict(schema=1, cells=1, rounds=3))
        stdout += "".join(encoded("CURVES_RESULT ", record) for record in rows)
        stdout += encoded("CURVES_DONE ", done)
        if duplicate_done:
            stdout += encoded("CURVES_DONE ", done)
        if verification is None:
            verification = encoded(
                "CURVES_VERIFIED ",
                dict(
                    schema=1,
                    rows=3,
                    files=3,
                    bytes=3 * 8192,
                    verified=True,
                    count_scope="cell_round_verifications; reused readset counted each round",
                ),
            )
        process = FakeProcess(stdout)

        def spawn(command, **kwargs):
            if before_spawn:
                before_spawn(command, kwargs)
            return process

        group = dict(
            cgroup="/unused-test-cgroup",
            affinity=[0, 1],
            **{
                "memory.current": "128",
                "memory.max": str(2 << 30),
                "memory.swap.max": "0",
                "cpu.max": "200000 100000",
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(CURVES, "cgroup_state", return_value=group))
            stack.enter_context(patch.object(CURVES.threading, "Thread"))
            stack.enter_context(patch.object(CURVES.threading, "Timer"))
            stack.enter_context(patch.object(CURVES.subprocess, "Popen", side_effect=spawn))
            verify = stack.enter_context(
                patch.object(
                    CURVES.subprocess,
                    "run",
                    return_value=SimpleNamespace(returncode=0, stdout=verification, stderr=""),
                )
            )
            result = CURVES.one_trial(root, protocol, 0, condition)
        folder = root / "trials" / f"000-{condition}"
        self.assertEqual(json.loads((folder / "result.json").read_text()), result)
        return result, folder, verify

    def test_complete_three_round_inventory_is_accepted_and_successful_stage_removed(self):
        with tempfile.TemporaryDirectory() as root:
            result, folder, verification = self.run_fake_trial(root)
            self.assertTrue(result["valid"], result.get("error"))
            self.assertTrue(result["host_verified"])
            self.assertEqual(len(result["rows"]), 3)
            self.assertFalse((folder / "stage").exists())
            self.assertTrue((folder / "stdout.log").is_file())
            verification.assert_called_once()

    def test_missing_duplicate_and_foreign_round_rows_preserve_failed_stage(self):
        good = [row(round_) for round_ in (1, 2, 3)]
        for malformed in (good[:-1], good + [good[0]], [row(1), row(2), row(4)]):
            with self.subTest(rows=malformed), tempfile.TemporaryDirectory() as root:
                result, folder, verification = self.run_fake_trial(root, rows=malformed)
                self.assertFalse(result["valid"])
                self.assertTrue((folder / "stage").is_dir())
                verification.assert_not_called()

    def test_metadata_tampering_and_duplicate_done_cannot_pass_inventory(self):
        for field, bad in (
            ("block_bytes", 1),
            ("workers", 2),
            ("bytes_per_worker", 1),
            ("sync_every_ops", 1),
            ("workload", "read"),
        ):
            malformed = [row(round_) for round_ in (1, 2, 3)]
            malformed[0][field] = bad
            with self.subTest(field=field), tempfile.TemporaryDirectory() as root:
                result, _, _ = self.run_fake_trial(root, rows=malformed)
                self.assertFalse(result["valid"], "same id/round must not hide changed workload")
        with tempfile.TemporaryDirectory() as root:
            result, _, _ = self.run_fake_trial(root, duplicate_done=True)
            self.assertFalse(result["valid"], "DONE must be unique")

    def test_host_verification_requires_unique_exact_positive_receipt(self):
        invalid = [
            "prefix CURVES_VERIFIED suffix\n",
            encoded("CURVES_VERIFIED ", dict(schema=1, rows=3, verified=False)),
            encoded("CURVES_VERIFIED ", dict(schema=1, rows=2, verified=True)),
            encoded("CURVES_VERIFIED ", dict(schema=1, rows=3, verified=True)) * 2,
        ]
        for stdout in invalid:
            with self.subTest(stdout=stdout), tempfile.TemporaryDirectory() as root:
                result, folder, _ = self.run_fake_trial(root, verification=stdout)
                self.assertFalse(result["valid"])
                self.assertTrue((folder / "stage").exists())

    def test_native_workspace_exists_before_payload_starts(self):
        def inspect(command, kwargs):
            self.assertTrue(Path(command[2]).is_dir(), "native payload canonicalizes workspace")
            self.assertEqual(kwargs["env"]["CURVES_PROFILE"], "0")

        with tempfile.TemporaryDirectory() as root:
            result, _, _ = self.run_fake_trial(root, native=True, before_spawn=inspect)
            self.assertTrue(result["valid"], result.get("error"))


def measurement(units=0, calls=0, total_ns=0):
    return dict(
        units=units,
        calls=calls,
        total_ns=total_ns,
        max_ns=total_ns,
        latency_buckets=[calls, 0, 0, 0, 0],
    )


def profile(component, instance, *, final=False, values=None):
    return dict(
        schema=2,
        pid=1234,
        component=component,
        instance=instance,
        inclusive_spans=True,
        final_record=final,
        measurements={} if values is None else values,
    )


def registered_profiles():
    profiles = [
        profile(component, index + 1, final=True)
        for index, component in enumerate(sorted(DIAG.COMPONENTS))
    ]
    lines = [
        encoded(
            DIAG.START,
            {key: value for key, value in record.items() if key in ("pid", "component", "instance")}
            | {"schema": 1},
        )
        for record in profiles
    ]
    return profiles, lines


class CurvesDiagnosticContracts(unittest.TestCase):
    def test_startup_inventory_matches_unique_final_records_exactly(self):
        profiles, starts = registered_profiles()
        ends = [encoded(DIAG.REPORT, record) for record in profiles]
        self.assertEqual(DIAG.collect_profiles(starts + ends), profiles)
        for malformed in (
            starts + ends[:-1],
            starts + ends + [ends[0]],
            starts + starts[:1] + ends,
            starts[1:] + ends,
            starts + [encoded(DIAG.REPORT, profiles[0] | {"final_record": False})],
        ):
            with self.subTest(lines=malformed), self.assertRaises(ValueError):
                DIAG.collect_profiles(malformed)
        self.assertEqual(DIAG.collect_profiles([], expected_instances={}), [])
        with self.assertRaises(ValueError):
            DIAG.collect_profiles([])

    def test_cumulative_records_cannot_reset_or_follow_a_final_record(self):
        profiles, starts = registered_profiles()
        current = copy.deepcopy(profiles[0])
        current["final_record"] = False
        current["measurements"] = {"WRITE": measurement(units=9, calls=2, total_ns=100)}
        ends = [encoded(DIAG.REPORT, record) for record in profiles]
        with self.assertRaisesRegex(ValueError, "nonmonotonic"):
            DIAG.collect_profiles(starts + [encoded(DIAG.REPORT, current)] + ends)
        with self.assertRaisesRegex(ValueError, "final record"):
            DIAG.collect_profiles(starts + ends + [encoded(DIAG.REPORT, current)])

    def test_cell_phase_deltas_follow_saved_order_and_remove_only_marker_counts(self):
        final, _ = registered_profiles()
        lines = []
        for token in range(1, 5):
            snapshots = []
            for record in final:
                labels = {"work": measurement(units=token * 10)}
                if record["component"] == "virtio-fs-protocol":
                    labels.update(
                        IOCTL=measurement(calls=token), request_bytes=measurement(units=80 * token)
                    )
                if record["component"] == "virtio-fs-dispatch":
                    labels["direct_requests"] = measurement(units=token)
                snapshots.append(record | {"final_record": False, "measurements": labels})
                record["measurements"] = copy.deepcopy(labels)
            lines.append(
                encoded(DIAG.PHASE, dict(schema=1, scope="rootfs", token=token, profiles=snapshots))
            )
        rows = DIAG.collect_phases(lines, final, ["second", "first"], 1)
        self.assertEqual(len(rows), 8)
        self.assertEqual(
            {(r["id"], r["before_token"]) for r in rows}, {("second", 1), ("first", 3)}
        )
        for result in rows:
            self.assertEqual(result["raw_measurements"]["work"]["units"], 10)
            self.assertNotIn("max_ns", result["raw_measurements"]["work"])
            if result["component"] == "virtio-fs-protocol":
                self.assertEqual(result["raw_measurements"]["IOCTL"]["calls"], 1)
                self.assertEqual(result["measurements"]["IOCTL"]["calls"], 0)
                self.assertEqual(result["measurements"]["request_bytes"]["units"], 0)
        for malformed in (lines[:-1], lines + lines[:1]):
            with self.subTest(lines=malformed), self.assertRaises(ValueError):
                DIAG.collect_phases(malformed, final, ["second", "first"], 1)

    def test_disabled_or_missing_counters_are_distinct_from_explicit_zero(self):
        self.assertEqual(DIAG.host_writeback({}, False), {})
        with self.assertRaisesRegex(ValueError, "missing explicit"):
            DIAG.host_writeback({}, True)
        labels = {label: measurement() for label in DIAG.HOST_COUNTERS}
        labels["host_writeback_enabled"] = measurement(units=1)
        labels["host_writeback_queue_sessions"] = measurement(units=1)
        components = {"virtio-fs-overlay": labels}
        counters = DIAG.host_writeback(components, True)
        self.assertEqual(counters["host_writeback_backing_errors"], 0)
        labels["host_writeback_accepted_bytes"] = measurement(units=1)
        with self.assertRaisesRegex(ValueError, "completion counters"):
            DIAG.host_writeback(components, True)

    def test_batch_summary_preserves_trial_inventory_and_groups_final_counters(self):
        profiles, starts = registered_profiles()
        phase_lines = []
        for token in (1, 2):
            snapshots = []
            for record in profiles:
                labels = {}
                if record["component"] == "virtio-fs-protocol":
                    labels = dict(
                        IOCTL=measurement(calls=token), request_bytes=measurement(units=80 * token)
                    )
                if record["component"] == "virtio-fs-dispatch":
                    labels = dict(direct_requests=measurement(units=token))
                snapshots.append(record | {"final_record": False, "measurements": labels})
                record["measurements"] = copy.deepcopy(labels)
            phase_lines.append(
                encoded(DIAG.PHASE, dict(schema=1, scope="rootfs", token=token, profiles=snapshots))
            )
        overlay = next(record for record in profiles if record["component"] == "virtio-fs-overlay")
        overlay["measurements"].update({label: measurement() for label in DIAG.HOST_COUNTERS})
        for label, units in (
            ("enabled", 1),
            ("queue_sessions", 1),
            ("accepted_requests", 2),
            ("accepted_bytes", 8192),
            ("backing_bytes", 8192),
            ("queue_jobs", 2),
            ("backing_batches", 1),
            ("queue_batches", 1),
        ):
            overlay["measurements"]["host_writeback_" + label] = measurement(units=units)
        ends = [encoded(DIAG.REPORT, record) for record in profiles]
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            protocol = dict(
                benchmark_id="B-FS-DIAG",
                profiling=True,
                resident_rounds=1,
                conditions={"async": CURVES.conditions("shape")["async"]},
                orders=[dict(pair=0, conditions=["async"], cell_order=["stream"])],
            )
            (root / "protocol.json").write_text(json.dumps(protocol))
            (root / "records.json").write_text(
                json.dumps([dict(pair=0, condition="async", valid=True)])
            )
            log = root / "trials/000-async/stderr.log"
            log.parent.mkdir(parents=True)
            log.write_text("".join(starts + phase_lines + ends))
            result = DIAG.summarize_batch(root)
            self.assertFalse(result["performance_samples"])
            self.assertEqual(len(result["trials"]), 1)
            self.assertEqual(len(result["trials"][0]["phase_rows"]), 4)
            self.assertEqual(result["conditions"].keys(), {"async"})
            self.assertEqual(result["total"], result["conditions"]["async"])
            self.assertEqual(
                result["trials"][0]["host_writeback"]["host_writeback_backing_bytes"], 8192
            )
            log.write_text("".join(starts + phase_lines + ends[:-1]))
            with self.assertRaisesRegex(ValueError, "inventory mismatch"):
                DIAG.summarize_batch(root)


if __name__ == "__main__":
    unittest.main()
