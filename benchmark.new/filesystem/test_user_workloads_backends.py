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
    def test_runtime_override_rejects_corruption_incomplete_receipts_and_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir()
            for name in ("pvisor", "payload", "workload", "libkrunfw.so.5.6.2"):
                (runtime / name).write_bytes(name.encode())
            (runtime / "libkrunfw.so.5").symlink_to("libkrunfw.so.5.6.2")
            frozen = {"path": str(runtime), "sha256": {p.name: B.sha(p) for p in runtime.iterdir()}}
            self.assertEqual(B.runtime_artifact_source(frozen), runtime)
            with self.assertRaisesRegex(ValueError, "incomplete"):
                B.runtime_artifact_source({"path": str(runtime), "sha256": {}})
            (runtime / "pvisor").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "identity mismatch"):
                B.runtime_artifact_source(frozen)
            (runtime / "pvisor").unlink()
            outside = root / "outside"
            outside.write_bytes(b"pvisor")
            (runtime / "pvisor").symlink_to(outside)
            with self.assertRaisesRegex(ValueError, "identity mismatch"):
                B.runtime_artifact_source(frozen)

    def test_authenticated_inputs_replace_root_and_keep_existing_limits(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            (batch / "toolroot").mkdir()
            inputs = {}
            for kind in ("rootfs", "workspace"):
                store = batch / kind
                store.mkdir()
                inputs[kind] = {"store": str(store), "base": {"id": kind + "-base"}}
            for diagnostic in (False, True):
                folder = batch / str(diagnostic)
                folder.mkdir()
                _, env = B.build_command(
                    batch,
                    {
                        "prepared_inputs": inputs,
                        "krun_enomem_workaround": "0",
                        "diagnostic_profile": diagnostic,
                        "role": "diagnostic",
                    },
                    folder,
                    "S03",
                    "vm-writeback",
                )
                launch = json.loads((folder / "launch.json").read_text())
                argv = launch["argv"]
                self.assertNotIn("--rootfs", argv)
                for kind in inputs:
                    self.assertEqual(
                        argv[argv.index(f"--{kind}-base-store") + 1], f"/bench-prepared/{kind}"
                    )
                    self.assertLess(argv.index(f"--{kind}-base"), argv.index("--"))
                self.assertEqual(env["PVISOR_FS_PROFILE"], str(int(diagnostic)))
                self.assertEqual(env["PVISOR_VM_GUEST_CACHE"], "1")
                self.assertEqual(env["PVISOR_VM_FS_WRITEBACK"], "1")
                self.assertEqual(argv[-2:], ["3", "512"])
                self.assertEqual(launch["prepared_inputs"], inputs)

    def test_external_stage_keeps_guest_path_and_host_oracle_location(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary).resolve()
            (batch / "toolroot").mkdir()
            external = batch / "ssd"
            external.mkdir()
            folder = batch / "trial"
            folder.mkdir()
            B.build_command(batch, {"stage_storage": str(external)}, folder, "S03", "vm-writeback")
            config = json.loads((folder / "launch.json").read_text())
            self.assertEqual(config["stage_storage"], str(external / "trial"))
            self.assertTrue((external / "trial").is_dir())
            self.assertEqual(
                config["argv"][config["argv"].index("--stage") + 1], "/bench-trial/stage"
            )
            self.assertEqual(
                B.output_root(folder, "vm-writeback"), (external / "trial/upper/output", True)
            )
            self.assertEqual(config["folder"], str(folder))

    def test_frozen_kvm_selection_survives_ambient_environment_scrub(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            (batch / "toolroot").mkdir()
            for selection in ("auto", "0", "1"):
                folder = batch / selection
                folder.mkdir()
                with patch.dict(os.environ, {"PVISOR_KRUN_ENOMEM_WORKAROUND": "1"}):
                    _, env = B.build_command(
                        batch,
                        {"krun_enomem_workaround": selection},
                        folder,
                        "S02",
                        "vm-writeback",
                    )
                key = "PVISOR_KRUN_ENOMEM_WORKAROUND"
                if selection == "auto":
                    self.assertNotIn(key, env)
                else:
                    self.assertEqual(env[key], selection)
                    self.assertEqual(
                        json.loads((folder / "launch.json").read_text())["vm_environment"][key],
                        selection,
                    )
                self.assertEqual(env["PVISOR_VM_FS_WRITEBACK"], "1")
                self.assertEqual(env["PVISOR_VM_FS_HOST_WRITEBACK"], "0")

    def test_reference_adapters_own_launch_export_and_cleanup(self):
        expected = (["private-reference"], {"LC_ALL": "C"})
        for backend, module in (
            ("agentenv", "user_workloads_agentenv"),
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
