"""Offline guest-writeback followup contracts; no build, VM or systemd launch."""

import copy
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import guest_writeback_curves as WB  # noqa: E402 - import after module-loading setup


class GuestWritebackContracts(unittest.TestCase):
    def test_new_firmware_receipt_must_bind_bytes_and_guest_writeback_patch(self):
        record = dict(
            firmware_sha256="abc",
            inputs={"patches/0048-fuse-pipeline-native-writeback-pressure.patch": "def"},
        )
        WB.validate_firmware_record(record, "abc")
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(record, "different binary")
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(dict(firmware_sha256="abc", inputs={}), "abc")
        old = dict(
            firmware_sha256="old",
            inputs={"patches/0047-fuse-isolate-immutable-lower-data.patch": "ghi"},
        )
        WB.validate_firmware_record(old, "old", old=True)
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(record, "abc", old=True)

    def test_tuning_receipt_preserves_0048_and_requires_only_0049_as_candidate(self):
        baseline = dict(firmware_sha256="baseline", inputs={WB.PATCHES[48]: "old"})
        candidate = dict(
            firmware_sha256="candidate", inputs={WB.PATCHES[48]: "old", WB.PATCHES[49]: "new"}
        )
        WB.validate_firmware_record(baseline, "baseline", old=True, baseline_patch=48)
        WB.validate_firmware_record(candidate, "candidate", baseline_patch=48)
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(baseline, "baseline", baseline_patch=48)
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(candidate, "candidate", old=True, baseline_patch=48)
        with self.assertRaises(ValueError):
            WB.validate_firmware_record(candidate, "candidate", baseline_patch=49)

    def test_registered_axes_inventory_and_single_variable(self):
        cells = WB.cells()
        self.assertEqual(len(cells), 15)
        self.assertEqual(len({row["id"] for row in cells}), 15)
        self.assertEqual(
            [r["block_bytes"] for r in cells if r["family"] == "block"],
            [1024, 4096, 65536, 1048576],
        )
        self.assertEqual([r["workers"] for r in cells if r["family"] == "workers"], [1, 2, 4])
        self.assertEqual(
            {r["workers"] * r["bytes_per_worker"] for r in cells if r["family"] == "workers"},
            {4 * WB.MIB},
        )
        self.assertEqual(
            [r["sync_every_ops"] for r in cells if r["family"] == "barrier"], [1, 16, 256, 0]
        )
        self.assertEqual(
            [r["bytes_per_worker"] for r in cells if r["family"] == "pressure"],
            [8 * WB.MIB, 64 * WB.MIB, 128 * WB.MIB],
        )
        arms = WB.conditions()
        self.assertEqual(tuple(arms), WB.ARMS)
        differences = {
            key
            for key in arms["off"]["env"]
            if arms["off"]["env"][key] != arms["guest-wb"]["env"][key]
        }
        self.assertEqual(differences, {"PVISOR_VM_FS_WRITEBACK"})
        self.assertEqual(
            {
                key
                for key in arms["old-wb"]["env"]
                if arms["old-wb"]["env"][key] != arms["guest-wb"]["env"][key]
            },
            {"CURVES_FIRMWARE"},
        )
        self.assertEqual(arms["native"]["env"], arms["off"]["env"])
        self.assertTrue(all(a["env"]["PVISOR_VM_FS_HOST_WRITEBACK"] == "0" for a in arms.values()))
        self.assertEqual(15 * 4 * 5 * 3, 900)  # cells x arms x independent pairs x rounds

    def test_payload_has_no_auxiliary_close_and_parent_source_remains_separate(self):
        payload = (HERE / "guest_writeback_guest.rs").read_text()
        parent = (HERE / "curves_guest.rs").read_text()
        self.assertNotEqual(payload, parent)
        self.assertIn("fn applied_close(", parent)
        self.assertNotIn("fn applied_close(", payload)
        self.assertNotIn("dup(", payload)
        self.assertNotIn("Phase::Applied", payload)
        self.assertIn("let applied = accepted;", payload)
        self.assertIn("self.file.as_ref().unwrap().sync_all()?;", payload)
        self.assertIn("file.sync_all()?;", payload)

    def test_completed_clock_requires_no_auxiliary_phase_and_all_syncs(self):
        row = dict(
            applied_phase_measured=False,
            applied_ns=0,
            applied_barriers=0,
            applied_boundary="unused_no_auxiliary_flush",
            workload="stream",
            workers=2,
            bytes_per_worker=8192,
            block_bytes=1024,
            final_fsyncs=2,
            final_closes=2,
            directory_fsyncs=6,
            accepted_ns=7,
            fsync_ns=3,
            close_ns=2,
            dirsync_ns=1,
            total_ns=13,
            fsync_boundary=WB.PHASE_CONTRACT["stream_fsync_boundary"],
        )
        WB.validate_row(row)
        for key, value in [
            ("applied_phase_measured", True),
            ("applied_ns", 1),
            ("applied_barriers", 1),
            ("final_fsyncs", 0),
            ("final_closes", 1),
            ("directory_fsyncs", 0),
            ("total_ns", 12),
        ]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                WB.validate_row({**row, key: value})

    def test_actual_negotiation_requires_both_explicit_flags_and_rejects_bool(self):
        for condition, value in [("off", 0), ("guest-wb", 1), ("old-wb", 1)]:
            values = {
                name: dict(units=value)
                for name in ["guest_writeback_enabled", "guest_batched_writeback_enabled"]
            }
            self.assertEqual(
                WB.actual_writeback({"virtio-fs-overlay": values}, condition), (value, value)
            )
            for malformed in [
                {},
                {**values, "guest_writeback_enabled": {"units": bool(value)}},
                {**values, "guest_batched_writeback_enabled": {"units": 1 - value}},
            ]:
                with self.subTest(condition=condition), self.assertRaises(ValueError):
                    WB.actual_writeback({"virtio-fs-overlay": malformed}, condition)

    def test_write_sizes_use_disjoint_counts_and_do_not_invent_phase_maximum(self):
        WB.validate_counter_contract(WB.COUNTERS)
        values = {b["label"]: dict(units=0) for b in WB.COUNTERS["buckets"]}
        values.update(
            WRITE=dict(calls=2),
            write_payload_bytes=dict(units=66048),
            write_background_requests=dict(units=1),
        )
        values["write_size_le_1024"]["units"] = 1
        values["write_size_le_65536"]["units"] = 1
        result = WB.write_sizes(values, WB.COUNTERS)
        self.assertEqual(result["mean_payload_bytes"], 33024)
        self.assertIsNone(result["interval_max_bytes"])
        self.assertEqual(
            result["largest_occupied_bucket_bounds"], dict(lower_bytes=16385, upper_bytes=65536)
        )
        for key, bad in [
            ("write_payload_bytes", dict(units=True)),
            ("write_background_requests", dict(units=3)),
            ("write_size_le_1024", dict(units=0)),
        ]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                WB.write_sizes({**values, key: bad}, WB.COUNTERS)
        del values["write_size_gt_1048576"]
        with self.assertRaises(ValueError):
            WB.write_sizes(values, WB.COUNTERS)

    def test_size_contract_requires_ordered_disjoint_buckets_and_explicit_overflow(self):
        bad = copy.deepcopy(WB.COUNTERS)
        bad["buckets"][1]["upper_bytes"] = 1024
        with self.assertRaises(ValueError):
            WB.validate_counter_contract(bad)
        bad = copy.deepcopy(WB.COUNTERS)
        bad["buckets"][-1]["upper_bytes"] = 2097152
        with self.assertRaises(ValueError):
            WB.validate_counter_contract(bad)

    def test_new_contract_failure_retains_output_until_all_observer_checks_pass(self):
        for broken in (False, True):
            with self.subTest(broken=broken), tempfile.TemporaryDirectory() as root:
                batch = Path(root)
                (batch / "bin").mkdir()
                (batch / "bin/libkrunfw.so").write_bytes(b"firmware fixture")
                folder = batch / "trials/000-off"
                stage = folder / "stage"

                def observer(batch, protocol, pair, condition):
                    stage.mkdir(parents=True)
                    (folder / "stderr.log").write_text(
                        "CURVES_FIRMWARE_SELECTED "
                        + __import__("json").dumps(
                            dict(path=str((batch / "bin/libkrunfw.so").resolve()), selection="argv")
                        )
                        + "\n"
                    )
                    WB.BASE.shutil.rmtree(stage)
                    row = dict(
                        applied_phase_measured=broken,
                        applied_ns=0,
                        applied_barriers=0,
                        applied_boundary="unused_no_auxiliary_flush",
                        workload="stream",
                        workers=1,
                        bytes_per_worker=1024,
                        block_bytes=1024,
                        final_fsyncs=1,
                        final_closes=1,
                        directory_fsyncs=5,
                        accepted_ns=7,
                        fsync_ns=3,
                        close_ns=2,
                        dirsync_ns=1,
                        total_ns=13,
                        fsync_boundary=WB.PHASE_CONTRACT["stream_fsync_boundary"],
                    )
                    return dict(valid=True, environment=WB.conditions()["off"]["env"], rows=[row])

                with patch.object(WB.BASE, "one_trial", observer):
                    WB.install_trial_validation()
                    result = WB.BASE.one_trial(batch, dict(conditions=WB.conditions()), 0, "off")
                self.assertEqual(result["valid"], not broken)
                self.assertEqual(stage.is_dir(), broken)
                if broken:
                    self.assertTrue((folder / "result.json").is_file())


if __name__ == "__main__":
    unittest.main()
