"""Admission of complete measured blocks only; these fixtures do not measure a VM."""

import json
import runpy
import tempfile
import unittest
from pathlib import Path

measured_blocks = runpy.run_path(
    str(Path(__file__).resolve().parents[1] / "vm/summarize_tree_granularity.py")
)["measured_blocks"]


class PairedSummary(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.batch = Path(temp.name)
        sample = self.batch / "criterion/native-tree/producer-write-size/new/sample.json"
        sample.parent.mkdir(parents=True)
        sample.write_text('{"iters":[1.0,2.0]}')
        for n in range(1, 5):
            path = self.batch / f"raw/block-{n:04}/block.json"
            path.parent.mkdir(parents=True)
            path.write_text(
                json.dumps(
                    [
                        {"write_kib": k, "arm": arm, "end_to_end_ns": n}
                        for k in (1, 16, 256)
                        for arm in ("shared", "local")
                    ]
                )
            )
        (self.batch / "raw/complete.json").write_text('{"blocks":4}')

    def test_iterations_not_statistical_samples_and_no_warmup(self):
        blocks, excluded = measured_blocks(self.batch)
        self.assertEqual(excluded, 1)
        self.assertEqual([b[1, "local"]["end_to_end_ns"] for b in blocks], [2, 3, 4])

    def test_missing_block_not_replaced_by_warmup(self):
        (self.batch / "raw/block-0003/block.json").unlink()
        with self.assertRaises(FileNotFoundError):
            measured_blocks(self.batch)

    def test_duplicate_arm_rejected(self):
        path = self.batch / "raw/block-0004/block.json"
        rows = json.loads(path.read_text())
        rows[-1] = rows[-2]
        path.write_text(json.dumps(rows))
        with self.assertRaisesRegex(ValueError, "incomplete paired block"):
            measured_blocks(self.batch)
