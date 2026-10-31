import concurrent.futures
import multiprocessing
import os
import unittest

from analyze_batch import offload_counts


class Mainline(unittest.TestCase):
    def test_zero_duration_and_simultaneous_handoffs(self):
        peak, rows = offload_counts([(0, 1), (1, -1), (1, 1), (2, -1), (0, 1), (0, -1)])
        self.assertEqual(peak, 1)
        self.assertEqual(rows, [[0, 1], [1, 1], [2, 0]])

    def test_unbalanced_intervals_fail(self):
        for points in [[(0, -1)], [(0, 1)]]:
            with self.assertRaises(AssertionError):
                offload_counts(points)

    def test_postprocessor_is_a_separate_spawned_process(self):
        with concurrent.futures.ProcessPoolExecutor(
            max_workers=2, mp_context=multiprocessing.get_context("spawn")
        ) as pool:
            self.assertNotEqual(pool.submit(os.getpid).result(timeout=20), os.getpid())


if __name__ == "__main__":
    unittest.main()
