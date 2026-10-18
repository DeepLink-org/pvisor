"""Offline reference VM launch, artifact and independent validation contracts."""

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "user_workloads_reference_vm", HERE / "user_workloads_reference_vm.py"
)
R = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(R)


class ReferenceVMContracts(unittest.TestCase):
    def protocol(self, root):
        assets = dict(
            root_disk=str(root / "root-template.ext4"),
            work_disk=str(root / "work-template.ext4"),
            kernel_paths=dict(
                kernel="/stock/kernel", vmlinuz="/stock/vmlinuz", initrd="/stock/initrd"
            ),
            qemu_data_dir="/frozen/qemu-data",
            qemu_firmware={
                name: {"path": "/frozen/qemu-data/" + name}
                for name in ("qboot.rom", "bios-256k.bin")
            },
            runtimes={
                "firecracker": {"executable": "/frozen/firecracker"},
                "qemu-system-x86_64": {"executable": "/frozen/qemu"},
            },
        )
        return dict(
            rounds=3,
            ram_mib=512,
            reference_configuration={key: {"assets": assets} for key in R.BACKENDS},
        )

    def test_three_backends_have_private_disks_and_honour_guest_flush(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            protocol = self.protocol(root)
            with patch.object(R, "_run") as execute:
                for backend in R.BACKENDS:
                    folder = root / backend
                    folder.mkdir()
                    command, env = R.build_command(root, protocol, folder, "S03", backend)
                    self.assertEqual(command[-2], "--launch")
                    self.assertEqual(env["LC_ALL"], "C")
                    launch = json.loads((folder / "reference-launch.json").read_text())
                    self.assertEqual(launch["guest_memory_mib"], 512)
                    self.assertEqual(launch["guest_vcpus"], 2)
                    argv = launch["argv"]
                    if backend == "fc-system":
                        cfg = json.loads((folder / "firecracker.json").read_text())
                        self.assertTrue(all(d["cache_type"] == "Writeback" for d in cfg["drives"]))
                        self.assertEqual(cfg["boot-source"]["kernel_image_path"], "/stock/kernel")
                        self.assertFalse(any("net" in key for key in cfg))
                    else:
                        self.assertEqual(argv[argv.index("-kernel") + 1], "/stock/vmlinuz")
                        self.assertEqual(sum("cache=writeback" in token for token in argv), 2)
                        self.assertEqual(argv[argv.index("-L") + 1], "/frozen/qemu-data")
                        self.assertEqual(
                            argv[argv.index("-bios") + 1],
                            "/frozen/qemu-data/"
                            + ("qboot.rom" if backend == "qemu-microvm" else "bios-256k.bin"),
                        )
                        self.assertNotIn("-nic", argv)
                    self.assertEqual(
                        sum("fsbench.case=S03" in token for token in argv),
                        0 if backend == "fc-system" else 1,
                    )
            self.assertEqual(execute.call_count, 6)

    def test_failed_guest_and_unclean_fs_never_reach_output_oracle(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for stdout in (
                "WORKLOADS_V2_DONE {}\n",
                'USER_WORKLOADS_VM_EXIT {"returncode":1,"teardown_ok":true}\n',
                'USER_WORKLOADS_VM_EXIT {"returncode":0,"teardown_ok":false}\n',
                'Kernel panic - not syncing\nUSER_WORKLOADS_VM_EXIT {"returncode":0,"teardown_ok":true}\n',
            ):
                (root / "stdout.log").write_text(stdout)
                with patch.object(R, "_run") as execute:
                    with self.assertRaises(ValueError):
                        R.finalize_trial(root, {}, root, "S03", "qemu")
                    execute.assert_not_called()
            (root / "stdout.log").write_text(
                'USER_WORKLOADS_VM_EXIT {"returncode":0,"teardown_ok":true}\n'
            )
            with patch.object(R, "_run", side_effect=RuntimeError("filesystem not clean")):
                with self.assertRaises(RuntimeError):
                    R.finalize_trial(root, {}, root, "S03", "qemu")
            self.assertFalse((root / "reference-output").exists())

    def test_success_exports_read_only_after_clean_unmount(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "stdout.log").write_text(
                'USER_WORKLOADS_VM_EXIT {"returncode":0,"teardown_ok":true}\n'
            )
            calls = []

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "debugfs":
                    (root / "reference-output/output").mkdir()
                return subprocess.CompletedProcess(command, 0, "", "")

            with patch.object(R, "_run", side_effect=run):
                output = R.finalize_trial(root, {}, root, "S03", "qemu")
            self.assertEqual(output, root / "reference-output/output")
            self.assertEqual(calls[0][:3], ["e2fsck", "-f", "-n"])
            self.assertEqual(calls[1][:2], ["debugfs", "-R"])
            self.assertNotIn("-w", calls[1])

    def test_cleanup_never_follows_an_aliased_disk(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            victim = root / "retained-input"
            victim.write_bytes(b"untouched")
            (root / "root.ext4").symlink_to(victim)
            with self.assertRaises(ValueError):
                R.cleanup_trial(root, {}, root, "S01", "qemu")
            self.assertEqual(victim.read_bytes(), b"untouched")


if __name__ == "__main__":
    unittest.main()
