"""Offline launch contracts; never mounts, starts product code or measures."""

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
SPEC = importlib.util.spec_from_file_location(
    "user_workloads_backends", HERE / "user_workloads_backends.py"
)
B = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(B)


class LaunchContracts(unittest.TestCase):
    def test_reference_adapters_own_launch_export_and_cleanup(self):
        expected = (["private-reference"], {"LC_ALL": "C"})
        for backend, module in (
            ("docker", "user_workloads_docker"),
            ("fc-system", "user_workloads_reference_vm"),
            ("qemu", "user_workloads_reference_vm"),
            ("qemu-microvm", "user_workloads_reference_vm"),
        ):
            with (
                self.subTest(backend=backend),
                patch(module + ".build_command", return_value=expected) as launch,
                patch(module + ".finalize_trial", return_value=Path("/retained/output")) as export,
                patch(module + ".cleanup_trial") as cleanup,
            ):
                self.assertEqual(B.build_command("/batch", {}, "/trial", "S03", backend), expected)
                self.assertEqual(
                    B.finalize_output("/batch", {}, "/trial", "S03", backend),
                    (Path("/retained/output"), False),
                )
                B.cleanup_trial("/batch", {}, "/trial", "S03", backend)
                launch.assert_called_once()
                export.assert_called_once_with("/batch", {}, "/trial", "S03", backend)
                cleanup.assert_called_once_with("/batch", {}, "/trial", "S03", backend)

    def test_all_conditions_use_fresh_namespaces_and_one_output_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            (batch / "toolroot").mkdir()
            for backend in B.NAMESPACE_BACKENDS:
                folder = batch / backend
                folder.mkdir()
                command, environment = B.build_command(batch, {}, folder, "S02", backend)
                config = json.loads((folder / "launch.json").read_text())
                self.assertEqual(
                    command[:7],
                    [
                        "unshare",
                        "--user",
                        "--map-root-user",
                        "--mount",
                        "--pid",
                        "--fork",
                        "--kill-child=KILL",
                    ],
                )
                self.assertEqual(set(config["parent_namespaces"]), {"user", "mnt", "pid"})
                self.assertEqual(
                    config["argv"][0],
                    "/bench-artifacts/payload"
                    if backend == "native"
                    else "/bench-artifacts/pvisor",
                )
                self.assertEqual(environment["PVISOR_RUN_HOME"], "/bench-trial/runs")
                self.assertEqual(environment["PVISOR_FS_PROFILE"], "0")
                payload_index = config["argv"].index("/bench-artifacts/payload")
                self.assertEqual(
                    config["argv"][payload_index + 2 : payload_index + 4], ["output", "input"]
                )
                self.assertEqual({p.name for p in (folder / "work").iterdir()}, {"input", "output"})
                output, overlay = B.output_root(folder, backend)
                self.assertTrue(output.is_relative_to(folder))
                self.assertEqual(overlay, backend in ("staged", "vm", "vm-writeback"))
                if backend.startswith("vm"):
                    self.assertIn("--vm", config["argv"])
                    self.assertEqual(
                        environment["PVISOR_VM_FS_WRITEBACK"],
                        "1" if backend == "vm-writeback" else "0",
                    )
                    self.assertEqual(environment["PVISOR_VM_FS_HOST_WRITEBACK"], "0")
                if backend == "staged":
                    self.assertIn("/bench-trial/stage", config["argv"])

    def test_trial_cannot_be_reused_and_caller_cache_overrides_are_removed(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            (batch / "toolroot").mkdir()
            folder = batch / "trial"
            folder.mkdir()
            with patch.dict(
                os.environ, {"PVISOR_VM_FS_LOWER_DATA_CACHE": "1", "HTTPS_PROXY": "ambient"}
            ):
                _, environment = B.build_command(batch, {}, folder, "M01", "native")
            self.assertNotIn("PVISOR_VM_FS_LOWER_DATA_CACHE", environment)
            self.assertNotIn("HTTPS_PROXY", environment)
            with self.assertRaises(FileExistsError):
                B.build_command(batch, {}, folder, "M01", "native")

    def test_private_install_replaces_leaf_alias_and_rejects_parent_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root, outside = directory / "root", directory / "outside"
            root.mkdir()
            outside.mkdir()
            source = directory / "source"
            source.write_bytes(b"frozen executable")
            target = outside / "untouched"
            target.write_bytes(b"original")
            (root / "leaf").symlink_to(target)
            B._install(source, root / "leaf", root)
            self.assertEqual(target.read_bytes(), b"original")
            self.assertEqual((root / "leaf").read_bytes(), source.read_bytes())
            (root / "escape").symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                B._install(source, root / "escape/newdir/executable", root)
            self.assertFalse((outside / "newdir").exists())


if __name__ == "__main__":
    unittest.main()
