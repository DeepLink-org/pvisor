"""Real Cargo, linker-process and executable-replacement regressions."""

import json
import os
import shutil
import subprocess
import sys
import unittest

from tests.fixtures import ROOT, workspace
from tests.packaging_fixtures import release_version, wheel_stage


class ReleasePackagingTests(unittest.TestCase):
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
