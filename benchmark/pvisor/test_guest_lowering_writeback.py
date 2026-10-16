"""Conventional trust-boundary checks; no VM required."""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import guest_lowering_writeback as bench


class BenchmarkTests(unittest.TestCase):
    def payload(self):
        lines = ["CACHE_RESULT " + json.dumps({"phase": p, "ms": 1}) for p in bench.base.PHASES] + [
            "CACHE_PASS all"
        ]
        lines += [
            "WB_RESULT " + json.dumps({"case": c, "write_ms": 2, "sync_ms": 3, "total_ms": 5})
            for c in bench.CASES
        ]
        return ("\n".join(lines + ["WB_PASS all"]) + "\n").encode()

    def test_complete_and_contiguous_results(self):
        self.assertEqual(
            set(bench.parse(self.payload(), "all")), set(bench.METRICS) - {"completion_ms"}
        )
        bad = [
            self.payload() + b"WB_PASS all\n",
            self.payload().replace(b'"total_ms": 5', b'"total_ms": 6', 1),
            self.payload().replace(b'"write_ms": 2', b'"write_ms": true', 1),
            self.payload().replace(b'"write_ms": 2', b'"write_ms": NaN', 1),
            self.payload().replace(b'"case": "defer"', b'"case": "tiny"', 1),
        ]
        for value in bad:
            with self.subTest(value=value[:60]), self.assertRaises(ValueError):
                bench.parse(value, "all")

    def test_diagnostics_cannot_invent_metadata_timings(self):
        self.assertEqual(bench.parse(b"WB_PASS empty\n", "empty"), {})
        with self.assertRaises(ValueError):
            bench.parse(self.payload().replace(b"WB_PASS all", b"WB_PASS empty"), "empty")

    def test_host_checks_final_revision_and_exact_names(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            d = root / "write-output/overwrite"
            d.mkdir(parents=True)
            for i in range(16):
                (d / f"f{i:04}").write_bytes(bytes([(i + 63) % 251]) * 4096)
            self.assertEqual(len(bench.check_upper(root, "overwrite")), 16)
            (d / "f0000").write_bytes(bytes([0]) * 4096)
            with self.assertRaises(ValueError):
                bench.check_upper(root, "overwrite")
            (d / "extra").write_bytes(b"bad")
            with self.assertRaises(ValueError):
                bench.check_upper(root, "overwrite")

    def test_complete_factorial_and_known_paired_effect(self):
        report = {
            "state": "passed",
            "failures": [],
            "samples": 1,
            "cohorts": ["sdk"],
            "rows": [
                {"round": 0, "cohort": "sdk", "arm": a, "metrics": {"one": float(a)}}
                for a in bench.ARMS
            ],
        }
        with (
            patch.object(bench, "METRICS", ("one",)),
            patch.object(bench, "CONTRASTS", {"combined": (0, 3)}),
        ):
            self.assertEqual(bench.analyze(report)[0]["difference_ci95_ms"], [3, 3])
            for rows in [report["rows"][:-1], report["rows"] + [report["rows"][0]]]:
                with self.assertRaises(ValueError):
                    bench.analyze({**report, "rows": rows})
        with self.assertRaises(ValueError):
            bench.analyze({**report, "state": "failed"})

    @unittest.skipUnless(shutil.which("zstd"), "zstd required for retained stage evidence")
    def test_archive_checks_before_retiring_stage(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            stage = root / "stage"
            stage.mkdir()
            (stage / "bytes").write_bytes(b"checked")
            if sys.platform == "darwin":
                subprocess.run(
                    [
                        "/usr/bin/xattr",
                        "-wx",
                        "com.pvisor.benchmark",
                        "00ff00",
                        str(stage / "bytes"),
                    ],
                    check=True,
                )
            receipt = bench.archive_stage(root)
            self.assertFalse(stage.exists())
            self.assertGreater(receipt["bytes"], 0)
            record = json.loads((root / "stage-archive.json").read_text())
            self.assertEqual(
                record["members"]["stage/bytes"]["sha256"], hashlib.sha256(b"checked").hexdigest()
            )
            if sys.platform == "darwin":
                self.assertEqual(
                    record["members"]["stage/bytes"]["xattrs"]["com.pvisor.benchmark"], "AP8A"
                )
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            stage = root / "stage"
            stage.mkdir()
            (stage / "bytes").write_bytes(b"checked")
            with patch.object(bench, "validate_archive", side_effect=ValueError("corrupt")):
                with self.assertRaises(ValueError):
                    bench.archive_stage(root)
            self.assertEqual((stage / "bytes").read_bytes(), b"checked")


if __name__ == "__main__":
    unittest.main()
