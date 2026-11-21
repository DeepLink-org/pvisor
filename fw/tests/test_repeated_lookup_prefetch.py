"""Exercise the guest lookup path with an explicitly selected Python 3 image."""

import json
import os
import subprocess
import unittest

IMAGE = os.environ.get("PVISOR_PREFETCH_TEST_IMAGE")
GUEST = r"""
import sysconfig
from pathlib import Path

stdlib = Path(sysconfig.get_path("stdlib"))
directory = stdlib / "concurrent"
missing = directory / "pvisor-missing-lookup-probe"
renamed = directory / "pvisor-renamed-lookup-probe"
def assert_missing(path):
    try:
        path.stat()
    except FileNotFoundError:
        return
    raise AssertionError(f"unexpectedly present: {path}")

for _ in range(2000):
    assert_missing(missing)
missing.write_bytes(b"new")
assert missing.read_bytes() == b"new"
assert_missing(renamed)
missing.rename(renamed)
assert_missing(missing)
assert renamed.read_bytes() == b"new"
renamed.unlink()
assert_missing(renamed)

# The first bounded window may cover only part of this larger directory.
large = stdlib / "encodings"
for _ in range(16):
    assert_missing(large / "pvisor-absent-codec")
assert (large / "zlib_codec.py").read_bytes()
"""


@unittest.skipUnless(IMAGE, "set PVISOR_PREFETCH_TEST_IMAGE to a local Python 3 image")
class RepeatedLookupTests(unittest.TestCase):
    def test_negative_lookup_traffic_and_mutation_visibility(self):
        counts = []
        for enabled in ("0", "1"):
            env = dict(os.environ, PVISOR_FS_PROFILE="1", PVISOR_VM_GUEST_CACHE=enabled,
                       PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE="1",
                       PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE="0")
            command = [env.get("PVISOR_TEST_CLI", "pvisor"), "run", "--rm",
                       "--network", "none", "--entrypoint", "/usr/local/bin/python3",
                       IMAGE, "-c", GUEST]
            result = subprocess.run(command, env=env, text=True, capture_output=True,
                                    timeout=120)
            self.assertEqual(result.returncode, 0, result.stderr[-2000:])
            owners = []
            for line in result.stderr.splitlines():
                if line.startswith("pvisor-fs-profile "):
                    report = json.loads(line.split(" ", 1)[1])
                    if report["final_record"] and "negative_entry_cache_enabled" in report["measurements"]:
                        owners.append(report["measurements"])
            self.assertTrue(owners, "missing final overlay filesystem profile")
            counts.append(sum(m["lookup"]["calls"] for m in owners))
        self.assertGreaterEqual(counts[0] - counts[1], 1500, counts)

    @unittest.skipUnless(os.environ.get("PVISOR_PREFETCH_TEST_LARGE_DIR"),
                         "set PVISOR_PREFETCH_TEST_LARGE_DIR to a partial-window image directory")
    def test_large_directory_continuation(self):
        guest = """
import fcntl, os, struct, sys
fd = os.open(sys.argv[1], os.O_DIRECTORY)
def checkpoint(token):
    fcntl.ioctl(fd, 0x400876f1, struct.pack('<Q', token))

checkpoint(1)
for i in range(2000):
    name = 'pvisor-test-missing' if i < 6 else f'pvisor-test-missing-{i}'
    try:
        os.stat(name, dir_fd=fd)
    except FileNotFoundError:
        pass
    else:
        raise AssertionError(name)
    if i == 0:
        checkpoint(2)
checkpoint(3)
"""
        env = dict(os.environ, PVISOR_FS_PROFILE="1", PVISOR_VM_GUEST_CACHE="1",
                   PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE="1", PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE="0")
        command = [env.get("PVISOR_TEST_CLI", "pvisor"), "run", "--rm", "--network", "none",
                   "--entrypoint", "/usr/local/bin/python3", IMAGE, "-c", guest,
                   env["PVISOR_PREFETCH_TEST_LARGE_DIR"]]
        result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stderr[-2000:])
        phases = {}
        for line in result.stderr.splitlines():
            if line.startswith("pvisor-fs-profile-phase "):
                report = json.loads(line.split(" ", 1)[1])
                protocol = next(p for p in report["profiles"] if p["component"] == "virtio-fs-protocol")
                phases[report["token"]] = protocol["measurements"]
        def calls(token, operation):
            return phases[token].get(operation, {}).get("calls", 0) - phases[1].get(operation, {}).get("calls", 0)

        self.assertEqual(calls(2, "LOOKUP"), 1)
        self.assertEqual(calls(2, "READINDEX"), 0)
        self.assertIn(calls(3, "READINDEX"), range(2, 5))
        self.assertLess(calls(3, "LOOKUP"), 100)
