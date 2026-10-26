import unittest

import admission as a

G = a.GIB


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        self.p = dict(
            condition_bytes=31.75 * G,
            headroom_bytes=8 * G,
            boot_bytes={"x": 2 * G},
            startup_reservation_s=30,
            max_snapshot_age_s=3,
            memory_psi_avg10_max=1,
            max_queue_s=120,
        )
        self.s = dict(
            elapsed_s=10,
            total=dict(
                memory_current=15 * G,
                memory_stat=dict(
                    file=8 * G,
                    inactive_file=6 * G,
                    file_mapped=G,
                    file_dirty=G,
                    file_writeback=0,
                    shmem=0,
                ),
                pressure={"memory": "some avg10=0.00 avg60=0 total=0\n"},
            ),
        )

    def test_same_observations_same_decision(self):
        self.assertEqual(
            a.decision(self.s, [], 10, "x", self.p), a.decision(self.s, [], 10, "x", self.p)
        )

    def test_safe_cache_credit(self):
        self.assertEqual(a.clean_credit(self.s["total"]["memory_stat"]), 2 * G)

    def test_no_credit_for_mapped_dirty(self):
        self.s["total"]["memory_stat"]["file_mapped"] = 9 * G
        self.assertEqual(a.clean_credit(self.s["total"]["memory_stat"]), 0)

    def test_memory_boundary_blocks_new_launch(self):
        self.s["total"]["memory_current"] = 30 * G
        self.assertIn("memory_budget", a.decision(self.s, [], 10, "x", self.p)["reasons"])

    def test_stale_observation_blocks(self):
        self.assertIn("stale_sample", a.decision(self.s, [], 14, "x", self.p)["reasons"])

    def test_pressure_blocks(self):
        self.s["total"]["pressure"]["memory"] = "some avg10=1.00 total=1"
        self.assertIn("memory_pressure", a.decision(self.s, [], 10, "x", self.p)["reasons"])

    def test_startup_pending_is_not_free(self):
        e = dict(task="x", launch_s=9, observed_vm_bytes=G, scope_seen=True)
        self.assertEqual(a.startup_reservations([e], 10, self.p), G)

    def test_startup_missing_scope_keeps_reservation(self):
        e = dict(task="x", launch_s=0)
        self.assertEqual(a.startup_reservations([e], 60, self.p), 2 * G)

    def test_startup_observed_after_window_releases(self):
        e = dict(task="x", launch_s=0, scope_seen=True, observed_vm_bytes=G)
        self.assertEqual(a.startup_reservations([e], 30, self.p), 0)

    def test_real_memory_release_can_enable_admission(self):
        self.s["total"]["memory_current"] = 26 * G
        self.assertFalse(a.decision(self.s, [], 10, "x", self.p)["allowed"])
        self.s["total"]["memory_current"] = 20 * G
        self.assertTrue(a.decision(self.s, [], 10, "x", self.p)["allowed"])

    def test_queue_timeout_boundary(self):
        self.assertFalse(a.expired({"at_s": 5}, 125, self.p))
        self.assertTrue(a.expired({"at_s": 5}, 125.1, self.p))

    def test_missing_memory_stat_fails_closed(self):
        del self.s["total"]["memory_stat"]["inactive_file"]
        with self.assertRaises(KeyError):
            a.decision(self.s, [], 10, "x", self.p)


if __name__ == "__main__":
    unittest.main()
