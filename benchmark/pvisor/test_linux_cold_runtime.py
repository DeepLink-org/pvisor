import argparse
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import linux_cold_runtime as harness


class ColdRuntimeTests(unittest.TestCase):
    def config(self):
        return dict(
            example="/worker",
            rootfs="/rootfs",
            firmware="/firmware",
            output="/out",
            pattern="random-unique",
            cold=True,
            seed=1,
            wait1=20,
            wait2=35,
        )

    def test_worker_command(self):
        command = harness.worker_command(self.config())
        self.assertEqual(command[command.index("--cold") + 1], "true")
        self.assertNotIn("--ram-backing", command)
        self.assertNotIn("--vms", command)

    def test_conditions_rejected(self):
        for change in ({"cold": 1}, {"pattern": "random-shared"}):
            with self.assertRaises(ValueError):
                harness.worker_command(self.config() | change)

    def test_limits(self):
        args = argparse.Namespace(seed=1, wait1=20, wait2=35, output=Path("/short"))
        harness.validate_limits(args)
        for name, value in [
            ("seed", -1),
            ("seed", 2**64),
            ("wait1", 19),
            ("wait2", 34),
            ("output", Path("/" + "x" * 70)),
        ]:
            with self.assertRaises(ValueError):
                harness.validate_limits(argparse.Namespace(**(vars(args) | {name: value})))

    def test_unit_bounds_entire_group(self):
        command = harness.unit_command("owned.service", "/harness", "/config")
        for value in (
            "MemoryMax=2147483648",
            "MemorySwapMax=0",
            "CPUQuota=400%",
            "KillMode=control-group",
            "RuntimeMaxSec=200",
        ):
            self.assertIn("--property=" + value, command)

    def metric(self, discarded=4096, restored=4096, cold=0):
        return (
            "pvisor-cold-linux cold_bytes_current=%d discarded_bytes_total=%d "
            "restored_bytes_total=%d put_rejections_total=1 pool_encoded_bytes_current=0 "
            "pool_objects_current=0 pid=42" % (cold, discarded, restored)
        )

    def test_metric_gauges_not_summed(self):
        rows = harness.parse_metrics(self.metric(cold=4096) + "\n" + self.metric(cold=0))
        self.assertEqual(rows[-1]["cold_bytes_current"], 0)
        self.assertEqual(rows[-1]["discarded_bytes_total"], 4096)

    def test_counter_regression_rejected(self):
        with self.assertRaises(ValueError):
            harness.parse_metrics(self.metric(discarded=8192) + "\n" + self.metric())

    def test_incomplete_metrics_rejected(self):
        with self.assertRaises(ValueError):
            harness.parse_metrics("pvisor-cold-linux pid=42")

    def test_ram_vma_selection(self):
        raw = (
            "10000000-20000000 rw-p 00000000 00:00 0\n"
            "Size: 262144 kB\nPss: 65536 kB\nRss: 65536 kB\nAnonymous: 65536 kB\n"
            "30000000-40000000 rw-p 00000000 00:01 5 /not-ram\nSize: 262144 kB\n"
        )
        rows = harness.ram_vmas({"processes": [{"pid": 42, "smaps": {"raw": raw}}]})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["stats"]["Pss"], 64 * 1024**2)

    def test_off_vma_requires_runner_and_matching_fd(self):
        raw = (
            "10000000-20000000 rw-s 00000000 00:23 51 /ram\n"
            "Size: 262144 kB\nPss: 65536 kB\nRss: 65536 kB\n"
        )
        process = dict(
            pid=42,
            cmdline={"raw": "/worker\0"},
            smaps={"raw": raw},
            large_file_fds=[dict(inode=51, target="/ram")],
        )
        self.assertEqual(len(harness.ram_vmas({"processes": [process]}, "/worker", False)), 1)
        self.assertEqual(harness.ram_vmas({"processes": [process]}, "/wrong", False), [])
        process["large_file_fds"][0]["inode"] = 52
        self.assertEqual(harness.ram_vmas({"processes": [process]}, "/worker", False), [])

    def test_full_off_report_and_rejection_gates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "guest-1.stderr").write_text("")
            (root / "worker.stderr").write_text("")
            names = ["ready", "cold1", "restore1", "mutation", "cold2", "restore2"]
            times = [1000, 22000, 23000, 24000, 60000, 61000]
            counters = {
                key: {"raw": value}
                for key, value in {
                    "memory.max": "2147483648",
                    "memory.swap.max": "0",
                    "cpu.max": "400000 100000",
                    "memory.events": "oom 0\noom_kill 0\nmax 0",
                    "memory.current": "200000000",
                    "memory.peak": "250000000",
                    "memory.stat": "anon 100\nfile 200\nkernel 300",
                    "cpu.stat": "usage_usec 1234",
                    "memory.pressure": "some avg10=0.00 total=0",
                    "cpu.pressure": "some avg10=0.00 total=0",
                }.items()
            }
            process = dict(
                pid=42,
                cmdline={"raw": "/worker\0"},
                smaps={
                    "raw": "10000000-20000000 rw-s 00000000 00:23 51 /ram\nSize: 262144 kB\nPss: 65536 kB\n"
                },
                large_file_fds=[dict(inode=51, target="/ram")],
            )
            raw = dict(
                schema="pvisor-cold-runtime/v1",
                correctness="passed",
                cleanup={"all_reaped": True},
                conditions=dict(cold=False, pattern="repeated"),
                profile=dict(
                    memory_mib=256,
                    cpus=1,
                    payload_bytes=64 * 1024**2,
                    max_live_vms=1,
                    overlaynet_mode="off",
                    network_policy="no-network",
                ),
                guests=[{"result": {"state": "completed", "exit_code": 0}}],
                source={"binary_sha256": "binary"},
                phases=[
                    dict(
                        name=name,
                        elapsed_ms=elapsed,
                        heartbeat=[i + 1],
                        heartbeat_stable=True,
                        accounting=dict(
                            cgroup="/group",
                            counters=counters,
                            processes=[process],
                            process_errors=[],
                        ),
                    )
                    for i, (name, elapsed) in enumerate(zip(names, times))
                ],
                expected_digests=[
                    dict(percent=0, digest="base"),
                    dict(percent=100, digest="mutated"),
                ],
                checks=[
                    dict(
                        name=name,
                        passed=True,
                        evidence=dict(percent=percent, digest=digest, device_io_bytes=64 * 1024**2),
                    )
                    for name, percent, digest in [
                        ("ready", 0, "base"),
                        ("restore1", 0, "base"),
                        ("mutation", 100, "mutated"),
                        ("restore2", 100, "mutated"),
                        ("exit", 100, "mutated"),
                    ]
                ],
            )
            events = [
                dict(time_ns=i + 1, stream="stdout", line=json.dumps({"phase": name}))
                for i, name in enumerate(names)
            ]
            (root / "stream-events.jsonl").write_text(
                "".join(json.dumps(row) + "\n" for row in events)
            )
            config = self.config() | dict(
                cold=False,
                pattern="repeated",
                output=str(root),
                stderr=str(root / "worker.stderr"),
                cgroup="/group",
                example_sha256="binary",
            )
            self.assertEqual(len(harness.validate_report(raw, config)["phases"]), 6)
            raw["checks"][1]["evidence"]["digest"] = "corrupt"
            with self.assertRaisesRegex(ValueError, "oracle mismatch"):
                harness.validate_report(raw, config)
            raw["checks"][1]["evidence"]["digest"] = "base"
            raw["phases"][1]["elapsed_ms"] = 1001
            with self.assertRaisesRegex(ValueError, "cold window"):
                harness.validate_report(raw, config)

    def layout_text(self):
        return (
            "pvisor-cold-linux-layout host_page_bytes=4096 block_bytes=65536 "
            "eligible_mappings=1 eligible_bytes_total=8192 kernel_excluded=true "
            "pss_attribution=requires_exact_vma_union pid=42\n"
            "pvisor-cold-linux-region host_start=0x10000000 length=8192 guest_start=0x0 pid=42\n"
            "pvisor-cold-linux-kernel-excluded host_start=0x50000000 length=4096 "
            "guest_start=0x8000 reason=trusted_raw_firmware pid=42\n"
        )

    def accounting(self, smaps):
        return {"processes": [dict(pid=42, cmdline={"raw": "/worker\0"}, smaps={"raw": smaps})]}

    def test_layout_trusted_geometry(self):
        inventory = harness.parse_cold_layout(self.layout_text())
        self.assertEqual(inventory["eligible_union"], [(0x10000000, 0x10002000)])
        self.assertEqual(inventory["layout"]["eligible_bytes_total"], 8192)

    def test_layout_missing_ambiguous_and_malformed(self):
        text = self.layout_text()
        bad = [
            "",
            text + text,
            text.replace("length=8192", "length=-1"),
            text.replace("host_start=0x10000000", "host_start=0x10000001"),
            text.replace("eligible_mappings=1", "eligible_mappings=2"),
            text.replace("eligible_bytes_total=8192", "eligible_bytes_total=4096"),
            text.replace("guest_start=0x0 pid=42", "guest_start=0x0 pid=43"),
            text.replace("kernel_excluded=true", "kernel_excluded=false"),
            text.replace("pid=42\n", "pid=42 pid=42\n"),
            text.replace("length=8192", "length=18446744073709551616"),
            text.replace("host_start=0x10000000", "host_start=0xfffffffffffff000"),
        ]
        for value in bad:
            with self.subTest(value=value), self.assertRaises(ValueError):
                harness.parse_cold_layout(value)

    def test_layout_overlapping_host_or_guest_intervals_rejected(self):
        text = (
            self.layout_text()
            .replace("eligible_mappings=1", "eligible_mappings=2")
            .replace("eligible_bytes_total=8192", "eligible_bytes_total=12288")
        )
        for host, guest in [("0x10001000", "0x10000"), ("0x30000000", "0x1000")]:
            duplicate = f"pvisor-cold-linux-region host_start={host} length=4096 guest_start={guest} pid=42\n"
            with self.assertRaisesRegex(ValueError, "overlapping"):
                harness.parse_cold_layout(text + duplicate)

    def test_layout_kernel_overlap_rejected(self):
        with self.assertRaisesRegex(ValueError, "overlapping"):
            harness.parse_cold_layout(
                self.layout_text().replace("host_start=0x50000000", "host_start=0x10001000")
            )

    def test_exact_vma_union_pss(self):
        inventory = harness.parse_cold_layout(self.layout_text())
        smaps = (
            "10000000-10001000 rw-p 00000000 00:00 0\nSize: 4 kB\nPss: 2 kB\n"
            "10001000-10002000 rw-p 00000000 00:00 0\nSize: 4 kB\nPss: 3 kB\n"
        )
        result = harness.cold_ram_pss(self.accounting(smaps), "/worker", inventory)
        self.assertTrue(result["isolated_ram_pss_available"])
        self.assertEqual(result["isolated_ram_pss_bytes"], 5 * 1024)
        self.assertIsNone(result["ram_envelope_pss_bytes"])

    def test_coalesced_envelope_not_isolated_ram(self):
        inventory = harness.parse_cold_layout(self.layout_text())
        smaps = "10000000-10003000 rw-p 00000000 00:00 0\nSize: 12 kB\nPss: 10 kB\n"
        result = harness.cold_ram_pss(self.accounting(smaps), "/worker", inventory)
        self.assertFalse(result["isolated_ram_pss_available"])
        self.assertIsNone(result["isolated_ram_pss_bytes"])
        self.assertEqual(result["isolated_ram_vmas"], [])
        self.assertEqual(result["ram_envelope_pss_bytes"], 10 * 1024)

    def test_adjacent_inventory_intervals_can_match_one_vma(self):
        text = (
            self.layout_text()
            .replace("eligible_mappings=1", "eligible_mappings=2")
            .replace("length=8192 guest_start=0x0", "length=4096 guest_start=0x0")
        )
        text += (
            "pvisor-cold-linux-region host_start=0x10001000 length=4096 guest_start=0x1000 pid=42\n"
        )
        inventory = harness.parse_cold_layout(text)
        smaps = "10000000-10002000 rw-p 00000000 00:00 0\nSize: 8 kB\nPss: 8 kB\n"
        self.assertTrue(
            harness.cold_ram_pss(self.accounting(smaps), "/worker", inventory)[
                "isolated_ram_pss_available"
            ]
        )

    def test_missing_smaps_coverage_and_wrong_runner_rejected(self):
        inventory = harness.parse_cold_layout(self.layout_text())
        smaps = "10000000-10001000 rw-p 00000000 00:00 0\nSize: 4 kB\nPss: 4 kB\n"
        with self.assertRaisesRegex(ValueError, "fully covered"):
            harness.cold_ram_pss(self.accounting(smaps), "/worker", inventory)
        with self.assertRaisesRegex(ValueError, "runner identity"):
            harness.cold_ram_pss(self.accounting(smaps), "/wrong", inventory)

    def test_malformed_smaps_rejected(self):
        inventory = harness.parse_cold_layout(self.layout_text())
        smaps = "10000000-10002000 rw-p 00000000 00:00 0\nSize: 8 kB\nPss: 8 kB\n"
        for value in [
            smaps.replace("Size: 8", "Size: 4"),
            smaps.replace("Pss: 8", "Pss: bad"),
            smaps + smaps,
            smaps.replace("rw-p", "rw-s"),
            smaps.replace("Pss: 8", "Pss: 12"),
        ]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                harness.cold_ram_pss(self.accounting(value), "/worker", inventory)

    def test_quiet_wait_resets_after_interference(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(harness, "current_group", return_value="/group"),
            patch.object(
                harness,
                "competing_jobs",
                side_effect=[
                    {"jobs": []},
                    {"jobs": ["build"]},
                    {"jobs": []},
                    {"jobs": []},
                    {"jobs": []},
                ],
            ),
            patch.object(harness.time, "monotonic", side_effect=[0, 1, 29, 30, 59, 60]),
            patch.object(harness.time, "sleep"),
        ):
            self.assertTrue(harness.wait_for_quiet(Path(directory)))

    def test_quiet_wait_bounded_deadline(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(harness, "current_group", return_value="/group"),
            patch.object(harness, "competing_jobs", return_value={"jobs": ["build"]}),
            patch.object(harness.time, "monotonic", side_effect=[0, 1, 181]),
            patch.object(harness.time, "sleep"),
        ):
            self.assertFalse(harness.wait_for_quiet(Path(directory)))


if __name__ == "__main__":
    unittest.main()
