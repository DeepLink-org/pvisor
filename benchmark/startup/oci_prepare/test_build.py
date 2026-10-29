"""Packaging guards; no external tools or performance workloads."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import build


class BuildGuards(unittest.TestCase):
    def test_bad_archive_does_not_create_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "archive"
            archive.write_bytes(b"not the pinned archive")
            with self.assertRaisesRegex(ValueError, "checksum"):
                build.build(archive, root / "output", Path("/unused/go"))
            self.assertFalse((root / "output").exists())

    def test_existing_output_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output"
            output.mkdir()
            marker = output / "failure.log"
            marker.write_text("retain")
            with patch.object(build, "digest", return_value=build.ARCHIVE_SHA256):
                with self.assertRaises(FileExistsError):
                    build.build(root / "archive", output, Path("/unused/go"))
            self.assertEqual(marker.read_text(), "retain")


if __name__ == "__main__":
    unittest.main()
