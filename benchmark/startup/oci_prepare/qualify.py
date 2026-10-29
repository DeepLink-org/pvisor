import argparse
import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

from audit_image import audit
from restore_metadata import apply_metadata

UMOCI = None
INSTALLED_UMOCI = None


def run(args):
    return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60).stdout


def command(arm, source, bundle):
    return [
        str(UMOCI),
        "raw",
        "unpack",
        "--rootless",
        *(["--pvisor-metadata"] if arm == "raw" else []),
        "--image",
        str(source) + ":vuls",
        str(bundle / "rootfs"),
    ]


class Tests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.t = Path(self.tmp.name)
        self.oci = self.t / "oci"
        self.layer = self.t / "layer.tar"
        run([str(UMOCI), "init", "--layout", str(self.oci)])
        run([str(UMOCI), "new", "--image", str(self.oci) + ":vuls"])
        with tarfile.open(self.layer, "w", format=tarfile.PAX_FORMAT) as f:
            for name, kind, mode in [
                ("dir", tarfile.DIRTYPE, 0o555),
                ("dir/file", tarfile.REGTYPE, 0o4755),
                ("hard", tarfile.LNKTYPE, 0o4755),
                ("sym", tarfile.SYMTYPE, 0o777),
            ]:
                m = tarfile.TarInfo(name)
                m.type = kind
                m.uid = 123
                m.gid = 456
                m.mode = mode
                m.mtime = 123456
                if kind in (tarfile.REGTYPE, tarfile.LNKTYPE):
                    m.pax_headers = {
                        "SCHILY.xattr.user.test": "value",
                        "SCHILY.xattr.user.rootlesscontainers": "archived",
                    }
                if kind == tarfile.DIRTYPE:
                    m.pax_headers = {"SCHILY.xattr.user.parent": "kept"}
                if kind == tarfile.REGTYPE:
                    m.size = 3
                if kind in (tarfile.LNKTYPE, tarfile.SYMTYPE):
                    m.linkname = "dir/file"
                if kind == tarfile.SYMTYPE:
                    m.uid = os.getuid()
                    m.gid = os.getgid()
                f.addfile(m, io.BytesIO(b"abc") if kind == tarfile.REGTYPE else None)
        run([str(UMOCI), "raw", "add-layer", "--image", str(self.oci) + ":vuls", str(self.layer)])
        # Canonical test input is uncompressed, unlike raw add-layer's default output.
        idx = json.loads((self.oci / "index.json").read_text())
        desc = idx["manifests"][0]
        manifest = json.loads(
            (self.oci / "blobs/sha256" / desc["digest"].split(":")[1]).read_text()
        )
        payload = self.layer.read_bytes()
        digest = hashlib.sha256(payload).hexdigest()
        (self.oci / "blobs/sha256" / digest).write_bytes(payload)
        manifest["layers"] = [
            {
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": "sha256:" + digest,
                "size": len(payload),
            }
        ]
        data = json.dumps(manifest).encode()
        digest = hashlib.sha256(data).hexdigest()
        (self.oci / "blobs/sha256" / digest).write_bytes(data)
        desc.update(digest="sha256:" + digest, size=len(data))
        (self.oci / "index.json").write_text(json.dumps(idx))

    def tearDown(self):
        self.tmp.cleanup()

    def view(self, root):
        out = {}
        for p in root.rglob("*"):
            s = p.lstat()
            out[str(p.relative_to(root))] = (
                s.st_uid,
                s.st_gid,
                s.st_mode,
                {
                    k: os.getxattr(p, k, follow_symlinks=False)
                    for k in os.listxattr(p, follow_symlinks=False)
                },
                os.readlink(p) if p.is_symlink() else (p.read_bytes() if p.is_file() else None),
            )
        return out

    def test_exact_original_audit(self):
        for arm in ("standard", "raw"):
            b = self.t / arm
            b.mkdir()
            run(command(arm, self.oci, b))
            if arm == "standard":
                apply_metadata(self.layer, b / "rootfs")
            self.assertTrue(audit(self.layer, b / "rootfs")["passed"])
        self.assertEqual(self.view(self.t / "standard/rootfs"), self.view(self.t / "raw/rootfs"))
        p = self.t / "raw/rootfs/dir/file"
        p.write_bytes(b"bad")
        with self.assertRaises(AssertionError):
            audit(self.layer, self.t / "raw/rootfs")

    def test_default_matches_installed(self):
        for name, binary in [("new", UMOCI), ("installed", INSTALLED_UMOCI)]:
            b = self.t / name
            b.mkdir()
            run(
                [
                    str(binary),
                    "raw",
                    "unpack",
                    "--rootless",
                    "--image",
                    str(self.oci) + ":vuls",
                    str(b / "rootfs"),
                ]
            )
        self.assertEqual(self.view(self.t / "new/rootfs"), self.view(self.t / "installed/rootfs"))

    def test_reject_existing_destination(self):
        b = self.t / "raw"
        b.mkdir()
        (b / "rootfs").mkdir()
        with self.assertRaises(subprocess.CalledProcessError):
            run(command("raw", self.oci, b))

    def test_reject_nonrootless(self):
        b = self.t / "raw"
        b.mkdir()
        args = command("raw", self.oci, b)
        args.remove("--rootless")
        with self.assertRaises(subprocess.CalledProcessError):
            run(args)

    def test_reject_keepdirlinks(self):
        b = self.t / "raw"
        b.mkdir()
        args = command("raw", self.oci, b)
        args.insert(3, "--keep-dirlinks")
        with self.assertRaises(subprocess.CalledProcessError):
            run(args)

    def test_reject_multilayer(self):
        run([str(UMOCI), "raw", "add-layer", "--image", str(self.oci) + ":vuls", str(self.layer)])
        b = self.t / "raw"
        b.mkdir()
        with self.assertRaises(subprocess.CalledProcessError):
            run(command("raw", self.oci, b))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Small real OCI qualification only; no VM or SWE.")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    args = parser.parse_args()
    UMOCI = args.binary.resolve()
    INSTALLED_UMOCI = args.reference.resolve()
    assert os.geteuid() != 0, "Run rootless qualification as an ordinary user"
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Tests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)
