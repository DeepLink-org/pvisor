"""Conventional runner tests; these do not start Docker or VMs."""

import contextlib
import io
import json
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))


import lazy_startup
from lazy_startup import (
    MARKER,
    NUMPY_MARKER,
    NUMPY_SOURCE,
    NUMPY_WORKLOAD,
    TORCH_MARKER,
    TORCH_WORKLOAD,
    WORKLOAD,
    workload,
)


class LazyStartupTests(unittest.TestCase):
    def test_ubuntu_workload(self):
        self.assertEqual(workload("ubuntu-shell"), (["/bin/sh", "-c", WORKLOAD], MARKER))
        self.assertEqual(MARKER, b"LAZY_READY ubuntu 26.04\n")
        self.assertIn('test "$ID" = ubuntu', WORKLOAD)
        self.assertIn('test "$VERSION_ID" = 26.04', WORKLOAD)
        with self.assertRaisesRegex(ValueError, "unknown workload"):
            workload("unknown")

    def test_torch_argv(self):
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

    def test_docker_bind_plan_preserves_base_and_recursive_mount_inputs(self):
        base = dict(binds=["/home", "/usr", "/etc", "/dev", "/proc"], symlinks=[])
        with (
            patch.object(Path, "resolve", autospec=True, side_effect=lambda path, **kwargs: path),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(Path, "is_symlink", autospec=True, return_value=False),
        ):
            result = lazy_startup.docker_bind_plan(base)
        self.assertIn("/run", result["binds"])
        self.assertIn("/var", result["binds"])
        self.assertIn("/sys", result["binds"])
        self.assertNotIn("/tmp", result["binds"])
        self.assertEqual(base["binds"], ["/home", "/usr", "/etc", "/dev", "/proc"])
        self.assertEqual(base["symlinks"], [])

    def test_docker_bind_plan_does_not_duplicate_existing_sys_bind(self):
        base = dict(binds=["/home", "/sys"], symlinks=[])
        with (
            patch.object(Path, "resolve", autospec=True, side_effect=lambda path, **kwargs: path),
            patch.object(Path, "is_dir", autospec=True, return_value=True),
            patch.object(Path, "is_symlink", autospec=True, return_value=False),
        ):
            result = lazy_startup.docker_bind_plan(base)
        self.assertEqual(result["binds"].count("/sys"), 1)
        self.assertEqual(base, dict(binds=["/home", "/sys"], symlinks=[]))

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
