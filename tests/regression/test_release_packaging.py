"""Real Cargo, linker-process and executable-replacement regressions."""

import json
import os
import shutil
import subprocess
import sys
import unittest
import zipfile

import yaml

from tests.fixtures import ROOT, workspace
from tests.packaging_fixtures import release_version, wheel_stage


class ReleasePackagingTests(unittest.TestCase):
    def test_wheel_contains_version_marker_but_not_kernel_build_inputs(self):
        with workspace() as (root, _):
            for name in (
                "pyproject.toml",
                "setup.py",
                "MANIFEST.in",
                "README.md",
                "LICENSE",
                "NOTICE",
            ):
                shutil.copyfile(ROOT / name, root / name)
            (root / "pvisor").mkdir()
            shutil.copyfile(ROOT / "pvisor/__init__.py", root / "pvisor/__init__.py")
            vdso = root / "pvisor-kernel/vdso"
            vdso.mkdir(parents=True)
            for target in ("x86_64", "aarch64"):
                (vdso / f"vdso_{target}.so").write_bytes(b"guest assembly build input")

            # Test the wheel packager without rebuilding native executables.
            result = subprocess.run(
                [
                    sys.executable,
                    "-c",
                    "from setuptools.build_meta import build_wheel; build_wheel('dist')",
                ],
                cwd=root,
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            (wheel,) = (root / "dist").glob("*.whl")
            with zipfile.ZipFile(wheel) as archive:
                modules = set()
                for entry in archive.namelist():
                    root_name, _, member = entry.partition("/")
                    if root_name.endswith(".dist-info"):
                        continue
                    if root_name.endswith(".data"):
                        scheme, _, member = member.partition("/")
                        if scheme not in ("purelib", "platlib"):
                            continue
                        entry = member
                    modules.add(entry)
                self.assertEqual(modules, {"pvisor/__init__.py"})

    def test_candidate_build_validates_versions_without_relaxing_tag_ancestry(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())["jobs"]
        steps = jobs["validate"]["steps"]
        validate = next(step for step in steps if step.get("id") == "version")["run"]
        fetch = next(step for step in steps if step.get("name") == "Fetch main release boundary")
        self.assertEqual(fetch["if"], "github.event_name == 'push'")
        for event, expected in (
            ("workflow_dispatch", ["run", "check-release-version"]),
            (
                "push",
                [
                    "run",
                    "check-release-version",
                    "--",
                    "--tag",
                    "v1.2.3",
                    "--main-ref",
                    "origin/main",
                ],
            ),
        ):
            with self.subTest(event=event), workspace() as (root, _):
                executable = root / "mise"
                executable.write_text(
                    '#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENTS"\nprintf "1.2.3\\n"\n'
                )
                executable.chmod(0o755)
                arguments, output = root / "arguments", root / "output"
                env = {
                    **os.environ,
                    "PATH": str(root) + os.pathsep + os.environ["PATH"],
                    "EVENT": event,
                    "RECOVERY": "false",
                    "RELEASE_TAG": "",
                    "GITHUB_REF_NAME": "v1.2.3",
                    "GITHUB_OUTPUT": str(output),
                    "ARGUMENTS": str(arguments),
                }
                subprocess.run(
                    ["bash", "-euo", "pipefail", "-c", validate], env=env, check=True, timeout=10
                )
                self.assertEqual(arguments.read_text().splitlines(), expected)
                self.assertIn("version=1.2.3", output.read_text())

    @unittest.skipIf(shutil.which("cargo") is None, "Cargo is needed to check lockfiles")
    def test_release_lockfile_rejects_stale_workspace_version(self):
        with workspace() as (tmp_path, resources):
            manifest = tmp_path / "Cargo.toml"
            manifest.write_text('[package]\nname = "lockfile-check"\nversion = "1.2.3"\n')
            (tmp_path / "src").mkdir()
            (tmp_path / "src/lib.rs").write_text("")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=tmp_path, check=True)
            release_version.validate_lockfile(tmp_path)

            manifest.write_text(manifest.read_text().replace("1.2.3", "1.2.4"))
            with self.assertRaisesRegex(release_version.ReleaseValidationError, "--locked"):
                release_version.validate_lockfile(tmp_path)

    def test_zig_linker_inherits_file_limit_without_changing_callers_limit(self):
        for soft, hard, expected_soft in [
            (1024, 32768, 16384),
            (1024, 4096, 4096),
            (32768, 32768, 32768),
        ]:
            with self.subTest(soft=soft, hard=hard, expected_soft=expected_soft):
                resource = __import__("resource")
                caller_limits = resource.getrlimit(resource.RLIMIT_NOFILE)
                if caller_limits[1] != resource.RLIM_INFINITY and caller_limits[1] < hard:
                    self.skipTest("test requires a sufficient open-file hard limit")
                # Lower limits only in a disposable build process, then observe what its
                # linker child inherits. The calling shell/test-runner limit must stay intact.
                code = (
                    "import json, resource, subprocess, sys\n"
                    f"sys.path.insert(0, {str(ROOT / 'scripts/packaging')!r})\n"
                    "from stage_wheel_binaries import _prepare_zig_file_limit\n"
                    f"resource.setrlimit(resource.RLIMIT_NOFILE, ({soft}, {hard}))\n"
                    "_prepare_zig_file_limit()\n"
                    "child = subprocess.check_output([sys.executable, '-c', "
                    "'import json, resource; print(json.dumps(resource.getrlimit(resource.RLIMIT_NOFILE)))'])\n"
                    "print(child.decode().strip())\n"
                )
                result = subprocess.run(
                    [sys.executable, "-c", code],
                    capture_output=True,
                    text=True,
                    check=True,
                    timeout=30,
                )
                self.assertEqual(json.loads(result.stdout), [expected_soft, hard])
                self.assertEqual(resource.getrlimit(resource.RLIMIT_NOFILE), caller_limits)

    @unittest.skipIf(os.name != "posix", "uses POSIX executable lifecycle")
    def test_native_artifact_replacement_preserves_a_running_image(self):
        with workspace() as (tmp_path, resources):
            destination = tmp_path / "pvisor"
            # Do not import macOS system-only file flags into the owned fixture.
            shutil.copy("/bin/sleep", destination)
            source = tmp_path / "replacement"
            shutil.copy("/usr/bin/true", source)
            if sys.platform == "darwin":
                # Copied Apple platform signatures are not valid for our owned paths.
                for executable in (source, destination):
                    subprocess.run(
                        ["codesign", "--force", "--sign", "-", str(executable)],
                        check=True,
                        capture_output=True,
                    )
            old_inode = destination.stat().st_ino
            process = subprocess.Popen([str(destination), "30"])
            try:
                wheel_stage.copy_artifact(source, destination)
                self.assertNotEqual(destination.stat().st_ino, old_inode)
                self.assertIsNone(process.poll())
                subprocess.run([str(destination)], check=True, timeout=3)
            finally:
                process.terminate()
                process.wait(timeout=3)
