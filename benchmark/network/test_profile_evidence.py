"""Read-only contracts for retained network engineering evidence."""

import copy
import json
import unittest
from unittest.mock import Mock

import audit_profile
import profile_evidence as bench
import tomllib


def fixture(arm="release", elapsed=100_000_000):
    records = []
    for name in bench.SCENARIOS:
        if arm == "native" and name == "dns":
            continue
        record = dict(
            scenario=name,
            elapsed_ns=elapsed,
            bytes=0,
            operations=128,
            samples_ns=[100_000] * 128,
            first_byte_ns=0,
        )
        if name.startswith(("upload_", "download_")):
            record.update(
                bytes=64 * 1024 * 1024, operations=int(name.rsplit("_", 1)[-1]), samples_ns=[]
            )
        elif name == "rpc_fresh":
            record["bytes"] = 128 * 2048
        elif name == "rpc_keepalive":
            record.update(bytes=256 * 2048, operations=256, samples_ns=[100_000] * 256)
        elif name == "stream":
            record.update(
                bytes=2048, operations=32, samples_ns=[2_000_000] * 31, first_byte_ns=1_000_000
            )
        records.append(record)
    kind = "native" if arm == "native" else "vm"
    return (
        'NET_READY {"kind":"'
        + kind
        + '"}\n'
        + "\n".join("NET_RESULT " + json.dumps(record) for record in records)
        + "\nNET_COMPLETE\n"
    )


class NetworkProfileContracts(unittest.TestCase):
    def test_release_optimizes_network_without_changing_global_size_policy(self):
        release = tomllib.loads((bench.REPO / "Cargo.toml").read_text())["profile"]["release"]
        self.assertEqual(release["opt-level"], "z")
        for package in ("pvisor-overlaynet", "smoltcp"):
            self.assertEqual(release["package"][package]["opt-level"], 3)

    def test_complete_scenarios_and_native_dns_exclusion(self):
        self.assertEqual(len(bench.parse_results(fixture(), "release")), len(bench.SCENARIOS))
        self.assertEqual(
            len(bench.parse_results(fixture("native"), "native")), len(bench.SCENARIOS) - 1
        )

    def test_truncation_duplicate_and_wrong_counts_fail(self):
        for text in (
            fixture().replace("NET_COMPLETE", ""),
            fixture() + fixture(),
            fixture().replace('"bytes": 67108864', '"bytes": 67108863'),
        ):
            with self.assertRaises(ValueError):
                bench.parse_results(text, "release")

    def test_bulk_is_total_wall_goodput_not_mean_connection_speed(self):
        record = dict(scenario="download_32", bytes=64 * 1024 * 1024, elapsed_ns=500_000_000)
        self.assertEqual(bench.metric(record), (128, "MiB/s"))

    def test_paired_ci_keeps_pairing_and_null_result(self):
        result = bench.paired_ci([100, 200, 300], [110, 210, 310])
        self.assertEqual(result["median_delta"], 10)
        self.assertEqual(result["ci95"], [10, 10])
        result = bench.paired_ci([1, 1, 1], [1, 1, 1])
        self.assertEqual(result["ci95"], [0, 0])

    def test_cargo_audit_preserves_host_and_target_instances_regardless_of_order(self):
        records = [
            dict(
                reason="compiler-artifact",
                package_id="libc@0.2.186",
                target=dict(name="libc", kind=["lib"]),
                profile=dict(opt_level=level),
                features=features,
            )
            for level, features in (
                ("0", ["default", "std"]),
                ("z", ["default", "std"]),
                ("z", ["default", "extra_traits", "std"]),
            )
        ]
        left, right = Mock(), Mock()
        left.read_text.return_value = "\n".join(map(json.dumps, records))
        right.read_text.return_value = "\n".join(map(json.dumps, reversed(records)))
        profiles = audit_profile.profiles(left)
        self.assertEqual(profiles, audit_profile.profiles(right))
        self.assertEqual(sum(map(len, profiles.values())), 3)
        self.assertEqual(len(profiles), 2)

    def test_clusters_preserve_all_points_including_slow_group(self):
        groups = bench.clusters([1] * 15 + [3] * 15)
        self.assertEqual([len(group) for group in groups], [15, 15])
        self.assertEqual(sum(map(len, groups)), 30)

    def test_bundle_requires_real_vm_and_exact_byte_accounting(self):
        bundle = dict(
            run=dict(
                executor=dict(kind="virtual_machine"),
                state="completed",
                exit_code=0,
                metrics={"resource.vm_memory_bytes": 256 * 1024 * 1024},
            ),
            safety=dict(network_non_bypassable=True),
            network=dict(
                interception=dict(driver="vm-smoltcp", strength="non-bypassable"),
                policy=dict(
                    mode="allowlist",
                    limits=[],
                    rules=[
                        dict(host="localhost.localdomain", ports=[12345], allow_private_ips=True)
                    ],
                ),
                intercepted=dict(
                    policy_allowed=340,
                    policy_denied=0,
                    failures=0,
                    dns_queries=129,
                    dns_answers=129,
                    tcp_flows_opened=340,
                    tcp_flows_denied=0,
                    tcp_connect_failures=0,
                    bytes_guest_to_host=201723200,
                    bytes_host_to_guest=201722184,
                    unsupported_packets=0,
                    active_tcp_flows=0,
                ),
            ),
        )
        bench.validate_bundle(bundle, 12345)
        broken = copy.deepcopy(bundle)
        broken["network"]["intercepted"]["bytes_host_to_guest"] -= 1
        with self.assertRaises(ValueError):
            bench.validate_bundle(broken, 12345)
        broken = copy.deepcopy(bundle)
        broken["run"]["executor"]["kind"] = "process"
        with self.assertRaises(ValueError):
            bench.validate_bundle(broken, 12345)

    def test_missing_or_duplicate_round_cannot_produce_report(self):
        trials = [
            dict(
                arm=arm,
                phase="formal",
                round=index,
                results=bench.parse_results(fixture(arm), arm),
            )
            for index in range(3)
            for arm in bench.ARMS
        ]
        self.assertEqual(len(bench.summarize(trials, 3)["rows"]), len(bench.SCENARIOS) + 2)
        with self.assertRaises(ValueError):
            bench.summarize(trials[:-1], 3)
        duplicate = copy.deepcopy(trials)
        duplicate[-1]["round"] = 0
        with self.assertRaises(ValueError):
            bench.summarize(duplicate, 3)


if __name__ == "__main__":
    unittest.main()
