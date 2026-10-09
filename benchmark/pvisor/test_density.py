"""Density evidence rejects incomplete useful work and wrong memory fixtures."""

import unittest

from density import verify_result


def result(useful=True):
    ready = dict(
        token="unique-live-process",
        bytes=32 * 1024 * 1024 if useful else 0,
        checksum="full-payload",
    )
    done = ready | dict(changes=4 if useful else 0, integrity="passed")
    return ready, done


class DensityTests(unittest.TestCase):
    def test_complete_matching_density_task_is_valid(self):
        for useful in [True, False]:
            with self.subTest(useful=useful):
                verify_result(*result(useful), useful)

    def test_partial_or_wrong_task_cannot_count_as_density_completion(self):
        for field, value in [
            ("token", "other-process"),
            ("checksum", "changed"),
            ("changes", 3),
            ("integrity", "failed"),
        ]:
            with self.subTest(field=field, value=value):
                ready, done = result()
                done[field] = value
                with self.assertRaises(ValueError):
                    verify_result(ready, done, True)

    def test_matching_empty_checksums_cannot_replace_useful_private_memory(self):
        ready, done = result()
        ready["bytes"] = done["bytes"] = 0
        with self.assertRaisesRegex(ValueError, "requested private payload"):
            verify_result(ready, done, True)
