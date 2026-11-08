"""Contracts for phase-memory observation and the actual guest frame transport."""

import ctypes
import os
import socket
import struct
import unittest

import allocator_reclaim as experiment


class ObservationTests(unittest.TestCase):
    @unittest.skipUnless(experiment.platform.system() == "Darwin", "requires macOS libproc")
    def test_native_rusage_layout_and_positive_current_process(self):
        self.assertEqual(ctypes.sizeof(experiment.Usage), 96)
        self.assertEqual(experiment.Usage.phys_footprint.offset, 72)
        value = experiment.usage(os.getpid())
        self.assertGreater(value["phys_footprint"], 0)
        self.assertGreater(value["resident_size"], 0)
        self.assertGreater(value["proc_start_abstime"], 0)

    def test_frame_transport_rejects_failure_and_incomplete_messages(self):
        for packet, error in [
            (dict(kind="Failure", message="retired"), RuntimeError),
            (dict(kind="Ready"), None),
        ]:
            left, right = socket.socketpair()
            with left, right:
                experiment.send(left, packet)
                if error:
                    with self.assertRaises(error):
                        experiment.receive(right)
                else:
                    self.assertEqual(experiment.receive(right), packet)
        left, right = socket.socketpair()
        with left, right:
            left.sendall(struct.pack("!I", 20) + b"{}")
            left.shutdown(socket.SHUT_WR)
            with self.assertRaises(EOFError):
                experiment.receive(right)
        left, right = socket.socketpair()
        with left, right:
            left.sendall(struct.pack("!I", 1048577))
            with self.assertRaises(ValueError):
                experiment.receive(right)

    def test_guest_counters_keep_bytes_separate_from_process_counts(self):
        value = experiment.counters("RssAnon:\t65536 kB\nThreads:\t6\nName:\tpayload\n")
        self.assertEqual(value, {"RssAnon": 67108864, "Threads": 6})

    def test_phase_summary_requires_a_complete_stable_window(self):
        with self.assertRaises(ValueError):
            experiment.phase_summary([dict(time=0, resident_size=1)])
        rows = [
            dict(time=i / 4, resident_size=i, phys_footprint=i * 2, wired_size=0) for i in range(20)
        ]
        self.assertEqual(experiment.phase_summary(rows)["resident_size"], 15)


if __name__ == "__main__":
    unittest.main()
