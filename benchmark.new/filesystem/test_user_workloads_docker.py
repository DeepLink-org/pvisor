"""Offline Docker contracts; no daemon, image import or performance sampling."""

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "user_workloads_docker", HERE / "user_workloads_docker.py"
)
D = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(D)


class DockerContracts(unittest.TestCase):
    def configuration(self):
        return dict(
            docker_host="unix:///tmp/private-test.sock",
            budget_slice_unit="owned.slice",
            budget_cgroup="/sys/fs/cgroup/user.slice/owned.slice",
            image_id="sha256:" + "a" * 64,
        )

    def inspection(self, config):
        return dict(
            Config=dict(Labels={D.LABEL: config["token"]}),
            State=dict(Running=False, ExitCode=0, OOMKilled=False),
            GraphDriver=dict(Name="overlay2"),
            HostConfig=dict(CgroupParent="owned.slice"),
            Mounts=[dict(Destination="/bench-trial/work/input", RW=False)],
        )

    def create(self, batch, folder):
        protocol = dict(
            reference_configuration=dict(docker=self.configuration()), rounds=3, ram_mib=512
        )
        with patch.object(
            D.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "b" * 64, "")
        ):
            command, environment = D.build_command(batch, protocol, folder, "M06")
        return (
            protocol,
            command,
            environment,
            json.loads((folder / "docker-launch.json").read_text()),
        )

    def test_create_uses_overlay_output_and_only_readonly_input_bind(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            folder = batch / "trial"
            folder.mkdir()
            with patch.dict(
                os.environ,
                {"DOCKER_HOST": "foreign", "PVISOR_VM_FS_WRITEBACK": "1", "HTTPS_PROXY": "foreign"},
            ):
                protocol, command, environment, config = self.create(batch, folder)
            argv = config["create_command"]
            self.assertEqual(
                argv[:3], ["/usr/bin/docker", "--host", "unix:///tmp/private-test.sock"]
            )
            self.assertEqual(argv[argv.index("--cgroup-parent") + 1], "owned.slice")
            self.assertEqual(argv[argv.index("--cgroupns") + 1], "host")
            self.assertEqual(argv.count("--mount"), 1)
            self.assertIn("dst=/bench-trial/work/input,readonly", argv[argv.index("--mount") + 1])
            self.assertNotIn("--tmpfs", argv)
            self.assertEqual(
                argv[-6:], ["/bench-artifacts/payload", "M06", "output", "input", "3", "512"]
            )
            self.assertEqual(command[-2:], ["--start", str(folder / "docker-launch.json")])
            self.assertFalse(
                any(
                    k in environment
                    for k in ("DOCKER_HOST", "PVISOR_VM_FS_WRITEBACK", "HTTPS_PROXY")
                )
            )
            with self.assertRaises(FileExistsError):
                D.build_command(batch, protocol, folder, "M06")

    def test_finalize_checks_parent_affinity_and_real_overlay_before_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            folder = batch / "trial"
            folder.mkdir()
            protocol, _, _, config = self.create(batch, folder)
            marker = dict(
                affinity=[0, 1],
                root_filesystem="overlay",
                cgroup_namespace_path="0::/user.slice/owned.slice/docker-test.scope",
            )
            (folder / "stdout.log").write_text(D.RUNTIME_MARKER + json.dumps(marker) + "\n")
            with (
                patch.object(D, "_inspect_owned", return_value=self.inspection(config)),
                patch.object(D, "_checked") as copied,
            ):
                output = D.finalize_trial(batch, protocol, folder)
            self.assertEqual(output, folder / "docker-output")
            self.assertTrue(output.is_dir())
            self.assertEqual(copied.call_args.args[1], "cp")
            self.assertIn(":/bench-trial/work/output/.", copied.call_args.args[2])

    def test_finalize_rejects_budget_escape_or_tmpfs_and_does_not_copy(self):
        for error in ("budget", "storage", "oom"):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as temporary:
                batch = Path(temporary)
                folder = batch / "trial"
                folder.mkdir()
                protocol, _, _, config = self.create(batch, folder)
                marker = dict(
                    affinity=[0, 1],
                    root_filesystem="overlay",
                    cgroup_namespace_path="0::/user.slice/owned.slice/docker-test.scope",
                )
                inspection = self.inspection(config)
                if error == "budget":
                    marker["cgroup_namespace_path"] = (
                        "0::/user.slice/foreign.slice/docker-test.scope"
                    )
                elif error == "storage":
                    inspection["Mounts"].append(
                        dict(Destination="/bench-trial/work/output", RW=True)
                    )
                else:
                    inspection["State"]["OOMKilled"] = True
                (folder / "stdout.log").write_text(D.RUNTIME_MARKER + json.dumps(marker) + "\n")
                with (
                    patch.object(D, "_inspect_owned", return_value=inspection),
                    patch.object(D, "_checked") as copied,
                ):
                    with self.assertRaises(ValueError):
                        D.finalize_trial(batch, protocol, folder)
                    copied.assert_not_called()

    def test_cleanup_never_removes_foreign_container(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            folder = batch / "trial"
            folder.mkdir()
            protocol, _, _, config = self.create(batch, folder)
            wrong = self.inspection(config)
            wrong["Config"]["Labels"][D.LABEL] = "foreign"
            response = subprocess.CompletedProcess([], 0, json.dumps([wrong]), "")
            with patch.object(D.subprocess, "run", return_value=response) as command:
                with self.assertRaises(ValueError):
                    D.cleanup_trial(batch, protocol, folder)
            self.assertTrue(all("rm" not in call.args[0] for call in command.call_args_list))

    def test_preparation_rejects_unmatched_parent_before_importing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            setup = root / "setup.json"
            setup.write_text(json.dumps(dict(state="failed")))
            with patch.object(D.subprocess, "Popen") as started:
                with self.assertRaises(ValueError):
                    D.prepare_assets(root, setup)
                started.assert_not_called()

    def test_cleanup_still_removes_owned_container_after_snapshot_timeout(self):
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            folder = batch / "trial"
            folder.mkdir()
            protocol, _, _, config = self.create(batch, folder)
            inspection = self.inspection(config)
            inspection["State"]["Running"] = True

            def run(argv, **kwargs):
                if "inspect" in argv:
                    return subprocess.CompletedProcess(argv, 0, json.dumps([inspection]), "")
                if "cp" in argv:
                    raise subprocess.TimeoutExpired(argv, 30, stderr="snapshot too large")
                return subprocess.CompletedProcess(argv, 0, "ok", "")

            with patch.object(D.subprocess, "run", side_effect=run) as command:
                D.cleanup_trial(batch, protocol, folder)
            self.assertTrue(any("kill" in call.args[0] for call in command.call_args_list))
            self.assertTrue(any("rm" in call.args[0] for call in command.call_args_list))
            snapshot = json.loads((folder / "docker-failure-snapshot.json").read_text())
            self.assertIn("snapshot too large", snapshot["stderr"])


if __name__ == "__main__":
    unittest.main()
