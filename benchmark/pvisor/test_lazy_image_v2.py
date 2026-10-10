"""Conventional B-LAZY-ENG tests; no VM, build, namespace or benchmark launch."""

import contextlib
import hashlib
import json
import os
import signal
import socket
import socketserver
import struct
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lazy_image_v2 as harness


def encode(value):
    # Deliberate noncanonical whitespace: forwarding must preserve original bytes.
    raw = json.dumps(value, indent=1).encode()
    return struct.pack("!I", len(raw)) + raw


def envelope(version, op="ping", **fields):
    return dict(version=version, token="test-token", request=dict(op=op, **fields))


@contextlib.contextmanager
def proxy_with(replies):
    """A framed fake service, not a real cache/benchmark workload."""
    seen = []
    failures = []

    class Upstream(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(2)
            try:
                for wire in replies:
                    request = harness.frame(self.request, clean_eof=True)
                    if request is None:
                        return
                    seen.append(request[1])
                    # Split writes exercise exact-length reads, including raw bodies.
                    for offset in range(0, len(wire), 7):
                        self.request.sendall(wire[offset : offset + 7])
                    if request[1]["version"] == 1:
                        return
            except Exception as error:
                failures.append(error)

    upstream = harness.TCPServer(("127.0.0.1", 0), Upstream)
    proxy = harness.TCPServer(("127.0.0.1", 0), harness.CacheProxy)
    proxy.upstream = upstream.server_address[1]
    proxy.counts = harness.Counts()
    for server in (upstream, proxy):
        threading.Thread(target=server.serve_forever, daemon=True).start()
    client = socket.create_connection(proxy.server_address, timeout=2)
    try:
        yield client, proxy.counts, seen, failures
    finally:
        client.close()
        for server in (proxy, upstream):
            server.shutdown()
            server.server_close()


def wait_rows(counts, number):
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        snapshot = counts.snapshot()
        if len(snapshot["rows"]) >= number or snapshot["errors"]:
            return snapshot
        time.sleep(0.005)
    raise AssertionError("proxy did not finish bounded test exchange")


def cohort(samples=3, warmups=0, variants=harness.VARIANTS):
    return [
        dict(
            trial=trial,
            variant=variant,
            cache=cache,
            ready_ms=100 + trial + (10 if variant == variants[1] else 0),
            completion_ms=200 + trial,
            content_bytes=100 if cache == "cold" else 0,
            metadata_bytes=20 if variant == variants[0] else 40,
            response_bytes=300,
            requests=10,
            connections=2,
            correctness="passed",
            bundle_validated=True,
            proxy_errors=[],
        )
        for trial in range(-warmups - 1, samples)
        for variant in variants
        for cache in ("cold", "warm")
    ]


class ProxyTests(unittest.TestCase):
    def test_v1_closes_after_one_frame_and_preserves_json(self):
        reply = encode(dict(status="ready"))
        with proxy_with([reply]) as (client, counts, seen, failures):
            client.sendall(encode(envelope(1)))
            self.assertEqual(harness.exact(client, len(reply)), reply)
            self.assertEqual(client.recv(1), b"")
            snapshot = wait_rows(counts, 1)
            self.assertEqual(snapshot["connections"], 1)
            self.assertEqual(snapshot["errors"], [])
            self.assertEqual(snapshot["rows"][0]["version"], 1)
            self.assertEqual(snapshot["rows"][0]["op"], "ping")
            self.assertEqual(snapshot["rows"][0]["response_bytes"], len(reply))
            self.assertEqual(len(seen), 1)
            self.assertEqual(failures, [])

    def test_v2_multiple_frames_raw_body_and_application_error(self):
        body = b'\x00\x00\x00\x11{"status":"ready"}\x00\xffraw'
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        replies = [
            encode(dict(status="ready")),
            encode(dict(status="data", length=len(body), sha256=digest)) + body,
            encode(dict(status="error", code="not_found", message="missing")),
            encode(dict(status="ready")),
        ]
        requests = [
            envelope(2),
            envelope(2, "read", path=[102, 111, 111], length=100, offset=0),
            envelope(2, "stat", path=[120]),
            envelope(2),
        ]
        with proxy_with(replies) as (client, counts, seen, failures):
            for request, reply in zip(requests, replies):
                client.sendall(encode(request))
                self.assertEqual(harness.exact(client, len(reply)), reply)
                # Snapshot must return while the persistent client is still open.
                snapshot = wait_rows(counts, len(seen))
                self.assertEqual(snapshot["errors"], [])
            snapshot = wait_rows(counts, 4)
            self.assertEqual(snapshot["connections"], 1)
            self.assertEqual([r["op"] for r in snapshot["rows"]], ["ping", "read", "stat", "ping"])
            self.assertEqual({r["connection_id"] for r in snapshot["rows"]}, {1})
            row = snapshot["rows"][1]
            self.assertEqual(row["path"], [102, 111, 111])
            self.assertEqual(row["content_bytes"], len(body))
            self.assertEqual(row["server_body_sha256"], digest)
            self.assertEqual(row["forwarded_body_sha256"], digest)
            self.assertGreaterEqual(row["elapsed_ms"], 0)
            self.assertEqual(snapshot["rows"][2]["content_bytes"], 0)
            self.assertEqual(seen, requests)
            self.assertEqual(failures, [])

    def test_file_and_metadata_data_accounting_is_separate(self):
        body = b"\x00\xffindex-or-file-payload"
        data = (
            encode(
                dict(
                    status="data",
                    length=len(body),
                    sha256="sha256:" + hashlib.sha256(body).hexdigest(),
                )
            )
            + body
        )
        ready = encode(dict(status="ready"))
        error = encode(dict(status="error", code="not_found", message="missing"))
        exchanges = [
            ("read", data, len(body), 0),
            ("metadata", data, 0, len(body)),
            ("stat", data, 0, 0),
            ("read", ready, 0, 0),
            ("metadata", error, 0, 0),
        ]
        with proxy_with([reply for _, reply, _, _ in exchanges]) as (client, counts, _, failures):
            for number, (op, reply, content, metadata) in enumerate(exchanges, 1):
                client.sendall(encode(envelope(2, op)))
                self.assertEqual(harness.exact(client, len(reply)), reply)
                row = wait_rows(counts, number)["rows"][-1]
                self.assertEqual(row["content_bytes"], content)
                self.assertEqual(row["metadata_bytes"], metadata)
                self.assertEqual(row["response_bytes"], len(reply))
            snapshot = counts.snapshot()
            self.assertEqual(snapshot["errors"], [])
            self.assertEqual(sum(r["content_bytes"] for r in snapshot["rows"]), len(body))
            self.assertEqual(sum(r["metadata_bytes"] for r in snapshot["rows"]), len(body))
            self.assertEqual(
                sum(r["response_bytes"] for r in snapshot["rows"]),
                sum(len(reply) for _, reply, _, _ in exchanges),
            )
            self.assertEqual(failures, [])

    def test_partial_response_is_rejected_and_recorded(self):
        cases = [
            b"\x00\x00",
            struct.pack("!I", 20) + b"{",
            encode(dict(status="data", length=8, sha256="sha256:unused")) + b"abc",
        ]
        for reply in cases:
            with self.subTest(reply=reply), proxy_with([reply]) as (client, counts, _, _):
                client.sendall(encode(envelope(2, "read", path=[], length=8, offset=0)))
                self.assertEqual(client.recv(1), b"")
                snapshot = wait_rows(counts, 1)
                self.assertTrue(snapshot["errors"])
                self.assertEqual(snapshot["rows"], [])

    def test_hash_mismatch_rejects_complete_body(self):
        reply = encode(dict(status="data", length=3, sha256="sha256:wrong")) + b"abc"
        with proxy_with([reply]) as (client, counts, _, _):
            client.sendall(encode(envelope(2, "read", path=[], length=3, offset=0)))
            self.assertEqual(client.recv(1), b"")
            self.assertIn("hash mismatch", wait_rows(counts, 1)["errors"][0]["error"])

    def test_active_requests_not_idle_connections_control_drain(self):
        counts = harness.Counts()
        counts.connections = 4
        self.assertEqual(counts.snapshot(timeout=0.01)["connections"], 4)
        counts.active = 1
        with self.assertRaises(TimeoutError):
            counts.snapshot(timeout=0.01)

    def test_frame_limits_and_partial_request(self):
        left, right = socket.socketpair()
        try:
            right.sendall(struct.pack("!I", harness.MAX_FRAME + 1))
            with self.assertRaises(ValueError):
                harness.frame(left)
        finally:
            left.close()
            right.close()
        with proxy_with([encode(dict(status="ready"))]) as (client, counts, _, _):
            client.sendall(b"\x00\x00")
            client.shutdown(socket.SHUT_WR)
            self.assertEqual(client.recv(1), b"")
            self.assertTrue(wait_rows(counts, 1)["errors"])


class SummaryTests(unittest.TestCase):
    def test_paired_bootstrap_sorted_trials_and_excluded_round(self):
        rows = cohort()
        for row in rows:
            if row["trial"] < 0:
                row.update(ready_ms=9000, completion_ms=10000)
        result = harness.summarize(list(reversed(rows)), 3, 0)
        metric = result["cold"]["ready_ms"]
        self.assertEqual(metric["v1"]["n"], 3)
        self.assertEqual(metric["v1"]["p50"], 101)
        self.assertEqual(metric["v1"]["p95_reference"], "")
        self.assertEqual(metric["ci95"], [10, 10])
        self.assertEqual(metric["delta_v2_minus_v1"], 10)
        self.assertEqual(metric["bootstrap_resamples"], 5000)
        self.assertEqual(result, harness.summarize(rows, 3, 0))

    def test_index_pairing_summary_labels_and_warm_metadata(self):
        variants = tuple(harness.COMPARISONS["v2-index"])
        rows = cohort(3, 2, variants)
        for row in rows:
            if row["trial"] < 0:
                row.update(ready_ms=9000, completion_ms=10000, metadata_bytes=9000)
        result = harness.summarize(list(reversed(rows)), 3, 2, variants)
        self.assertEqual(result, harness.summarize(rows, 3, 2, variants))
        for cache in ("cold", "warm"):
            metric = result[cache]["ready_ms"]
            self.assertEqual(metric["rpc"]["p50"], 101)
            self.assertEqual(metric["pages"]["p50"], 111)
            self.assertEqual(metric["delta_pages_minus_rpc"], 10)
            self.assertEqual(metric["ci95"], [10, 10])
            self.assertNotIn("v1", metric)
            self.assertNotIn("delta_v2_minus_v1", metric)
            metadata = result[cache]["metadata_bytes"]
            self.assertEqual(metadata["rpc"]["p50"], 20)
            self.assertEqual(metadata["pages"]["p50"], 40)
            self.assertEqual(metadata["ci95"], [20, 20])
        for bad in (rows[1:], rows + [rows[0]], [dict(row, variant="v2") for row in rows]):
            with self.subTest(bad=bad[:1]), self.assertRaises(ValueError):
                harness.summarize(bad, 3, 2, variants)
        with self.assertRaises(ValueError):
            harness.summarize(rows, 3, 2)

    def test_legacy_rows_without_metadata_remain_supported(self):
        rows = cohort()
        for row in rows:
            del row["metadata_bytes"]
        result = harness.summarize(rows, 3, 0)
        self.assertEqual(result["cold"]["ready_ms"]["delta_v2_minus_v1"], 10)
        self.assertEqual(result["warm"]["metadata_bytes"]["v2"]["p50"], 0)

    def test_duplicate_incomplete_unexpected_wrong_correctness(self):
        rows = cohort()
        bad = [
            rows + [rows[0]],
            rows[1:],
            rows[:-1],
            [dict(row, correctness="failed") for row in rows],
            [dict(row, trial=999) for row in rows],
            [dict(row, bundle_validated=False) for row in rows],
            [dict(row, proxy_errors=["partial body"]) for row in rows],
        ]
        for values in bad:
            with self.subTest(values=values[:1]), self.assertRaises(ValueError):
                harness.summarize(values, 3, 0)

    def test_content_and_timing_validation(self):
        rows = cohort()
        for changes in (
            dict(content_bytes=0),
            dict(ready_ms=float("nan")),
            dict(completion_ms=1),
            dict(connections=-1),
            dict(metadata_bytes=-1),
            dict(metadata_bytes=float("nan")),
            dict(metadata_bytes=True),
        ):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                harness.summarize([dict(rows[0], **changes), *rows[1:]], 3, 0)
        warm = next(i for i, row in enumerate(rows) if row["cache"] == "warm")
        rows[warm]["content_bytes"] = 1
        with self.assertRaises(ValueError):
            harness.summarize(rows, 3, 0)

    def test_separated_clusters_and_reference_p95(self):
        rows = cohort(30, 3)
        for row in rows:
            if row["trial"] >= 0:
                row["ready_ms"] = (100 if row["trial"] < 23 else 1000) + row["trial"]
                row["completion_ms"] = row["ready_ms"] + 10
        result = harness.summarize(rows, 30, 3)["cold"]["ready_ms"]
        self.assertEqual(result["v1"]["distribution"], "separated-clusters")
        self.assertEqual(result["v1"]["p50"], "")
        self.assertEqual(result["v1"]["low_n"], 23)
        self.assertEqual(result["v1"]["high_n"], 7)
        self.assertAlmostEqual(result["v1"]["low_fraction"], 23 / 30)
        self.assertNotEqual(result["v1"]["p95_reference"], "")
        self.assertEqual(result["conclusion"], "no detected difference")


def stats_bytes(**changes):
    words = dict.fromkeys(harness.BRIDGE_WORDS, 0)
    words.update(schema=1, **changes)
    return struct.pack("=16Q", *(words[name] for name in harness.BRIDGE_WORDS))


class BridgeTelemetryTests(unittest.TestCase):
    def test_native_abi_schema_size_reserved_and_killed_gauges(self):
        data = stats_bytes(
            accepted_connections=7,
            v1_frames=3,
            v2_frames=4,
            forwarded_stat=1,
            forwarded_list=2,
            forwarded_read=3,
            forwarded_metadata=1,
            queue_wait_ns=123,
            upstream_execution_ns=456,
            rejected_connections=2,
            queue_full=1,
            active_connections=9,
            active_workers=4,
            queued_requests=8,
        )
        parsed = harness.parse_bridge_stats(data)
        self.assertEqual(list(parsed), list(harness.BRIDGE_WORDS))
        self.assertEqual(parsed["upstream_execution_ns"], 456)
        self.assertEqual(parsed["queued_requests"], 8)  # SIGKILL need not decrement gauges.
        for bad in (
            data[:-1],
            data + b"\0",
            struct.pack("=16Q", 2, *([0] * 15)),
            stats_bytes(reserved=1),
        ):
            with self.subTest(data=bad), self.assertRaises(ValueError):
                harness.parse_bridge_stats(bad)

    def test_reconcile_excludes_entire_prepare_connection_not_all_root_stats(self):
        requests = [dict(connection_id=1, op=op) for op in ("prepare", "stat", "list")]
        requests += [dict(connection_id=2, op=op) for op in ("ping", "stat", "read", "metadata")]
        stats = harness.parse_bridge_stats(
            stats_bytes(
                accepted_connections=5,
                v1_frames=4,
                forwarded_stat=1,
                forwarded_read=1,
                forwarded_metadata=1,
            )
        )
        result = harness.reconcile_bridge(stats, requests)
        self.assertEqual(result["host_prelaunch_connections"], [1])
        self.assertEqual(result["forwarded_operations"], dict(stat=1, read=1, metadata=1))
        # Legacy's denied V2 probe increases connections, never v2_frames.
        self.assertEqual(stats["v2_frames"], 0)
        with self.assertRaises(ValueError):
            harness.reconcile_bridge(dict(stats, forwarded_stat=2), requests)
        with self.assertRaises(ValueError):
            harness.reconcile_bridge(dict(stats, v1_frames=2), requests)

    def fixture(self, output, **changes):
        row = dict(trial=0, variant="bridge_v1", cache="warm", requests=2, connections=1)
        folder = output / "trial-0-bridge_v1" / "warm"
        folder.mkdir(parents=True)
        directory = harness.create_bridge_metrics_dir(folder)
        harness.save(
            folder / "requests.json",
            dict(rows=[dict(connection_id=1, op=op) for op in ("prepare", "stat")]),
        )
        path = directory / "bridge-test.stats"
        path.write_bytes(stats_bytes(**changes))
        path.chmod(0o600)
        return row, directory, path

    def test_collect_zero_request_warm_bridge_after_teardown_and_keep_upstream(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp)
            row, directory, path = self.fixture(output, active_connections=1)
            self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            self.assertEqual(directory.stat().st_uid, os.getuid())
            with self.assertRaises(ValueError):
                harness.collect_bridge_metrics(output, [row], False, required=True)
            totals = harness.collect_bridge_metrics(output, [row], True, required=True)
            self.assertEqual(row["bridge_connections"], 0)
            self.assertEqual(row["bridge_requests"], 0)
            self.assertEqual(row["requests"], 2)
            self.assertEqual(row["connections"], 1)
            self.assertTrue(row["bridge_telemetry_validated"])
            self.assertEqual(row["bridge_stats"][0]["active_connections"], 1)
            self.assertEqual(totals["bridge_requests"], 0)
            path.unlink()
            with self.assertRaisesRegex(ValueError, "missing bridge stats"):
                harness.collect_bridge_metrics(output, [row], True, required=True)

    def test_collect_sums_all_files_and_retains_each_snapshot(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp)
            row, directory, path = self.fixture(
                output,
                accepted_connections=2,
                v1_frames=1,
                queue_wait_ns=4,
                upstream_execution_ns=8,
            )
            second = directory / "bridge-second.stats"
            second.write_bytes(
                stats_bytes(
                    accepted_connections=3, v1_frames=2, queue_wait_ns=5, upstream_execution_ns=9
                )
            )
            second.chmod(0o600)
            totals = harness.collect_bridge_metrics(output, [row], True, required=True)
            self.assertEqual(len(row["bridge_stats"]), 2)
            self.assertEqual(row["bridge_connections"], 5)
            self.assertEqual(row["bridge_requests"], 3)
            self.assertEqual(totals["bridge_queue_wait_ns"], 9)
            self.assertEqual(totals["bridge_upstream_execution_ns"], 17)

    def test_collect_malformed_unsafe_and_unexpected_v2_fail(self):
        for case in ("short", "mode", "symlink", "directory", "v2", "mismatch"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                output = Path(tmp)
                row, directory, path = self.fixture(output)
                if case == "short":
                    path.write_bytes(b"bad")
                elif case == "mode":
                    path.chmod(0o644)
                elif case == "symlink":
                    target = directory / "target"
                    path.rename(target)
                    path.symlink_to(target)
                elif case == "directory":
                    directory.chmod(0o755)
                elif case == "v2":
                    path.write_bytes(stats_bytes(v2_frames=1))
                else:
                    path.write_bytes(stats_bytes(v1_frames=1, forwarded_stat=1))
                with self.assertRaises(ValueError):
                    harness.collect_bridge_metrics(output, [row], True, required=True)

    def test_bridge_summary_paired_telemetry_and_validation(self):
        variants = tuple(harness.COMPARISONS["bridge-rpc"])
        rows = cohort(3, 0, variants)
        for row in rows:
            row.update(dict.fromkeys(harness.BRIDGE_METRICS, 0))
            candidate = row["variant"] == "bridge_v2"
            row.update(
                bridge_connections=2 if candidate else 6,
                bridge_requests=4,
                bridge_v2_frames=4 if candidate else 0,
                bridge_v1_frames=0 if candidate else 4,
                bridge_forwarded_stat=1,
                bridge_queue_wait_ns=10,
                bridge_upstream_execution_ns=20,
                bridge_telemetry_validated=True,
            )
        result = harness.summarize(list(reversed(rows)), 3, 0, variants)
        self.assertEqual(result, harness.summarize(rows, 3, 0, variants))
        for cache in ("cold", "warm"):
            metric = result[cache]["bridge_connections"]
            self.assertEqual(metric["delta_bridge_v2_minus_bridge_v1"], -4)
            self.assertEqual(metric["ci95"], [-4, -4])
            self.assertEqual(result[cache]["bridge_requests"]["bridge_v2"]["p50"], 4)
        for changes in (
            dict(bridge_telemetry_validated=False),
            dict(bridge_requests=5),
            dict(bridge_queue_wait_ns=-1),
            dict(bridge_connections=True),
            dict(bridge_upstream_execution_ns=float("nan")),
            dict(bridge_requests=None),
            dict(bridge_v2_frames=1),
            dict(bridge_forwarded_metadata=10),
        ):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                harness.summarize([dict(rows[0], **changes), *rows[1:]], 3, 0, variants)
        # Telemetry never replaces the unchanged cold/warm content checks.
        with self.assertRaises(ValueError):
            harness.summarize([dict(rows[0], content_bytes=0), *rows[1:]], 3, 0, variants)


class ComparisonTests(unittest.TestCase):
    def test_variant_environments_and_inherited_overrides(self):
        self.assertEqual(
            harness.COMPARISONS["v1-v2"],
            {
                "v1": {
                    "PVISOR_LAZY_IMAGE_V2": "0",
                    "PVISOR_LAZY_INDEX_PAGES": "0",
                    "PVISOR_LAZY_BRIDGE_V2": "0",
                },
                "v2": {
                    "PVISOR_LAZY_IMAGE_V2": "1",
                    "PVISOR_LAZY_INDEX_PAGES": "0",
                    "PVISOR_LAZY_BRIDGE_V2": "0",
                },
            },
        )
        with patch.dict(
            os.environ,
            {
                "PVISOR_LAZY_IMAGE_V2": "0",
                "PVISOR_LAZY_INDEX_PAGES": "unexpected",
                "PVISOR_LAZY_BRIDGE_V2": "unexpected",
                "PVISOR_LAZY_BRIDGE_METRICS_DIR": "/stale",
            },
        ):
            base = harness.clean_env()
        self.assertNotIn("PVISOR_LAZY_IMAGE_V2", base)
        self.assertNotIn("PVISOR_LAZY_INDEX_PAGES", base)
        self.assertNotIn("PVISOR_LAZY_BRIDGE_V2", base)
        self.assertNotIn("PVISOR_LAZY_BRIDGE_METRICS_DIR", base)
        for comparison in ("v1-v2", "v2-index"):
            for env in harness.COMPARISONS[comparison].values():
                self.assertEqual(env["PVISOR_LAZY_BRIDGE_V2"], "0")
        for comparison, pages in (("bridge-rpc", "0"), ("bridge-pages", "1")):
            for variant, mode in (("bridge_v1", "0"), ("bridge_v2", "1")):
                env = base | harness.COMPARISONS[comparison][variant]
                self.assertEqual(env["PVISOR_LAZY_IMAGE_V2"], "1")
                self.assertEqual(env["PVISOR_LAZY_INDEX_PAGES"], pages)
                self.assertEqual(env["PVISOR_LAZY_BRIDGE_V2"], mode)
        for variant, v2 in (("v1", "0"), ("v2", "1")):
            env = base | harness.COMPARISONS["v1-v2"][variant]
            self.assertEqual(env["PVISOR_LAZY_IMAGE_V2"], v2)
            self.assertEqual(env["PVISOR_LAZY_INDEX_PAGES"], "0")
        for variant, pages in (("rpc", "0"), ("pages", "1")):
            env = base | harness.COMPARISONS["v2-index"][variant]
            self.assertEqual(env["PVISOR_LAZY_IMAGE_V2"], "1")
            self.assertEqual(env["PVISOR_LAZY_INDEX_PAGES"], pages)

    def test_cli_comparison_default_and_explicit_forwarding(self):
        argv = [
            "lazy_image_v2.py",
            "--binary-dir",
            "/release",
            "--prepared-store",
            "/prepared",
            "--output",
            "/evidence",
            "--namespace-child",
        ]
        for flags, expected in (
            ([], "v1-v2"),
            (["--comparison", "v1-v2"], "v1-v2"),
            (["--comparison", "v2-index"], "v2-index"),
            (["--comparison", "bridge-rpc"], "bridge-rpc"),
            (["--comparison", "bridge-pages"], "bridge-pages"),
        ):
            with (
                self.subTest(comparison=expected),
                patch.object(sys, "argv", argv + flags),
                patch.object(harness, "child", return_value=0) as child,
            ):
                self.assertEqual(harness.main(), 0)
                args = child.call_args.args[0]
                self.assertEqual(args.comparison, expected)
                command = harness.namespace_command(args)
                self.assertEqual(command[command.index("--comparison") + 1], expected)
        with (
            patch.object(sys, "argv", argv + ["--comparison", "unknown"]),
            contextlib.redirect_stderr(None),
            self.assertRaises(SystemExit),
        ):
            harness.main()


class ChrootTests(unittest.TestCase):
    def test_bind_plan_canonicalizes_directories_and_preserves_usrmerge_links(self):
        aliases = {
            "/bin": "usr/bin",
            "/sbin": "usr/sbin",
            "/lib": "usr/lib",
            "/lib64": "/usr/lib64",
        }
        canonical = {name: Path("/" + target.lstrip("/")) for name, target in aliases.items()}
        with (
            patch.object(Path, "exists", autospec=True, return_value=True),
            patch.object(
                Path,
                "resolve",
                autospec=True,
                side_effect=lambda path, **kwargs: canonical.get(str(path), path),
            ),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(
                Path, "is_symlink", autospec=True, side_effect=lambda path: str(path) in aliases
            ),
            patch.object(harness.os, "readlink", side_effect=lambda path: aliases[str(path)]),
        ):
            plan = harness.chroot_bind_plan()
        self.assertEqual(set(plan["binds"]), {"/home", "/usr", "/etc", "/dev", "/proc"})
        self.assertEqual({row["path"]: row["target"] for row in plan["symlinks"]}, aliases)

    def test_bind_plan_rejects_whole_host_root_or_private_tmp(self):
        for unsafe in (Path("/"), Path("/tmp/host-tree")):
            with (
                self.subTest(unsafe=unsafe),
                patch.object(Path, "resolve", autospec=True, return_value=unsafe),
                patch.object(Path, "is_dir", autospec=True, return_value=True),
            ):
                with self.assertRaisesRegex(RuntimeError, "unsafe chroot bind source"):
                    harness.chroot_bind_plan()

    def make_launch_fixture(self, base):
        root = base / "private-root"
        root.mkdir(mode=0o700)
        (root / "tmp").mkdir()
        (root / "tmp").chmod(0o1777)
        frozen = base / "frozen"
        frozen.mkdir()
        for name in ("pvisor", "pvisor-cache"):
            (frozen / name).write_bytes(("frozen exact bytes: " + name).encode())
        return root, frozen

    def test_launch_copies_are_identical_and_owned_private_paths_are_valid(self):
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            binaries = harness.copy_launch_binaries(frozen, root)
            harness.validate_launch_tree(root)
            harness.verify_launch_binaries(binaries, root)
            for name, record in binaries.items():
                copied = root / "tmp/lazy-binaries" / name
                self.assertEqual(copied.read_bytes(), (frozen / name).read_bytes())
                self.assertEqual(record["launch_path"], "/tmp/lazy-binaries/" + name)
                self.assertEqual(record["frozen_sha256"], record["launch_sha256"])
                self.assertEqual(copied.stat().st_mode & 0o7777, 0o700)
            (root / "tmp/lazy-binaries").chmod(0o777)
            with self.assertRaisesRegex(RuntimeError, "unsafe private launch ancestor"):
                harness.validate_launch_tree(root)

    def test_corrupted_copy_or_changed_launch_binary_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            with patch.object(
                harness.shutil,
                "copyfile",
                side_effect=lambda source, destination: destination.write_bytes(b"corrupt"),
            ):
                with self.assertRaisesRegex(RuntimeError, "copy hash mismatch"):
                    harness.copy_launch_binaries(frozen, root)
        with tempfile.TemporaryDirectory() as directory:
            root, frozen = self.make_launch_fixture(Path(directory).resolve())
            binaries = harness.copy_launch_binaries(frozen, root)
            (root / "tmp/lazy-binaries/pvisor").write_bytes(b"changed after copy")
            with self.assertRaisesRegex(RuntimeError, "launch/frozen binary changed"):
                harness.verify_launch_binaries(binaries, root)

    def test_unbound_evidence_path_refused_before_construction(self):
        with (
            patch.object(
                harness, "chroot_bind_plan", return_value=dict(binds=["/home"], symlinks=[])
            ),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(harness.tempfile, "mkdtemp") as create,
            patch.object(harness.subprocess, "run") as mount,
        ):
            with self.assertRaisesRegex(RuntimeError, "cannot preserve evidence/workspace path"):
                harness.enter_private_chroot(Path("/elsewhere/evidence"), Path("/home/workspace"))
            create.assert_not_called()
            mount.assert_not_called()

    def test_pivot_root_utility_invocation(self):
        root = Path("/tmp/private-root")
        with (
            patch.object(harness.shutil, "which", return_value="/usr/sbin/pivot_root"),
            patch.object(harness.subprocess, "run") as run,
        ):
            method = harness.perform_pivot_root(root)
        run.assert_called_once_with(
            ["/usr/sbin/pivot_root", str(root), str(root / ".old-root")], check=True, timeout=10
        )
        self.assertIn("utility", method)

    def test_pivot_root_libc_fallback_without_architecture_specific_syscall_number(self):
        root = Path("/tmp/private-root")
        pivot = Mock(return_value=0)
        with (
            patch.object(harness.shutil, "which", return_value=None),
            patch.object(
                harness.ctypes, "CDLL", return_value=SimpleNamespace(pivot_root=pivot)
            ) as load,
        ):
            self.assertEqual(harness.perform_pivot_root(root), "libc pivot_root")
        load.assert_called_once_with(None, use_errno=True)
        pivot.assert_called_once_with(os.fsencode(root), os.fsencode(root / ".old-root"))
        self.assertEqual(pivot.argtypes, [harness.ctypes.c_char_p, harness.ctypes.c_char_p])
        self.assertEqual(pivot.restype, harness.ctypes.c_int)

    def test_unsupported_or_denied_pivot_root_is_environment_gap_not_chroot_fallback(self):
        root = Path("/tmp/private-root")
        cases = [SimpleNamespace(), SimpleNamespace(pivot_root=Mock(return_value=-1))]
        for libc in cases:
            with (
                self.subTest(libc=libc),
                patch.object(harness.shutil, "which", return_value=None),
                patch.object(harness.ctypes, "CDLL", return_value=libc),
                patch.object(harness.ctypes, "get_errno", return_value=1),
                patch.object(harness.os, "chroot") as chroot,
            ):
                with self.assertRaisesRegex(
                    harness.ContainmentEnvironmentGap, "pivot_root environment gap"
                ):
                    harness.perform_pivot_root(root)
                chroot.assert_not_called()
        with (
            patch.object(harness.shutil, "which", return_value="/usr/sbin/pivot_root"),
            patch.object(
                harness.subprocess,
                "run",
                side_effect=harness.subprocess.CalledProcessError(1, ["pivot_root"]),
            ),
        ):
            with self.assertRaises(harness.ContainmentEnvironmentGap):
                harness.perform_pivot_root(root)

    def test_construction_mounts_and_pivot_are_mocked_and_receipt_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            host_home = base / "host-home"
            output = host_home / "evidence"
            cwd = host_home / "workspace"
            (output / "frozen/bin").mkdir(parents=True)
            cwd.mkdir()
            for name in ("pvisor", "pvisor-cache"):
                (output / "frozen/bin" / name).write_bytes(("same artifact: " + name).encode())
            private_root = base / "owned-root"
            private_root.mkdir(mode=0o700)
            sources = [str(host_home), "/usr", "/etc", "/dev", "/proc"]
            plan = dict(binds=sources, symlinks=[dict(path="/bin", target="usr/bin")])
            original_validate = harness.validate_launch_tree
            original_verify = harness.verify_launch_binaries
            original_read_text = Path.read_text
            operations = []

            def run(command, **kwargs):
                operations.append(("run", command))

            def pivot(root):
                operations.append(("pivot_root", root))
                return "mock pivot_root"

            def change_directory(path):
                operations.append(("chdir", path))

            def validate(root):
                original_validate(private_root if root == Path("/") else root)

            def verify(binaries, root=Path("/")):
                original_verify(binaries, private_root if root == Path("/") else root)

            def read_text(path, *args, **kwargs):
                if path == Path("/proc/self/mountinfo"):
                    return "mock private chroot mountinfo\n"
                return original_read_text(path, *args, **kwargs)

            with (
                patch.object(harness, "chroot_bind_plan", return_value=plan),
                patch.object(harness.tempfile, "mkdtemp", return_value=str(private_root)),
                patch.object(harness.subprocess, "run", side_effect=run) as mount,
                patch.object(harness, "perform_pivot_root", side_effect=pivot) as root_switch,
                patch.object(harness.os, "chroot") as chroot,
                patch.object(harness.os, "chdir", side_effect=change_directory) as chdir,
                patch.object(harness, "validate_launch_tree", side_effect=validate),
                patch.object(harness, "verify_launch_binaries", side_effect=verify),
                patch.object(Path, "read_text", autospec=True, side_effect=read_text),
            ):
                receipt = harness.enter_private_chroot(output, cwd)
            chroot.assert_not_called()
            root_switch.assert_called_once_with(private_root)
            self.assertEqual(
                [call.args[0] for call in chdir.call_args_list], [private_root, "/", cwd]
            )
            self.assertEqual(
                operations[-5:],
                [
                    ("chdir", private_root),
                    ("pivot_root", private_root),
                    ("chdir", "/"),
                    ("run", ["umount", "-l", "/.old-root"]),
                    ("chdir", cwd),
                ],
            )
            self.assertEqual(mount.call_count, len(sources) + 2)
            self.assertEqual(
                mount.call_args_list[0].args[0],
                ["mount", "--bind", str(private_root), str(private_root)],
            )
            self.assertEqual(mount.call_args_list[0].kwargs, dict(check=True, timeout=10))
            self.assertEqual(mount.call_args_list[-1].args[0], ["umount", "-l", "/.old-root"])
            self.assertEqual(mount.call_args_list[-1].kwargs, dict(check=True, timeout=10))
            for call, source in zip(mount.call_args_list[1:-1], sources):
                self.assertEqual(
                    call.args[0],
                    ["mount", "--rbind", source, str(private_root / source.lstrip("/"))],
                )
                self.assertEqual(call.kwargs, dict(check=True, timeout=10))
            self.assertEqual(os.readlink(private_root / "bin"), "usr/bin")
            self.assertTrue(receipt["entered"])
            self.assertEqual(receipt["root_switch"], "pivot_root")
            self.assertEqual(receipt["pivot_root_method"], "mock pivot_root")
            self.assertTrue(receipt["root_self_bind"])
            self.assertTrue(receipt["old_root_detached"])
            self.assertEqual((private_root / ".old-root").stat().st_mode & 0o7777, 0o700)
            self.assertTrue(receipt["evidence_path_unchanged"])
            self.assertEqual(receipt["cwd"], str(cwd))
            self.assertEqual(len(receipt["binds"]), len(sources))
            self.assertTrue(all(bind["recursive"] for bind in receipt["binds"]))
            self.assertEqual(json.loads((output / "containment-chroot.json").read_text()), receipt)
            for name in ("pvisor", "pvisor-cache"):
                record = receipt["binaries"][name]
                self.assertEqual(record["launch_path"], "/tmp/lazy-binaries/" + name)
                self.assertEqual(record["launch_sha256"], record["frozen_sha256"])


class PreparedStoreTests(unittest.TestCase):
    def test_copy_preserves_absolute_external_links_without_hashing_outside(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            source, destination, outside = (
                base / "prepared",
                base / "evidence-store",
                base / "outside",
            )
            (source / "rootfs/var").mkdir(parents=True)
            outside.mkdir()
            (outside / "secret").write_bytes(b"host data: never read or copy")
            (source / "record.json").write_bytes(b"actual prepared record")
            (source / "rootfs/regular").write_bytes(b"image bytes")
            links = {
                "rootfs/var/run": str(outside),
                "rootfs/external-file": str(outside / "secret"),
                "rootfs/broken": str(outside / "missing"),
                "rootfs/relative": "regular",
            }
            for name, target in links.items():
                (source / name).symlink_to(target)
            harness.copy_prepared_store(source, destination)
            for name, target in links.items():
                self.assertTrue((destination / name).is_symlink())
                self.assertEqual(os.readlink(destination / name), target)
            hashed = []
            original_sha = harness.lazy_startup.sha

            def confined_hash(path):
                self.assertFalse(path.is_symlink())
                self.assertTrue(path.resolve().is_relative_to(destination))
                hashed.append(str(path.relative_to(destination)))
                return original_sha(path)

            with patch.object(harness.lazy_startup, "sha", side_effect=confined_hash):
                manifest = harness.prepared_store_manifest(destination)
            self.assertEqual(set(hashed), {"record.json", "rootfs/regular"})
            self.assertEqual(set(manifest), set(links) | set(hashed))
            for name, target in links.items():
                self.assertEqual(manifest[name], dict(kind="symlink", target=target))
            self.assertEqual(
                manifest["rootfs/regular"],
                dict(kind="regular", sha256=hashlib.sha256(b"image bytes").hexdigest()),
            )
            self.assertNotIn("rootfs/var/run/secret", manifest)
            self.assertEqual((outside / "secret").read_bytes(), b"host data: never read or copy")


def proc_stat(pid, starttime):
    # Fields after comm begin at field 3; starttime is field 22 (index 19).
    fields = ["S", *(["0"] * 18), str(starttime), "0", "0"]
    return f"{pid} (protected ) worker) " + " ".join(fields) + "\n"


@contextlib.contextmanager
def inaccessible_proc(processes):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory).resolve()
        paths = []
        for pid, starttime in processes.items():
            path = root / str(pid)
            path.mkdir()
            (path / "stat").write_text(proc_stat(pid, starttime))
            paths.append(path)
        with (
            patch.object(Path, "iterdir", side_effect=lambda: iter(paths)),
            patch.object(harness.os, "readlink", side_effect=PermissionError("nondumpable")),
        ):
            yield root


class NamespaceTests(unittest.TestCase):
    def test_command_contains_required_isolation_and_frozen_script(self):
        args = SimpleNamespace(
            binary_dir=Path("/release"),
            prepared_store=Path("/prepared"),
            output=Path("/evidence"),
            samples=30,
            warmups=3,
            comparison="v1-v2",
        )
        command = harness.namespace_command(args)
        self.assertEqual(
            command[:9],
            [
                "unshare",
                "--user",
                "--map-root-user",
                "--mount",
                "--pid",
                "--fork",
                "--kill-child=KILL",
                "--mount-proc",
                "--propagation",
            ],
        )
        self.assertEqual(command[9], "private")
        self.assertIn("/evidence/frozen/source/benchmark/pvisor/lazy_image_v2.py", command)
        self.assertIn("--namespace-child", command)
        self.assertNotIn("--net", command)
        self.assertNotIn("docker", command)

    def supervise_mock(self, timed_out=False, leftovers=None, receipt=True):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve()
            if receipt:
                harness.save(
                    output / "containment-child.json", dict(namespaces=dict(mnt="mnt:[123]"))
                )
            process = Mock(pid=12345, returncode=-9 if timed_out else 0)
            if timed_out:
                process.wait.side_effect = [harness.subprocess.TimeoutExpired(["unshare"], 900), -9]
            with (
                patch.object(harness.subprocess, "Popen", return_value=process) as popen,
                patch.object(harness.os, "killpg") as kill,
                patch.object(harness, "capture_inaccessible_processes", return_value=[]),
                patch.object(harness, "namespace_users", return_value=leftovers or []),
            ):
                result = harness.supervise(["unshare", "mock-child"], output)
            kill.assert_called_once_with(12345, signal.SIGKILL)
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertEqual(process.wait.call_args_list[0].kwargs, dict(timeout=900))
            return result

    def test_timeout_kills_group_and_checks_teardown(self):
        result = self.supervise_mock(timed_out=True)
        self.assertTrue(result["timed_out"])
        self.assertTrue(result["teardown_verified"])
        self.assertEqual(result["leftover_namespace_users"], [])

    def test_normal_exit_also_kills_owned_group(self):
        result = self.supervise_mock()
        self.assertFalse(result["timed_out"])
        self.assertTrue(result["kill_group_attempted"])
        self.assertTrue(result["teardown_verified"])

    def test_missing_receipt_cannot_claim_teardown(self):
        self.assertFalse(self.supervise_mock(receipt=False)["teardown_verified"])

    def test_permission_denied_audit_is_failure(self):
        fake = Path("/proc/42")
        with (
            patch.object(Path, "iterdir", return_value=iter([fake])),
            patch.object(Path, "stat", return_value=Mock(st_uid=harness.os.getuid())),
            patch.object(harness, "process_starttime", return_value=100),
            patch.object(harness.os, "readlink", side_effect=PermissionError()),
        ):
            with self.assertRaisesRegex(RuntimeError, "cannot audit"):
                harness.namespace_users("mnt:[123]")

    def test_leftover_namespace_users_invalidate_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve()
            harness.save(output / "containment-child.json", dict(namespaces=dict(mnt="mnt:[123]")))
            process = Mock(pid=12345, returncode=0)
            with (
                patch.object(harness.subprocess, "Popen", return_value=process),
                patch.object(harness.os, "killpg"),
                patch.object(harness, "capture_inaccessible_processes", return_value=[]),
                patch.object(harness, "namespace_users", return_value=[789]),
                patch.object(harness.time, "monotonic", side_effect=[0, 6]),
            ):
                result = harness.supervise(["unshare", "mock-child"], output)
            self.assertFalse(result["teardown_verified"])
            self.assertEqual(result["leftover_namespace_users"], [789])

    def test_stable_preexisting_inaccessible_identity_is_exempt(self):
        with inaccessible_proc({42: 100}) as root:
            self.assertEqual(harness.process_starttime(root / "42"), 100)
            baseline = harness.capture_inaccessible_processes()
            self.assertEqual([(row["pid"], row["starttime"]) for row in baseline], [(42, 100)])
            exclusions = []
            self.assertEqual(harness.namespace_users("mnt:[123]", baseline, exclusions), [])
            self.assertEqual(exclusions, baseline)
            self.assertIn("before namespace creation", exclusions[0]["reason"])

    def test_pid_reuse_cannot_inherit_preexisting_exemption(self):
        with inaccessible_proc({42: 100}) as root:
            baseline = harness.capture_inaccessible_processes()
            (root / "42/stat").write_text(proc_stat(42, 200))
            exclusions = []
            with self.assertRaisesRegex(RuntimeError, "reused"):
                harness.namespace_users("mnt:[123]", baseline, exclusions)
            self.assertEqual(exclusions, [])

    def test_new_inaccessible_process_fails_closed(self):
        with inaccessible_proc({43: 200}):
            baseline = [dict(pid=42, starttime=100, reason="preexisting")]
            exclusions = []
            with self.assertRaisesRegex(RuntimeError, "cannot audit namespace user 43"):
                harness.namespace_users("mnt:[123]", baseline, exclusions)
            self.assertEqual(exclusions, [])

    def test_identity_change_during_denial_cannot_be_exempt(self):
        with (
            inaccessible_proc({42: 100}),
            patch.object(harness, "process_starttime", side_effect=[100, 200]),
        ):
            baseline = [dict(pid=42, starttime=100, reason="preexisting")]
            with self.assertRaisesRegex(RuntimeError, "changed inaccessible identity"):
                harness.namespace_users("mnt:[123]", baseline, [])

    def test_unstable_prelaunch_identity_not_captured(self):
        with (
            inaccessible_proc({42: 100}),
            patch.object(harness, "process_starttime", side_effect=[100, 200]),
        ):
            self.assertEqual(harness.capture_inaccessible_processes(), [])

    def test_accessible_preexisting_process_in_child_namespace_is_still_counted(self):
        with (
            inaccessible_proc({42: 100}),
            patch.object(harness.os, "readlink", return_value="mnt:[123]"),
        ):
            baseline = [dict(pid=42, starttime=100, reason="preexisting")]
            exclusions = []
            self.assertEqual(harness.namespace_users("mnt:[123]", baseline, exclusions), [42])
            self.assertEqual(exclusions, [])
            self.assertEqual(harness.capture_inaccessible_processes(), [])

    def test_supervisor_captures_before_spawn_and_records_exclusions(self):
        baseline = [dict(pid=42, starttime=100, reason="preexisting namespace inaccessible")]
        events = []
        process = Mock(pid=12345, returncode=0)

        def capture():
            events.append("capture")
            return baseline

        def spawn(*args, **kwargs):
            events.append("spawn")
            return process

        def audit(identity, preexisting, exclusions):
            self.assertEqual(preexisting, baseline)
            exclusions.extend(preexisting)
            return []

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve()
            harness.save(output / "containment-child.json", dict(namespaces=dict(mnt="mnt:[123]")))
            with (
                patch.object(harness, "capture_inaccessible_processes", side_effect=capture),
                patch.object(harness.subprocess, "Popen", side_effect=spawn),
                patch.object(harness.os, "killpg"),
                patch.object(harness, "namespace_users", side_effect=audit),
            ):
                receipt = harness.supervise(["unshare", "mock-child"], output)
            retained = json.loads((output / "containment-preexisting.json").read_text())
        self.assertEqual(events, ["capture", "spawn"])
        self.assertEqual(retained["inaccessible_processes"], baseline)
        self.assertEqual(receipt["preexisting_inaccessible_processes"], baseline)
        self.assertEqual(receipt["audit_exclusions"], baseline)
        self.assertTrue(receipt["teardown_verified"])

    def test_kill_group_already_gone_still_reaps(self):
        process = Mock(pid=123)
        with patch.object(harness.os, "killpg", side_effect=ProcessLookupError()):
            harness.kill_group(process)
        process.wait.assert_called_once_with(timeout=10)


if __name__ == "__main__":
    unittest.main()
