"""Independent full-copy Linux environment save/exit/restore on macOS HVF."""

import hashlib
import json
import os
import shutil
import subprocess
import time
import unittest

from tests.fixtures import workspace
from tests.hardware import support


class EnvironmentSnapshotTests(unittest.TestCase):
    def setUp(self):
        support.require_hvf()
        self.driver = support.binary()
        self.guest, self.firmware = support.guest_and_firmware()

    def test_closed_standard_descriptors_are_repaired_before_rust_startup(self):
        with workspace() as (base, _):
            root = base / "rootfs"
            root.mkdir()
            # The no-std trampoline makes all three descriptors absent on every
            # exec. An empty root also guarantees /dev/null is initially absent.
            shutil.copyfile(
                support.target_dir() / "debug/probes/guest_closed_stdio", root / "init.krun"
            )
            shutil.copyfile(self.guest, root / "init-real")
            for name in ["init.krun", "init-real"]:
                (root / name).chmod(0o700)
            self.assertFalse((root / "dev").exists())
            source = subprocess.run(
                [str(self.driver), "boot", str(base)],
                env=dict(os.environ, PVISOR_CASE_VM_LIBRARY_DIR=str(self.firmware)),
                text=True,
                capture_output=True,
                timeout=60,
            )
            support.assert_response(self, source)
            self.assertEqual((root / "bootstrap-stdio").read_text(), "7")
            self.assertEqual(len((root / "starts").read_text().splitlines()), 1)
            self.assertTrue((root / "ready").is_file())
            self.assertIn("environment-boot-ready", source.stdout)

    def test_restore_is_independent_of_original_and_published_backing(self):
        with workspace() as (base, _):
            runner = base / "vm-runner"
            shutil.copyfile(self.driver, runner)
            runner.chmod(0o700)
            root = base / "rootfs"
            root.mkdir()
            shutil.copyfile(self.guest, root / "init.krun")
            (root / "init.krun").chmod(0o700)
            (root / "unvisited").write_bytes(b"independent filesystem contents")
            env = dict(os.environ, PVISOR_CASE_VM_LIBRARY_DIR=str(self.firmware))
            source = subprocess.run(
                [str(runner), "save", str(base)],
                env=env,
                text=True,
                capture_output=True,
                timeout=60,
            )
            support.assert_response(self, source)
            original = (root / "ready").read_text()
            identity = (base / "snapshot.id").read_text().strip()
            object_path = base / "store/objects" / identity
            manifest = json.loads((object_path / "manifest.json").read_bytes())
            machine = json.loads((object_path / "machine.json").read_bytes())
            shutil.rmtree(root)
            self.assertFalse(root.exists())
            rejected = []
            for name in ["ram.bin", "machine.json", "manifest.json"]:
                with self.subTest(corruption=name):
                    path = object_path / name
                    pristine = path.read_bytes()
                    damaged = bytearray(pristine)
                    damaged[0] ^= 1
                    path.write_bytes(damaged)
                    bad = subprocess.run(
                        [str(runner), "restore", str(base)],
                        env=env,
                        text=True,
                        capture_output=True,
                        timeout=15,
                    )
                    support.assert_response(self, bad, "digest mismatch")
                    self.assertFalse((base / "restored-rootfs").exists())
                    rejected.append(name)
                    path.write_bytes(pristine)
            out, err = base / "restore.stdout", base / "restore.stderr"
            work = base / "restored-rootfs"
            with out.open("w") as stdout, err.open("w") as stderr:
                target = subprocess.Popen(
                    [str(runner), "restore", str(base)], env=env, stdout=stdout, stderr=stderr
                )
                try:
                    deadline = time.monotonic() + 40
                    while not (work / "ready").exists() or (work / "ready").read_text() == original:
                        if target.poll() is not None or time.monotonic() >= deadline:
                            self.fail(
                                "independent restore failed: " + out.read_text() + err.read_text()
                            )
                        time.sleep(0.05)
                    resumed = (work / "ready").read_text()
                    self.assertEqual(resumed.split()[:2], original.split()[:2])
                    self.assertGreater(int(resumed.split()[2]), int(original.split()[2]))
                    self.assertEqual(
                        (work / "unvisited").read_bytes(), b"independent filesystem contents"
                    )
                    self.assertFalse(root.exists())
                    contender = subprocess.run(
                        [str(runner), "restore", str(base)],
                        env=env,
                        text=True,
                        capture_output=True,
                        timeout=15,
                    )
                    support.assert_response(self, contender, "another runner owns")
                    deleted = subprocess.run(
                        [str(runner), "delete", str(base)],
                        env=env,
                        text=True,
                        capture_output=True,
                        timeout=15,
                    )
                    support.assert_response(self, deleted)
                    self.assertFalse(object_path.exists())
                    (work / "release").write_text("continue after backing deletion")
                    while not (work / "result").exists():
                        if target.poll() is not None or time.monotonic() >= deadline:
                            self.fail("guest result missing: " + out.read_text() + err.read_text())
                        time.sleep(0.05)
                    result = (work / "result").read_text()
                    self.assertTrue(result.startswith("linux-cold-restore-ok "))
                    self.assertEqual(len((work / "starts").read_text().splitlines()), 1)
                    support.write_evidence(
                        {
                            "scope": "same-host/boot/build real Linux independent full-copy environment snapshot",
                            "snapshot_id": identity,
                            "cpu_count": len(machine["state"]["cpus"]),
                            "ram_mapping_bytes": sum(
                                item["len"] for item in machine["state"]["ram"]
                            ),
                            "device_inventory": [
                                {
                                    "base": item["base"],
                                    "len": item["len"],
                                    "kind": item["device"]["kind"],
                                    "virtio_type": item["device"]["state"].get("device_type")
                                    if item["device"]["kind"] == "Virtio"
                                    else None,
                                }
                                for item in machine["state"]["devices"]
                            ],
                            "excluded_resources": machine["excluded_resources"],
                            "source_pid": machine["source_pid"],
                            "restore_pid": target.pid,
                            "source_reaped_before_restore": True,
                            "original_tree_deleted_before_restore": True,
                            "published_object_deleted_before_guest_final_check": True,
                            "guest_before": original,
                            "guest_after": resumed,
                            "result": result,
                            "manifest": manifest,
                            "negative_payloads": rejected,
                            "same_execution_lease_rejected_second_runner": True,
                            "source_stdout": source.stdout,
                            "source_stderr": source.stderr,
                            "restore_stdout": out.read_text(),
                            "restore_stderr": err.read_text(),
                            "driver_sha256": hashlib.sha256(runner.read_bytes()).hexdigest(),
                            "firmware_sha256": hashlib.sha256(
                                (self.firmware / "libkrunfw.5.dylib").read_bytes()
                            ).hexdigest(),
                            "guest_sha256": hashlib.sha256(
                                (work / "init.krun").read_bytes()
                            ).hexdigest(),
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
