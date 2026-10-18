"""Offline contracts for the ordinary-CLI guest writeback sampler; no VM."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "guest_writeback_cli", Path(__file__).with_name("guest_writeback_cli.py")
)
SUBJECT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUBJECT)


class GuestWritebackCliContracts(unittest.TestCase):
    def test_only_guest_writeback_varies(self):
        for diagnostic in (False, True):
            before = SUBJECT.environment("off", diagnostic)
            after = SUBJECT.environment("on", diagnostic)
            self.assertEqual(
                {key for key in before if before[key] != after[key]}, {SUBJECT.WRITEBACK_ENV}
            )
            self.assertEqual(
                (before[SUBJECT.WRITEBACK_ENV], after[SUBJECT.WRITEBACK_ENV]), ("0", "1")
            )
            for env in (before, after):
                self.assertEqual(env[SUBJECT.HOST_ENV], "0")
                self.assertEqual(env[SUBJECT.FLUSH_ENV], "0")
                self.assertEqual(env["PVISOR_VM_FS_LOWER_DATA_CACHE"], "0")
        with self.assertRaises(ValueError):
            SUBJECT.environment("auto")

    def test_actual_negotiation_requires_both_explicit_flags(self):
        for arm, value in (("off", 0), ("on", 1)):
            measurements = {
                key: {"units": value}
                for key in ("guest_writeback_enabled", "guest_batched_writeback_enabled")
            }
            self.assertEqual(
                SUBJECT.validate_guest_writeback_state(measurements, arm)["requested"], value
            )
            for key in measurements:
                missing = {k: v for k, v in measurements.items() if k != key}
                with self.assertRaises(ValueError):
                    SUBJECT.validate_guest_writeback_state(missing, arm)
                for wrong in (bool(value), 1 - value):
                    altered = {**measurements, key: {"units": wrong}}
                    with self.assertRaises(ValueError):
                        SUBJECT.validate_guest_writeback_state(altered, arm)

    def test_host_buffer_cannot_be_enabled_by_negotiation(self):
        values = {
            "guest_writeback_enabled": {"units": 1},
            "guest_batched_writeback_enabled": {"units": 1},
        }
        for wrong in (1, False):
            with self.assertRaises(ValueError):
                SUBJECT.validate_guest_writeback_state(
                    {**values, "host_writeback_enabled": {"units": wrong}}, "on"
                )

    def test_firmware_mapping_is_bound_to_actual_device_inode(self):
        with tempfile.TemporaryDirectory() as root:
            firmware = Path(root) / "libkrunfw.so.5.6.2"
            firmware.write_bytes(b"fixture")
            identity = firmware.stat()
            prefix = f"1000-2000 r--p 00000000 {os.major(identity.st_dev):02x}:{os.minor(identity.st_dev):02x}"
            line = f"{prefix} {identity.st_ino} {firmware}"
            self.assertEqual(
                SUBJECT.firmware_mapping_entry(line, firmware, identity)["inode"], identity.st_ino
            )
            self.assertIsNone(SUBJECT.firmware_mapping_entry(line, Path(root) / "other", identity))
            with self.assertRaises(ValueError):
                SUBJECT.firmware_mapping_entry(
                    f"{prefix} {identity.st_ino + 1} {firmware}", firmware, identity
                )

    def test_primary_sums_three_resident_rounds_without_exit(self):
        row = {"ready_ms": 4, "job_ms": 99999, "result": {"filesystem": {}}}
        for operation in SUBJECT.OPERATIONS:
            row["result"]["filesystem"][operation] = {
                "rounds": [{"worker_ms": value} for value in (1, 2, 3)]
            }
        metrics = SUBJECT.metrics(row)
        self.assertEqual(len(metrics), 32)
        self.assertEqual(metrics["ready_ms"], 4)
        self.assertEqual(metrics["three_rounds.seven_tools_ms"], 7 * (1 + 2 + 3))
        self.assertNotIn("job_ms", metrics)

    def test_btrfs_mapping_requires_process_mount_and_byte_proof(self):
        from types import SimpleNamespace

        firmware = Path("/tmp/private-out/firmware.so")
        identity = SimpleNamespace(st_dev=os.makedev(0, 54), st_ino=12)
        line = f"1000-2000 r--p 00000000 00:24 12 {firmware}"
        mount = "123 1 0:36 / /tmp/private-out rw - btrfs /dev/example rw"
        proof = SUBJECT.firmware_mapping_entry(
            line, firmware, identity, mountinfo=mount, mapped_sha256="a" * 64
        )
        self.assertEqual(proof["mapping_device"], os.makedev(0, 36))
        self.assertNotEqual(proof["mapping_device"], proof["device"])
        for kwargs in (
            {},
            {"mountinfo": mount},
            {"mountinfo": mount.replace("btrfs", "ext4"), "mapped_sha256": "a" * 64},
        ):
            with self.assertRaises(ValueError):
                SUBJECT.firmware_mapping_entry(line, firmware, identity, **kwargs)


if __name__ == "__main__":
    unittest.main()
