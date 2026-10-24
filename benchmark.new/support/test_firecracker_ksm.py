"""Conventional tests; no VM, KSM writes or administrator approval required."""

import importlib.util
import struct
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "firecracker_ksm", Path(__file__).with_name("firecracker_ksm.py")
)


f = importlib.util.module_from_spec(spec)


spec.loader.exec_module(f)


class PatternTests(unittest.TestCase):
    def test_repeated(self):
        self.assertEqual(f.page("repeated", 1, 0, 0), bytes(range(256)) * 16)
        self.assertEqual(f.page("repeated", 1, 100, 0), f.page("repeated", 4, 0, 0))

    def test_random_shared_and_unique(self):
        self.assertEqual(f.page("random-shared", 1, 123, 0), f.page("random-shared", 4, 123, 0))
        self.assertNotEqual(f.page("random-unique", 1, 123, 0), f.page("random-unique", 4, 123, 0))
        self.assertNotEqual(f.page("random-shared", 1, 123, 0), f.page("random-shared", 1, 124, 0))

    def test_exact_lcg_first_word(self):
        x = f.SEED ^ ((3 * 0xD1B54A32D192ED03) & f.MASK) ^ ((123 * 0x9E3779B97F4A7C15) & f.MASK)
        x = (x * 6364136223846793005 + 1442695040888963407) & f.MASK
        self.assertEqual(f.page("random-unique", 3, 123, 0)[:8], struct.pack("<Q", x))

    def test_mutation_boundary_and_peer_isolation(self):
        boundary = (f.SIZE // f.PAGE) // 4
        self.assertNotEqual(
            f.page("repeated", 1, boundary - 1, 25), f.page("repeated", 1, boundary - 1, 0)
        )
        self.assertEqual(f.page("repeated", 1, boundary, 25), f.page("repeated", 1, boundary, 0))
        for pattern in f.PATTERNS:
            self.assertNotEqual(f.page(pattern, 1, 0, 25), f.page(pattern, 2, 0, 25))
            self.assertEqual(f.page(pattern, 1, 0, 25), f.page(pattern, 1, 0, 100))


class EvidenceTests(unittest.TestCase):
    def test_worker_checks_every_byte_and_sha(self):
        self.assertIn("full-byte mismatch page", f.WORKER)
        self.assertIn("sha(data)", f.WORKER)
        self.assertIn("assert_eq!(requested,percent)", f.WORKER)
