"""Real Linux whole-machine save/exit/restore contracts on Apple Silicon HVF."""

import hashlib
import json
import os
import shutil
import subprocess
import time
import unittest

from tests.fixtures import workspace
from tests.hardware import support


class LinuxColdRestoreTests(unittest.TestCase):
    def setUp(self):
        support.require_hvf()
        self.driver = support.binary()
        self.guest, self.firmware = support.guest_and_firmware()

    def test_restore_preserves_linux_and_fences_the_execution_lease(self):
        with workspace() as (base, _):
            root = base / "rootfs"
            root.mkdir()
            shutil.copyfile(self.guest, root / "init.krun")
            (root / "init.krun").chmod(0o700)
            env = dict(os.environ, PVISOR_CASE_VM_LIBRARY_DIR=str(self.firmware))
            started = time.monotonic()
            source = subprocess.run(
                [str(self.driver), "save", str(base)],
                env=env,
                text=True,
                capture_output=True,
                timeout=45,
            )
            support.assert_response(self, source)
            original = (root / "ready").read_text()
            saved = json.loads((base / "state.json").read_text())
            self.assertIn("linux-save-runner-exiting", source.stdout)
            manifest_path = base / "state.json"
            pristine = manifest_path.read_bytes()
            rejected = []
            for field, message in [
                ("boot", "snapshot host/build mismatch"),
                ("state", "machine state digest mismatch"),
            ]:
                with self.subTest(corruption=field):
                    invalid = json.loads(pristine)
                    if field == "boot":
                        invalid["boot"] = "different-boot"
                    else:
                        invalid["state"]["version"] = 99
                    manifest_path.write_text(json.dumps(invalid))
                    bad = subprocess.run(
                        [str(self.driver), "restore", str(base)],
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    support.assert_response(self, bad, message)
                    rejected.append(message)
                    manifest_path.write_bytes(pristine)
            with (base / "ram.bin").open("r+b") as ram:
                byte = ram.read(1)
                ram.seek(0)
                ram.write(bytes([byte[0] ^ 1]))
            bad = subprocess.run(
                [str(self.driver), "restore", str(base)],
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            support.assert_response(self, bad, "RAM snapshot digest mismatch")
            rejected.append("RAM snapshot digest mismatch")
            with (base / "ram.bin").open("r+b") as ram:
                ram.write(byte)
            out, err = base / "restore.stdout", base / "restore.stderr"
            with out.open("w") as stdout, err.open("w") as stderr:
                target = subprocess.Popen(
                    [str(self.driver), "restore", str(base)],
                    env=env,
                    stdout=stdout,
                    stderr=stderr,
                    text=True,
                )
                try:
                    deadline = time.monotonic() + 30
                    while not (root / "ready").exists() or (root / "ready").read_text() == original:
                        if target.poll() is not None:
                            self.fail("restore exited: " + out.read_text() + err.read_text())
                        if time.monotonic() >= deadline:
                            self.fail(
                                "restored Linux heartbeat: " + out.read_text() + err.read_text()
                            )
                        time.sleep(0.05)
                    resumed = (root / "ready").read_text()
                    self.assertEqual(resumed.split()[:2], original.split()[:2])
                    self.assertGreater(int(resumed.split()[2]), int(original.split()[2]))
                    contender = subprocess.run(
                        [str(self.driver), "restore", str(base)],
                        env=env,
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    support.assert_response(self, contender, "another runner owns")
                    (root / "release").write_text("go")
                    while not (root / "result").exists():
                        if target.poll() is not None or time.monotonic() >= deadline:
                            self.fail("guest result missing: " + out.read_text() + err.read_text())
                        time.sleep(0.05)
                    result = (root / "result").read_text()
                    self.assertTrue(result.startswith("linux-cold-restore-ok "))
                    self.assertEqual(len((root / "starts").read_text().splitlines()), 1)
                    support.write_evidence(
                        {
                            "scope": "real Linux full-machine same-host cross-runner experiment",
                            "elapsed_seconds": time.monotonic() - started,
                            "firmware_sha256": hashlib.sha256(
                                (self.firmware / "libkrunfw.5.dylib").read_bytes()
                            ).hexdigest(),
                            "guest_sha256": hashlib.sha256(
                                (root / "init.krun").read_bytes()
                            ).hexdigest(),
                            "cpu_count": len(saved["state"]["cpus"]),
                            "device_inventory": [
                                {
                                    "base": item["base"],
                                    "len": item["len"],
                                    "kind": item["device"]["kind"],
                                    "virtio_type": item["device"]["state"].get("device_type")
                                    if item["device"]["kind"] == "Virtio"
                                    else None,
                                }
                                for item in saved["state"]["devices"]
                            ],
                            "state_hash": saved["state_hash"],
                            "ram_hash": saved["ram_hash"],
                            "negative_checks": rejected,
                            "source_pid": saved["source_pid"],
                            "restore_pid": target.pid,
                            "source_reaped_before_restore": True,
                            "guest_before": original,
                            "guest_after": resumed,
                            "result": result,
                            "starts": (root / "starts").read_text(),
                            "source_stdout": source.stdout,
                            "source_stderr": source.stderr,
                            "restore_stdout": out.read_text(),
                            "restore_stderr": err.read_text(),
                            "ram_bytes": (base / "ram.bin").stat().st_size,
                            "driver_sha256": hashlib.sha256(self.driver.read_bytes()).hexdigest(),
                            "same_execution_lease_rejected_second_runner": True,
                        }
                    )
                finally:
                    if target.poll() is None:
                        target.terminate()
                        try:
                            target.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            target.kill()
                            target.wait()
