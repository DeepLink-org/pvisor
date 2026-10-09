"""Conventional runner tests; these do not start Docker or VMs."""

import contextlib
import io
import hashlib
import gzip
import tarfile
import json
import struct

import os
import socket
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, call, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lazy_startup
from lazy_startup import (
    MARKER,
    NUMPY_MARKER,
    NUMPY_SOURCE,
    NUMPY_WORKLOAD,
    TORCH_MARKER,
    TORCH_SOURCE,
    TORCH_WORKLOAD,
    WORKLOAD,
    blob_bytes,
    docker,
    exact,
    main,
    percentile,
    summarize,
    timed,
    workload,
)


def samples(count=30):
    return [
        dict(
            variant=variant,
            cache=state,
            trial=i,
            ready_ms=(100 if variant == "docker" else 200) + i,
            completion_ms=300 + i,
            response_bytes=500,
            content_bytes=400,
            correctness="passed",
        )
        for i in range(count)
        for variant in ("docker", "lazy")
        for state in ("cold", "warm")
    ]


class LazyStartupTests(unittest.TestCase):
    def test_statistics_and_paired_direction(self):
        summary = summarize(samples())
        self.assertEqual(summary["docker-cold"]["ready_ms"]["p50"], 114.5)
        self.assertEqual(summary["docker-minus-lazy-cold"]["ci95_ms"], [-100, -100])
        self.assertEqual(summary["lazy-warm"]["n"], 30)

    def test_separated_clusters_replace_median(self):
        rows = samples()
        for row in rows:
            if row["variant"] == "docker" and row["cache"] == "cold":
                row["ready_ms"] = 100 + row["trial"] if row["trial"] < 23 else 1000 + row["trial"]
        metric = summarize(rows)["docker-cold"]["ready_ms"]
        self.assertEqual(metric["distribution"], "separated-clusters")
        self.assertEqual(metric["p50"], "")
        self.assertEqual((metric["low_n"], metric["high_n"]), (23, 7))

    def test_reject_duplicate_missing_and_incorrect(self):
        rows = samples()
        for bad in (rows + [rows[0]], rows[1:], [dict(row, correctness="failed") for row in rows]):
            with self.assertRaises(ValueError):
                summarize(bad)

    def test_small_samples_do_not_report_p95(self):
        self.assertEqual(summarize(samples(2))["docker-cold"]["ready_ms"]["p95_reference"], "")

    def test_shell_quoting(self):
        argv = docker("run", "image", "/bin/sh", "-c", 'printf "hello world"')
        self.assertEqual(argv[:3], ["sg", "docker", "-c"])
        import shlex

        self.assertEqual(
            shlex.split(argv[3]),
            ["docker", "run", "image", "/bin/sh", "-c", 'printf "hello world"'],
        )

    def test_exact_and_eof(self):
        left, right = socket.socketpair()
        try:
            right.sendall(b"abcdef")
            right.shutdown(socket.SHUT_WR)
            self.assertEqual(exact(left, 6), b"abcdef")
            with self.assertRaises(EOFError):
                exact(left, 1)
        finally:
            left.close()
            right.close()

    def test_percentile_interpolates(self):
        self.assertEqual(percentile([10, 20], 0.5), 15)

    def test_ubuntu_workload_and_cli_default_preserved(self):
        self.assertEqual(workload("ubuntu-shell"), (["/bin/sh", "-c", WORKLOAD], MARKER))
        self.assertEqual(MARKER, b"LAZY_READY ubuntu 26.04\n")
        self.assertIn('test "$ID" = ubuntu', WORKLOAD)
        self.assertIn('test "$VERSION_ID" = 26.04', WORKLOAD)
        # Stop before output creation or any service/preparation work.
        with (
            patch.object(sys, "argv", ["lazy_startup.py", "--output", "unused"]),
            patch(
                "lazy_startup.workload", side_effect=RuntimeError("stop before preparation")
            ) as selected,
        ):
            with self.assertRaisesRegex(RuntimeError, "stop before preparation"):
                main()
        selected.assert_called_once_with("ubuntu-shell")
        with self.assertRaisesRegex(ValueError, "unknown workload"):
            workload("unknown")

    def test_torch_argv_and_pinned_source(self):
        argv, marker = workload("torch-import")
        self.assertEqual(
            argv,
            [
                "/usr/bin/env",
                "OMP_NUM_THREADS=1",
                "MKL_NUM_THREADS=1",
                "OPENBLAS_NUM_THREADS=1",
                "PYTHONHASHSEED=0",
                "/opt/conda/bin/python",
                "-B",
                "-u",
                "-c",
                TORCH_WORKLOAD,
            ],
        )
        self.assertEqual(marker, TORCH_MARKER)
        self.assertEqual(TORCH_MARKER, b"LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240\n")
        self.assertEqual(
            TORCH_SOURCE,
            "docker.io/determinedai/pytorch-cpu@sha256:875cbd3391016a74c42cfb0b3712d3b70f5b803a04b80eebd7f2a46b9d53d18d",
        )

    def test_torch_workload_checks_before_ready(self):
        cases = (
            ("valid", (3, 10, 14), "2.0.1+cpu", None, 1240),
            ("python version", (3, 10, 13), "2.0.1+cpu", None, 1240),
            ("torch version", (3, 10, 14), "2.0.2+cpu", None, 1240),
            ("cuda build", (3, 10, 14), "2.0.1+cpu", "11.7", 1240),
            ("incorrect arithmetic", (3, 10, 14), "2.0.1+cpu", None, 1239),
        )
        for name, python_version, torch_version, cuda, result in cases:
            with self.subTest(name=name):
                scalar = SimpleNamespace(item=Mock(return_value=result))
                squared = SimpleNamespace(sum=Mock(return_value=scalar))
                tensor = SimpleNamespace(square=Mock(return_value=squared))
                torch = SimpleNamespace(
                    __version__=torch_version,
                    version=SimpleNamespace(cuda=cuda),
                    int64=object(),
                    set_num_threads=Mock(),
                    set_num_interop_threads=Mock(),
                    arange=Mock(return_value=tensor),
                )
                fake_sys = SimpleNamespace(version_info=python_version, version=str(python_version))
                stdout = io.StringIO()
                with (
                    patch.dict(sys.modules, {"sys": fake_sys, "torch": torch}),
                    contextlib.redirect_stdout(stdout),
                ):
                    if name == "valid":
                        exec(workload("torch-import")[0][-1], {})
                    else:
                        with self.assertRaises(AssertionError):
                            exec(workload("torch-import")[0][-1], {})
                if name == "valid":
                    self.assertEqual(stdout.getvalue().encode(), TORCH_MARKER)
                    torch.set_num_threads.assert_called_once_with(1)
                    torch.set_num_interop_threads.assert_called_once_with(1)
                    torch.arange.assert_called_once_with(16, dtype=torch.int64, device="cpu")
                    tensor.square.assert_called_once_with()
                    squared.sum.assert_called_once_with()
                    scalar.item.assert_called_once_with()
                    self.assertEqual(sum(x * x for x in range(16)), result)
                else:
                    self.assertEqual(stdout.getvalue(), "")

    def test_numpy_argv_and_pinned_source(self):
        argv, marker = workload("numpy-script")
        self.assertEqual(
            argv,
            [
                "/usr/bin/env",
                "OMP_NUM_THREADS=1",
                "MKL_NUM_THREADS=1",
                "OPENBLAS_NUM_THREADS=1",
                "PYTHONHASHSEED=0",
                "/usr/local/bin/python",
                "-B",
                "-u",
                "-c",
                NUMPY_WORKLOAD,
            ],
        )
        self.assertEqual(marker, NUMPY_MARKER)
        self.assertEqual(NUMPY_MARKER, b"LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680\n")
        self.assertEqual(
            NUMPY_SOURCE,
            "docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159",
        )

    def test_numpy_workload_checks_before_ready_without_pandas(self):
        cases = (
            ("valid", (3, 13, 14), "2.5.2", 1240, 3680),
            ("python version", (3, 13, 13), "2.5.2", 1240, 3680),
            ("numpy version", (3, 13, 14), "2.5.1", 1240, 3680),
            ("incorrect square sum", (3, 13, 14), "2.5.2", 1239, 3680),
            ("incorrect matrix sum", (3, 13, 14), "2.5.2", 1240, 3679),
        )
        for name, python_version, numpy_version, square_sum, matrix_sum in cases:
            with self.subTest(name=name):
                matrix = MagicMock()
                matrix.__matmul__.return_value.sum.return_value = matrix_sum
                array = SimpleNamespace(reshape=Mock(return_value=matrix))
                squared = SimpleNamespace(sum=Mock(return_value=square_sum))
                numpy = SimpleNamespace(
                    __version__=numpy_version,
                    int64=object(),
                    arange=Mock(return_value=array),
                    square=Mock(return_value=squared),
                )
                fake_sys = SimpleNamespace(version_info=python_version, version=str(python_version))
                stdout = io.StringIO()
                # A pandas import must fail even though the selected image contains it.
                with (
                    patch.dict(sys.modules, {"sys": fake_sys, "numpy": numpy, "pandas": None}),
                    contextlib.redirect_stdout(stdout),
                ):
                    if name == "valid":
                        exec(workload("numpy-script")[0][-1], {})
                    else:
                        with self.assertRaises(AssertionError):
                            exec(workload("numpy-script")[0][-1], {})
                if name == "valid":
                    self.assertEqual(stdout.getvalue().encode(), NUMPY_MARKER)
                    numpy.arange.assert_called_once_with(16, dtype=numpy.int64)
                    array.reshape.assert_called_once_with(4, 4)
                    numpy.square.assert_called_once_with(matrix)
                    squared.sum.assert_called_once_with()
                    matrix.__matmul__.assert_called_once_with(matrix.T)
                    matrix.__matmul__.return_value.sum.assert_called_once_with()
                    rows = [list(range(i, i + 4)) for i in range(0, 16, 4)]
                    self.assertEqual(sum(x * x for row in rows for x in row), square_sum)
                    self.assertEqual(
                        sum(
                            sum(a * b for a, b in zip(left, right))
                            for left in rows
                            for right in rows
                        ),
                        matrix_sum,
                    )
                else:
                    self.assertEqual(stdout.getvalue(), "")

    def test_numpy_cli_accepts_separate_binary_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "new-cohort"
            binary_dir = Path("target/lazy-torch-build/release").resolve()
            argv = [
                "lazy_startup.py",
                "--workload",
                "numpy-script",
                "--output",
                str(output),
                "--binary-dir",
                str(binary_dir),
            ]
            # Stop at artifact hashing, before subprocesses or service preparation.
            with (
                patch.object(sys, "argv", argv),
                patch(
                    "lazy_startup.sha",
                    side_effect=["binary-hash", RuntimeError("stop before services")],
                ) as hashed,
            ):
                with self.assertRaisesRegex(RuntimeError, "stop before services"):
                    main()
            self.assertEqual(
                hashed.call_args_list,
                [call(binary_dir / "pvisor"), call(binary_dir / "pvisor-cache")],
            )

    def test_numpy_cli_cache_binary_override_preserves_launcher(self):
        root = Path(__file__).resolve().parents[2]
        cache_binary = Path("target/lazy-torch-build/release/pvisor-cache")
        for binary_dir in (None, Path("target/release")):
            with self.subTest(binary_dir=binary_dir), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "new-cohort"
                argv = [
                    "lazy_startup.py",
                    "--workload",
                    "numpy-script",
                    "--output",
                    str(output),
                    "--cache-binary",
                    str(cache_binary),
                ]
                if binary_dir is not None:
                    argv.extend(["--binary-dir", str(binary_dir)])
                launcher_dir = (
                    binary_dir.resolve() if binary_dir is not None else root / "target/release"
                )
                # Hash only the selected artifacts; never launch or disturb a Host listener.
                with (
                    patch.object(sys, "argv", argv),
                    patch(
                        "lazy_startup.sha",
                        side_effect=["binary-hash", RuntimeError("stop before services")],
                    ) as hashed,
                ):
                    with self.assertRaisesRegex(RuntimeError, "stop before services"):
                        main()
                self.assertEqual(
                    hashed.call_args_list,
                    [
                        call(launcher_dir / "pvisor"),
                        call(cache_binary.resolve()),
                    ],
                )

    def test_timed_default_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            result = timed(
                [sys.executable, "-c", f"import sys; sys.stdout.buffer.write({MARKER!r})"],
                folder,
                os.environ.copy(),
                folder,
                timeout=5,
            )
            self.assertGreaterEqual(result["ready_ms"], 0)
            self.assertGreaterEqual(result["completion_ms"], result["ready_ms"])
            self.assertEqual((folder / "stdout.log").read_bytes(), MARKER)

    def test_timed_requires_unique_selected_stdout_marker_and_success(self):
        custom = b"CUSTOM_READY\n"
        for marker in (NUMPY_MARKER, TORCH_MARKER, custom):
            cases = (
                ("valid", marker, b"", 0),
                ("missing", b"no ready output\n", b"", 0),
                ("wrong marker", MARKER, b"", 0),
                ("duplicate", marker * 2, b"", 0),
                ("stderr only", b"", marker, 0),
                ("failed exit", marker, b"", 7),
            )
            for name, stdout, stderr, code in cases:
                with (
                    self.subTest(marker=marker, name=name),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    folder = Path(directory)
                    script = (
                        f"import sys; sys.stdout.buffer.write({stdout!r}); "
                        f"sys.stderr.buffer.write({stderr!r}); sys.exit({code})"
                    )
                    argv = [sys.executable, "-c", script]
                    if name == "valid":
                        result = timed(
                            argv, folder, os.environ.copy(), folder, timeout=5, marker=marker
                        )
                        self.assertGreaterEqual(result["completion_ms"], result["ready_ms"])
                    else:
                        with self.assertRaisesRegex(RuntimeError, "workload failed"):
                            timed(argv, folder, os.environ.copy(), folder, timeout=5, marker=marker)
                    self.assertEqual((folder / "stdout.log").read_bytes(), stdout)
                    self.assertEqual((folder / "stderr.log").read_bytes(), stderr)

    def test_campaign_validation_includes_initial_round_and_warmups(self):
        rows = samples(2) + [dict(row, trial=row["trial"] - 4) for row in samples(4)]
        lazy_startup.validate_campaign_rows(rows, 2, 3)
        for bad in (
            rows[1:],
            rows + [rows[0]],
            [dict(row, correctness="failed") if row["trial"] == -1 else row for row in rows],
        ):
            with self.assertRaises(ValueError):
                lazy_startup.validate_campaign_rows(bad, 2, 3)

    def test_paired_bootstrap_aligns_by_trial_not_row_order(self):
        import random

        rows = samples()
        expected = summarize(rows)
        random.Random(42).shuffle(rows)
        actual = summarize(rows)
        for state in ("cold", "warm"):
            self.assertEqual(
                actual["docker-minus-lazy-" + state], expected["docker-minus-lazy-" + state]
            )

    def test_current_isolated_cli_dispatch_and_defaults(self):
        argv = [
            "lazy_startup.py",
            "--isolate-host",
            "--workload",
            "numpy-script",
            "--binary-dir",
            "target/new-release",
            "--output",
            "unused",
        ]
        with (
            patch.object(sys, "argv", argv),
            patch("lazy_startup.isolated_campaign", return_value=0) as run,
        ):
            self.assertEqual(main(), 0)
        args = run.call_args.args[0]
        self.assertEqual((args.samples, args.warmups), (30, 3))
        self.assertEqual(args.source, NUMPY_SOURCE)
        self.assertIsNone(args.prepared_store)
        self.assertIsNone(args.registry_source_store)
        self.assertEqual(args.registry_copy_timeout, 180)
        self.assertEqual(
            lazy_startup.CURRENT_LAZY_ENV,
            dict(PVISOR_LAZY_IMAGE_V2="1", PVISOR_LAZY_INDEX_PAGES="1", PVISOR_LAZY_BRIDGE_V2="1"),
        )

    def test_docker_direct_only_inside_mapped_namespace(self):
        with patch("lazy_startup.NAMESPACE_CHILD", True):
            self.assertEqual(
                docker("run", "image", "argument with spaces"),
                [
                    "docker",
                    "--host",
                    "unix:///run/docker.sock",
                    "run",
                    "image",
                    "argument with spaces",
                ],
            )
        self.assertEqual(docker("info")[:3], ["sg", "docker", "-c"])

    def test_namespace_command_reexecutes_frozen_entire_campaign(self):
        args = SimpleNamespace(
            output=Path("/home/user/evidence"),
            workload="numpy-script",
            source=NUMPY_SOURCE,
            registry_image="registry@sha256:pinned",
            samples=30,
            warmups=3,
            prepared_store=Path("/home/user/store"),
            registry_source_store=Path("/home/user/registry-store"),
            registry_copy_timeout=45,
        )
        command = lazy_startup.namespace_command(args)
        self.assertEqual(command[0], "unshare")
        self.assertIn("--map-root-user", command)
        self.assertIn("--kill-child=KILL", command)
        self.assertIn("/home/user/evidence/frozen/source/benchmark/pvisor/lazy_startup.py", command)
        self.assertIn("--namespace-child", command)
        self.assertIn("/home/user/evidence/frozen/bin", command)
        self.assertEqual(command[command.index("--prepared-store") + 1], "/home/user/store")
        self.assertEqual(
            command[command.index("--registry-source-store") + 1], "/home/user/registry-store"
        )
        self.assertEqual(command[-2:], ["--registry-copy-timeout", "45"])
        args.prepared_store = None
        args.registry_source_store = None
        command = lazy_startup.namespace_command(args)
        self.assertNotIn("--prepared-store", command)
        self.assertNotIn("--registry-source-store", command)

    def test_docker_bind_plan_preserves_base_and_recursive_mount_inputs(self):
        base = dict(binds=["/home", "/usr", "/etc", "/dev", "/proc"], symlinks=[])
        result = lazy_startup.docker_bind_plan(base)
        self.assertIn(str(Path("/run").resolve()), result["binds"])
        self.assertIn(str(Path("/var").resolve()), result["binds"])
        self.assertIn(str(Path("/sys").resolve()), result["binds"])
        self.assertNotIn("/tmp", result["binds"])
        self.assertEqual(base["binds"], ["/home", "/usr", "/etc", "/dev", "/proc"])
        self.assertEqual(base["symlinks"], [])

    def test_docker_bind_plan_does_not_duplicate_existing_sys_bind(self):
        base = dict(binds=["/home", "/sys"], symlinks=[])
        result = lazy_startup.docker_bind_plan(base)
        self.assertEqual(result["binds"].count(str(Path("/sys").resolve())), 1)
        self.assertEqual(base, dict(binds=["/home", "/sys"], symlinks=[]))

    def test_isolation_rejects_wrong_primary_gid_before_freeze_or_services(self):
        with (
            patch("lazy_startup.grp.getgrnam", return_value=SimpleNamespace(gr_gid=42)),
            patch("lazy_startup.os.getgid", return_value=43),
            patch("lazy_startup.isolation_helpers") as helpers,
        ):
            with self.assertRaisesRegex(RuntimeError, "entire script via sg docker"):
                lazy_startup.isolated_campaign(SimpleNamespace())
            helpers.return_value.freeze.assert_not_called()
            helpers.return_value.supervise.assert_not_called()

    def test_prepared_record_requires_digest_platform_and_handle(self):
        record = dict(
            status="prepared", digest="sha256:pinned", architecture="amd64", image_handle="handle"
        )
        self.assertEqual(
            lazy_startup.validate_prepared_record(json.dumps(record), "sha256:pinned"), record
        )
        for bad in (
            dict(record, digest="sha256:other"),
            dict(record, architecture="arm64"),
            dict(record, status="failed"),
            dict(record, image_handle=""),
        ):
            with self.assertRaisesRegex(RuntimeError, "digest/platform/handle"):
                lazy_startup.validate_prepared_record(json.dumps(bad), "sha256:pinned")

    def test_prepared_store_copy_keeps_guest_symlinks(self):
        helpers = lazy_startup.isolation_helpers()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            (source / "content").write_bytes(b"immutable")
            (source / "guest-link").symlink_to("/nonexistent/guest/path")
            helpers.copy_prepared_store(source, root / "copy")
            self.assertTrue((root / "copy/guest-link").is_symlink())
            self.assertEqual(
                helpers.prepared_store_manifest(source),
                helpers.prepared_store_manifest(root / "copy"),
            )

    def test_proxy_persistent_v2_separates_metadata_and_read_data(self):
        helpers = lazy_startup.isolation_helpers()
        counts = helpers.Counts()
        bodies = [("metadata", b"index page"), ("read", b"file bytes")]
        with lazy_startup.TCPServer(("127.0.0.1", 0), helpers.CacheProxy) as upstream:
            # Minimal real framed service, deliberately keeps V2 sockets open.
            class Service(lazy_startup.socketserver.BaseRequestHandler):
                def handle(self):
                    for op, body in bodies:
                        _, request = helpers.frame(self.request)
                        assert request["request"]["op"] == op
                        response = json.dumps(
                            dict(
                                status="data",
                                length=len(body),
                                sha256="sha256:" + hashlib.sha256(body).hexdigest(),
                            )
                        ).encode()
                        self.request.sendall(struct.pack("!I", len(response)) + response + body)

            upstream.RequestHandlerClass = Service
            lazy_startup.serve(upstream)
            with lazy_startup.TCPServer(("127.0.0.1", 0), lazy_startup.CacheProxy) as proxy:
                proxy.upstream = upstream.server_address[1]
                proxy.counts = counts
                lazy_startup.serve(proxy)
                try:
                    with socket.create_connection(proxy.server_address, timeout=3) as client:
                        for op, body in bodies:
                            request = json.dumps(dict(version=2, request=dict(op=op))).encode()
                            client.sendall(struct.pack("!I", len(request)) + request)
                            _, response = helpers.frame(client)
                            self.assertEqual(helpers.exact(client, response["length"]), body)
                        # Idle persistent sockets must not block request draining.
                        snapshot = counts.snapshot(timeout=1)
                        self.assertEqual(snapshot["connections"], 1)
                        self.assertEqual(snapshot["errors"], [])
                        rows = snapshot["rows"]
                        self.assertEqual([r["content_bytes"] for r in rows], [0, len(bodies[1][1])])
                        self.assertEqual(
                            [r["metadata_bytes"] for r in rows], [len(bodies[0][1]), 0]
                        )
                        self.assertTrue(
                            all(r["server_body_sha256"] == r["forwarded_body_sha256"] for r in rows)
                        )
                finally:
                    proxy.shutdown()
            upstream.shutdown()

    def test_isolated_parent_freeze_supervision_and_immutable_final_report(self):
        helpers = lazy_startup.isolation_helpers()
        for verified in (True, False):
            with (
                self.subTest(teardown_verified=verified),
                tempfile.TemporaryDirectory() as directory,
            ):
                output = Path(directory) / "evidence"
                args = SimpleNamespace(
                    output=output,
                    binary_dir=Path(directory),
                    prepared_store=None,
                    firmware=None,
                    cache_binary=None,
                    build_receipt=None,
                    registry_source_store=None,
                    registry_copy_timeout=180,
                    supervisor_timeout=123,
                    workload="numpy-script",
                    source=NUMPY_SOURCE,
                    registry_image="registry@sha256:pinned",
                    samples=30,
                    warmups=3,
                )
                provenance = dict(
                    hashes={"frozen/source/benchmark/pvisor/lazy_startup.py": "harness"},
                    binary_source_relationship="unverified",
                )
                receipt = dict(timed_out=False, exit_code=0, teardown_verified=verified)

                def supervise(command, folder, timeout):
                    rows = samples() + [dict(row, trial=row["trial"] - 4) for row in samples(4)]
                    helpers.save(
                        folder / "child-result.json", dict(status="passed", failures=[], rows=rows)
                    )
                    return receipt

                # Substitute a permitted bind only to keep this conventional test in /tmp.
                with (
                    patch(
                        "lazy_startup.grp.getgrnam",
                        return_value=SimpleNamespace(gr_gid=os.getgid()),
                    ),
                    patch("lazy_startup.docker_bind_plan", return_value=dict(binds=[directory])),
                    patch.object(Path, "is_relative_to", return_value=False) as relative,
                    patch.object(helpers, "freeze", return_value=provenance) as freeze,
                    patch.object(helpers, "supervise", side_effect=supervise) as supervised,
                    patch.object(helpers, "verify_frozen") as verify,
                    patch(
                        "lazy_startup.cleanup_owned_docker",
                        return_value=dict(container_teardown_verified=True),
                    ),
                ):
                    relative.side_effect = lambda other: str(other) == directory
                    code = lazy_startup.isolated_campaign(args)
                self.assertEqual(code, 0 if verified else 1)
                freeze.assert_called_once_with(output, Path(directory))
                self.assertEqual(supervised.call_args.kwargs["timeout"], 123)
                self.assertEqual(verify.call_count, 1 if verified else 0)
                report = json.loads((output / "report.json").read_text())
                self.assertEqual(report["benchmark"], "B-LAZY-STARTUP")
                self.assertEqual(report["status"], "passed" if verified else "failed")
                self.assertEqual((output / "report.json").stat().st_mode & 0o777, 0o444)
                self.assertEqual(
                    (output / "report.sha256").read_text().split()[0],
                    lazy_startup.sha(output / "report.json"),
                )
                self.assertEqual("summary" in report, verified)

    def test_cleanup_audits_only_owned_docker_names_and_no_host_processes(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            responses = [
                SimpleNamespace(returncode=0, stdout=b"owned-id\n", stderr=b""),
                SimpleNamespace(returncode=0, stdout=b"logs", stderr=b""),
                SimpleNamespace(returncode=0, stdout=b"", stderr=b""),
                SimpleNamespace(returncode=0, stdout=b"", stderr=b""),
            ]
            with patch("lazy_startup.subprocess.run", side_effect=responses) as run:
                receipt = lazy_startup.cleanup_owned_docker(output, None)
            self.assertTrue(receipt["container_teardown_verified"])
            self.assertEqual(receipt["removed_containers"], ["owned-id"])
            commands = [c.args[0] for c in run.call_args_list]
            self.assertTrue(all(command[:3] == ["sg", "docker", "-c"] for command in commands))
            self.assertIn(lazy_startup.registry_name_for(output), commands[0][-1])
            self.assertNotIn("prune", str(commands))
            self.assertNotIn("kill", str(commands))

    def oci_fixture(
        self,
        root,
        platform="amd64",
        media_type="application/vnd.oci.image.manifest.v1+json",
        schema=2,
    ):
        # Genuine compressed tar/config/manifest bytes with content-derived IDs;
        # this is an OCI fixture, never fabricated production cache metadata.
        store = root / "store"
        blobs = store / "blobs/sha256"
        blobs.mkdir(parents=True)
        metadata = store / "metadata/manifests-v1"
        metadata.mkdir(parents=True)
        tar_bytes = io.BytesIO()
        with tarfile.open(fileobj=tar_bytes, mode="w") as archive:
            entry = tarfile.TarInfo("hello")
            entry.size = 5
            archive.addfile(entry, io.BytesIO(b"hello"))
        raw = tar_bytes.getvalue()
        layer = gzip.compress(raw, mtime=0)
        config = json.dumps(
            dict(
                os="linux",
                architecture=platform,
                rootfs=dict(type="layers", diff_ids=["sha256:" + hashlib.sha256(raw).hexdigest()]),
            )
        ).encode()

        def descriptor(body, kind):
            digest = "sha256:" + hashlib.sha256(body).hexdigest()
            (blobs / digest[7:]).write_bytes(body)
            return dict(digest=digest, size=len(body), mediaType=kind)

        manifest = dict(
            schemaVersion=schema,
            mediaType=media_type,
            config=descriptor(config, "application/vnd.oci.image.config.v1+json"),
            layers=[descriptor(layer, "application/vnd.oci.image.layer.v1.tar+gzip")],
        )
        body = json.dumps(manifest, indent=3).encode() + b"\n"
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        manifest_path = metadata / (digest[7:] + ".json")
        manifest_path.write_bytes(body)
        return store, "example.invalid/test@" + digest, manifest, body, manifest_path

    def test_registry_oci_archives_exact_manifest_and_original_compressed_blobs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            store, source, manifest, body, manifest_path = self.oci_fixture(root)
            before = {
                p: (p.read_bytes(), p.stat().st_mode) for p in store.rglob("*") if p.is_file()
            }
            output = root / "output"
            output.mkdir()
            transport, receipt = lazy_startup.archive_registry_oci(store, output, source)
            layout = output / "archive/registry-oci"
            self.assertEqual(transport, "oci:" + str(layout) + ":workload")
            self.assertEqual(
                (layout / "blobs/sha256" / source.split("sha256:")[1]).read_bytes(), body
            )
            index = json.loads((layout / "index.json").read_text())
            self.assertEqual(index["manifests"][0]["digest"], source.split("@")[1])
            self.assertEqual(index["manifests"][0]["size"], len(body))
            self.assertEqual(index["manifests"][0]["mediaType"], manifest["mediaType"])
            self.assertEqual(
                index["manifests"][0]["platform"], dict(os="linux", architecture="amd64")
            )
            self.assertEqual(
                json.loads((layout / "oci-layout").read_text()), dict(imageLayoutVersion="1.0.0")
            )
            self.assertEqual(receipt["mode"], "cached-oci")
            self.assertEqual(receipt["manifest_source_path"], str(manifest_path))
            lazy_startup.verify_registry_archive(output, receipt)
            for descriptor in [manifest["config"], *manifest["layers"]]:
                archived = layout / "blobs/sha256" / descriptor["digest"][7:]
                original = store / "blobs/sha256" / descriptor["digest"][7:]
                self.assertEqual(archived.read_bytes(), original.read_bytes())
                self.assertNotEqual(archived.stat().st_ino, original.stat().st_ino)
                self.assertEqual(archived.stat().st_mode & 0o777, 0o444)
            self.assertEqual(before, {p: (p.read_bytes(), p.stat().st_mode) for p in before})
            command = lazy_startup.registry_copy_command(transport)
            self.assertIn("--preserve-digests", command)
            self.assertEqual(command[-2], transport)
            self.assertNotIn("docker.io", str(command))

    @unittest.skipUnless(
        os.environ.get("PVISOR_TEST_RETAINED_OCI"),
        "set PVISOR_TEST_RETAINED_OCI for read-only retained-input verification",
    )
    def test_retained_numpy_original_blobs_and_exact_pinned_manifest_offline(self):
        store = Path(os.environ["PVISOR_TEST_RETAINED_OCI"]).resolve(strict=True)
        manifest_path = (
            store / "metadata/manifests-v1" / (NUMPY_SOURCE.split("sha256:")[1] + ".json")
        )
        body = manifest_path.read_bytes()
        manifest = json.loads(body)
        originals = [
            manifest_path,
            *[
                store / "blobs/sha256" / descriptor["digest"][7:]
                for descriptor in [manifest["config"], *manifest["layers"]]
            ],
        ]
        before = {path: path.stat() for path in originals}
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            transport, receipt = lazy_startup.archive_registry_oci(store, output, NUMPY_SOURCE)
            self.assertEqual(receipt["manifest_digest"], NUMPY_SOURCE.split("@")[1])
            self.assertEqual(receipt["manifest_media_type"], manifest["mediaType"])
            self.assertEqual(receipt["platform"], dict(os="linux", architecture="amd64"))
            self.assertEqual(
                sum(blob["size"] for blob in receipt["blobs"].values()), blob_bytes(manifest)
            )
            lazy_startup.verify_registry_archive(output, receipt)
            archived = output / "archive/registry-oci/blobs/sha256" / receipt["manifest_digest"][7:]
            self.assertEqual(archived.read_bytes(), body)
            if lazy_startup.shutil.which("skopeo"):
                inspected = lazy_startup.subprocess.run(
                    ["skopeo", "inspect", "--raw", transport], capture_output=True, timeout=10
                )
                self.assertEqual(inspected.returncode, 0, inspected.stderr.decode(errors="replace"))
                self.assertEqual(inspected.stdout.rstrip(b"\n"), body.rstrip(b"\n"))
            for path, original in before.items():
                current = path.stat()
                self.assertEqual(
                    (current.st_ino, current.st_size, current.st_mode, current.st_mtime_ns),
                    (original.st_ino, original.st_size, original.st_mode, original.st_mtime_ns),
                )
            print(
                f"retained OCI verified: {receipt['manifest_digest']}; "
                f"{len(receipt['blobs'])} unique blobs; {blob_bytes(manifest)} original bytes",
                flush=True,
            )

    @unittest.skipUnless(
        lazy_startup.shutil.which("skopeo"),
        "skopeo not installed; OCI transport integration unavailable",
    )
    def test_registry_oci_is_readable_by_supported_skopeo_transport_offline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            store, source, _, body, _ = self.oci_fixture(root)
            output = root / "output"
            output.mkdir()
            transport, _ = lazy_startup.archive_registry_oci(store, output, source)
            inspected = lazy_startup.subprocess.run(
                ["skopeo", "inspect", "--raw", transport], capture_output=True, timeout=10
            )
            self.assertEqual(inspected.returncode, 0, inspected.stderr.decode(errors="replace"))
            self.assertEqual(inspected.stdout.rstrip(b"\n"), body.rstrip(b"\n"))

    def test_registry_archive_revalidation_rejects_changed_index_or_blob(self):
        for name in ("index.json", "blob"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                store, source, manifest, _, _ = self.oci_fixture(root)
                output = root / "output"
                output.mkdir()
                _, receipt = lazy_startup.archive_registry_oci(store, output, source)
                path = (
                    output / "archive/registry-oci" / name
                    if name == "index.json"
                    else output
                    / "archive/registry-oci/blobs/sha256"
                    / manifest["layers"][0]["digest"][7:]
                )
                path.chmod(0o600)
                path.write_bytes(b"changed")
                with self.assertRaisesRegex(ValueError, "length/hash mismatch"):
                    lazy_startup.verify_registry_archive(output, receipt)

    def test_registry_oci_rejects_missing_corrupt_or_symlink_blobs_without_rootfs_fallback(self):
        for damage in ("missing", "length", "hash", "symlink"):
            with self.subTest(damage=damage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                store, source, manifest, _, _ = self.oci_fixture(root)
                layer = store / "blobs/sha256" / manifest["layers"][0]["digest"][7:]
                original = layer.read_bytes()
                if damage == "missing":
                    layer.unlink()
                elif damage == "length":
                    layer.write_bytes(original + b"extra")
                elif damage == "hash":
                    layer.write_bytes(bytes([original[0] ^ 1]) + original[1:])
                else:
                    target = root / "other"
                    target.write_bytes(original)
                    layer.unlink()
                    layer.symlink_to(target)
                (store / "rootfs-v3").mkdir()
                with self.assertRaises((FileNotFoundError, ValueError)):
                    lazy_startup.archive_registry_oci(store, root / "output", source)
                self.assertFalse((root / "output/archive").exists())

    def test_registry_oci_rejects_manifest_identity_schema_media_and_platform(self):
        for damage in ("manifest-hash", "schema", "media-type", "platform"):
            with self.subTest(damage=damage), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                store, source, _, body, manifest_path = self.oci_fixture(
                    root,
                    platform="arm64" if damage == "platform" else "amd64",
                    media_type="application/vnd.docker.distribution.manifest.v2+json"
                    if damage == "media-type"
                    else "application/vnd.oci.image.manifest.v1+json",
                    schema=1 if damage == "schema" else 2,
                )
                if damage == "manifest-hash":
                    manifest_path.write_bytes(body + b" ")
                with self.assertRaises(ValueError):
                    lazy_startup.archive_registry_oci(store, root / "output", source)

    def test_registry_oci_validates_config_blob_and_does_not_accept_unpinned_path_digests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            store, source, manifest, _, _ = self.oci_fixture(root)
            config = store / "blobs/sha256" / manifest["config"]["digest"][7:]
            config.write_bytes(b"bad config")
            with self.assertRaisesRegex(ValueError, "length/hash mismatch"):
                lazy_startup.archive_registry_oci(store, root / "output", source)
        for digest in ("sha256:../etc/passwd", "sha512:" + "a" * 64, "sha256:" + "A" * 64):
            with self.assertRaises(ValueError):
                lazy_startup.digest_hex(digest)

    def test_live_copy_logs_are_retained_on_success_failure_and_timeout(self):
        for script, timeout, error in (
            ('print("copy progress", flush=True)', 5, None),
            (
                'import sys; print("copy failure", file=sys.stderr, flush=True); sys.exit(7)',
                5,
                RuntimeError,
            ),
            ('import time; print("before timeout", flush=True); time.sleep(10)', 0.2, TimeoutError),
        ):
            with self.subTest(script=script), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                command = [sys.executable, "-c", script]
                with contextlib.redirect_stdout(io.StringIO()):
                    if error:
                        with self.assertRaises(error):
                            lazy_startup.checked_live(command, output, "registry-mirror", timeout)
                    else:
                        self.assertGreater(
                            lazy_startup.checked_live(command, output, "registry-mirror", timeout),
                            0,
                        )
                self.assertEqual(
                    json.loads((output / "registry-mirror.command.json").read_text()), command
                )
                self.assertTrue((output / "registry-mirror.stdout").exists())
                self.assertTrue((output / "registry-mirror.stderr").exists())
                phase = json.loads((output / "phase.json").read_text())
                self.assertEqual(
                    phase["status"],
                    "interrupted" if error is TimeoutError else "failed" if error else "passed",
                )
                if error is TimeoutError:
                    self.assertIn(
                        b"before timeout", (output / "registry-mirror.stdout").read_bytes()
                    )

    def test_live_registry_copy_emits_heartbeat_before_bounded_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            process = Mock(returncode=0)
            process.poll.side_effect = [None, 0]
            visible = io.StringIO()
            with (
                patch("lazy_startup.subprocess.Popen", return_value=process),
                patch("lazy_startup.time.monotonic", side_effect=[0, 6, 7]),
                patch("lazy_startup.time.sleep"),
                contextlib.redirect_stdout(visible),
            ):
                self.assertEqual(
                    lazy_startup.checked_live(["skopeo"], output, "registry-mirror", 10), 7000
                )
            self.assertIn('"elapsed_seconds": 6', visible.getvalue())
            self.assertEqual(json.loads((output / "phase.json").read_text())["status"], "passed")

    def test_registry_copy_network_default_keeps_preserved_digest_transport(self):
        command = lazy_startup.registry_copy_command("docker://" + NUMPY_SOURCE)
        self.assertEqual(command[-2], "docker://" + NUMPY_SOURCE)
        self.assertIn("--preserve-digests", command)
        self.assertEqual(command[-1], "docker://127.0.0.1:15000/bench/workload:latest")

    def test_registry_copy_rejects_nonpositive_timeout_before_services(self):
        with (
            patch.object(
                sys,
                "argv",
                ["lazy_startup.py", "--output", "unused", "--registry-copy-timeout", "0"],
            ),
            contextlib.redirect_stderr(io.StringIO()),
            patch("lazy_startup.isolated_campaign") as run,
        ):
            with self.assertRaises(SystemExit):
                main()
            run.assert_not_called()

    def test_offline_registry_options_parse_without_preparation(self):
        argv = [
            "lazy_startup.py",
            "--isolate-host",
            "--workload",
            "numpy-script",
            "--output",
            "unused",
            "--registry-source-store",
            "retained/store",
            "--prepared-store",
            "retained/store",
            "--registry-copy-timeout",
            "60",
        ]
        with (
            patch.object(sys, "argv", argv),
            patch("lazy_startup.isolated_campaign", return_value=0) as run,
        ):
            self.assertEqual(main(), 0)
        args = run.call_args.args[0]
        self.assertEqual(args.registry_source_store, Path("retained/store"))
        self.assertEqual(args.prepared_store, Path("retained/store"))
        self.assertEqual(args.registry_copy_timeout, 60)

    def test_blob_bytes_counts_unique_digests_including_config(self):
        config = dict(digest="sha256:config", size=10)
        layer = dict(digest="sha256:layer", size=100)
        empty = dict(digest="sha256:empty", size=20)
        self.assertEqual(blob_bytes(dict(config=config, layers=[])), 10)
        self.assertEqual(
            blob_bytes(dict(config=config, layers=[layer, empty, dict(empty), dict(layer)])), 130
        )
        self.assertEqual(blob_bytes(dict(config=config, layers=[layer, dict(config)])), 110)


if __name__ == "__main__":
    unittest.main()
