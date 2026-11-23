"""Native shell installation against controlled GitHub Release assets."""

import hashlib
import io
import os
import platform
import subprocess
import tarfile
import unittest

from tests.fixtures import ROOT, workspace

PLATFORM = {
    ("Linux", "x86_64"): "linux-x86_64",
    ("Darwin", "arm64"): "macos-arm64",
}.get((platform.system(), platform.machine()))
FILES = (
    "LICENSE",
    "NOTICE",
    "bin/libkrunfw.SOURCE",
    "bin/pvisor",
    "bin/pvisor-replay",
    "bin/pvisor-daemon",
)


@unittest.skipUnless(PLATFORM, "native installer supports Linux x86_64 and Apple Silicon")
class NativeInstallerTests(unittest.TestCase):
    def setUp(self):
        root, _ = self.enterContext(workspace())
        self.install_dir = root / "CLI with spaces"
        self.releases = root / "releases"
        self.home = root / "home"
        self.home.mkdir()
        self.requests = root / "requests"
        curl = root / "curl"
        curl.write_text(
            """#!/bin/sh
set -eu
while [ "$#" -gt 2 ]; do
    case "$1" in --output) break ;; esac
    shift
done
[ "$1" = --output ]
[ "$#" -eq 3 ]
case "$3" in https://github.com/DeepLink-org/pvisor/releases/*) ;; *) exit 1 ;; esac
path=${3#*/pvisor/releases/}
printf '%s\\n' "$path" >> "$REQUESTS"
cp "$RELEASES/$path" "$2"
"""
        )
        curl.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": str(root) + os.pathsep + os.environ["PATH"],
            "HOME": str(self.home),
            "RELEASES": str(self.releases),
            "REQUESTS": str(self.requests),
        }

    def release(self, version, *, checksum="valid", missing=None, latest=False, nightly=False):
        filename = f"pvisor-{version}-{PLATFORM}.tar.gz"
        tag = "nightly" if nightly else f"v{version}"
        directory = self.releases / "download" / tag
        directory.mkdir(parents=True, exist_ok=nightly)
        archive = directory / filename
        with tarfile.open(archive, "w:gz") as bundle:
            for name in FILES:
                if name == missing:
                    continue
                executable = name.startswith("bin/pvisor")
                payload = f"#!/bin/sh\nprintf '%s\\n' {version}\n" if executable else name
                data = payload.encode()
                item = tarfile.TarInfo(name)
                item.mode = 0o755 if executable else 0o644
                item.size = len(data)
                bundle.addfile(item, io.BytesIO(data))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        index = f"{digest if checksum != 'bad' else '0' * 64}  {filename}\n"
        if checksum == "duplicate":
            index *= 2
        elif checksum == "missing":
            index = f"{digest}  unrelated.tar.gz\n"
        (directory / "SHA256SUMS").write_text(index)
        if latest:
            pointer = self.releases / "latest" / "download" / "SHA256SUMS"
            pointer.parent.mkdir(parents=True, exist_ok=True)
            pointer.write_text(index)

    def install(self, *args, default_dir=False):
        command = ["sh", "-s", "--"]
        if not default_dir:
            command.extend(["--install-dir", str(self.install_dir)])
        return subprocess.run(
            [*command, *args],
            input=(ROOT / "scripts/install.sh").read_text(),
            env=self.env,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def assert_installed(self, root, version):
        self.assertEqual(os.readlink(root / "current"), f"versions/{version}")
        for binary in ("pvisor", "pvisor-replay", "pvisor-daemon"):
            result = subprocess.check_output(
                [str(root / "current" / "bin" / binary)], text=True, timeout=3
            )
            self.assertEqual(result.strip(), version)
        for name in FILES[:3]:
            self.assertEqual((root / "current" / name).read_text(), name)
        self.assertFalse((root / ".install-lock").exists())
        self.assertEqual(list(root.glob(".stage.*")), [])

    def test_latest_resolves_once_then_fetches_a_pinned_archive(self):
        self.release("0.4.0", latest=True)
        result = self.install(default_dir=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        root = self.home / ".local/share/pvisor/cli"
        self.assert_installed(root, "0.4.0")
        self.assertEqual(
            self.requests.read_text().splitlines(),
            ["latest/download/SHA256SUMS", f"download/v0.4.0/pvisor-0.4.0-{PLATFORM}.tar.gz"],
        )
        self.assertFalse((self.home / ".profile").exists())

    def test_nightly_uses_its_own_release_and_installation(self):
        self.release("0.4.0", latest=True)
        self.release("0.4.1+g42.a1b2c3d", nightly=True)
        stable = self.install(default_dir=True)
        self.assertEqual(stable.returncode, 0, stable.stderr)
        nightly = self.install("--nightly", default_dir=True)
        self.assertEqual(nightly.returncode, 0, nightly.stderr)
        self.assertEqual(
            self.requests.read_text().splitlines()[-2:],
            [
                "download/nightly/SHA256SUMS",
                f"download/nightly/pvisor-0.4.1+g42.a1b2c3d-{PLATFORM}.tar.gz",
            ],
        )
        self.release("0.4.1+g43.b2c3d4e", nightly=True, checksum="bad")
        failed = self.install("--nightly", default_dir=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("checksum mismatch", failed.stderr)
        self.assert_installed(self.home / ".local/share/pvisor/cli", "0.4.0")
        nightly_root = self.home / ".local/share/pvisor/cli-nightly"
        self.assert_installed(nightly_root, "0.4.1+g42.a1b2c3d")
        self.assertFalse((nightly_root / "versions/0.4.1+g43.b2c3d4e").exists())

    def test_nightly_cannot_select_a_stable_pin(self):
        result = self.install("--nightly", "--version", "0.4.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--nightly and --version cannot be combined", result.stderr)
        self.assertFalse(self.requests.exists())

    def test_nightly_rejects_a_stable_archive(self):
        self.release("0.4.0", nightly=True)
        result = self.install("--nightly")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected one valid", result.stderr)
        self.assertEqual(self.requests.read_text().splitlines(), ["download/nightly/SHA256SUMS"])
        self.assertFalse((self.install_dir / "current").exists())

    def test_incomplete_script_does_not_start_installing(self):
        self.release("0.4.0", latest=True)
        script = (ROOT / "scripts/install.sh").read_text()
        self.assertTrue(script.endswith("}\n"))
        result = subprocess.run(
            ["sh", "-s", "--", "--install-dir", str(self.install_dir)],
            input=script.removesuffix("}\n"),
            env=self.env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.requests.exists())
        self.assertFalse(self.install_dir.exists())

    def test_upgrade_and_rollback_select_one_complete_component_set(self):
        self.release("0.4.0")
        self.release("0.4.1")
        for version in ("0.4.0", "0.4.1", "0.4.0"):
            with self.subTest(version=version):
                result = self.install("--version", f"v{version}")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_installed(self.install_dir, version)
        self.assertTrue((self.install_dir / "versions/0.4.1/bin/pvisor").exists())

    def test_bad_release_does_not_change_the_active_installation(self):
        self.release("0.4.0")
        result = self.install("--version", "0.4.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        for version, checksum, missing, error in (
            ("0.4.1", "bad", None, "checksum mismatch"),
            ("0.4.2", "duplicate", None, "expected one valid"),
            ("0.4.3", "missing", None, "expected one valid"),
            ("0.4.4", "valid", "bin/pvisor-replay", "archive is missing"),
        ):
            with self.subTest(version=version):
                self.release(version, checksum=checksum, missing=missing)
                result = self.install("--version", version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)
                self.assert_installed(self.install_dir, "0.4.0")
                self.assertFalse((self.install_dir / "versions" / version).exists())

    def test_installer_refuses_to_replace_an_unmanaged_current_directory(self):
        self.release("0.4.0")
        directory = self.install_dir / "current"
        directory.mkdir(parents=True)
        marker = directory / "keep"
        marker.write_text("untouched")
        result = self.install("--version", "0.4.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a symlink", result.stderr)
        self.assertEqual(marker.read_text(), "untouched")
