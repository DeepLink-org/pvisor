"""Conventional runner tests; these do not start Docker or VMs."""
import contextlib
import io
import os
import socket
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, call, patch


sys.path.insert(0, str(Path(__file__).resolve().parent))
from lazy_startup import (
    MARKER, WORKLOAD, NUMPY_MARKER, NUMPY_SOURCE, NUMPY_WORKLOAD,
    TORCH_MARKER, TORCH_SOURCE, TORCH_WORKLOAD,
    blob_bytes, docker, exact, main, percentile, summarize, timed, workload,
)


def samples(count=30):
    return [dict(variant=variant, cache=state, trial=i, ready_ms=(100 if variant == 'docker' else 200) + i, completion_ms=300 + i, response_bytes=500, content_bytes=400, correctness='passed') for i in range(count) for variant in ('docker', 'lazy') for state in ('cold', 'warm')]


class LazyStartupTests(unittest.TestCase):
    def test_statistics_and_paired_direction(self):
        summary = summarize(samples())
        self.assertEqual(summary['docker-cold']['ready_ms']['p50'], 114.5)
        self.assertEqual(summary['docker-minus-lazy-cold']['ci95_ms'], [-100, -100])
        self.assertEqual(summary['lazy-warm']['n'], 30)

    def test_separated_clusters_replace_median(self):
        rows = samples()
        for row in rows:
            if row['variant'] == 'docker' and row['cache'] == 'cold':
                row['ready_ms'] = 100 + row['trial'] if row['trial'] < 23 else 1000 + row['trial']
        metric = summarize(rows)['docker-cold']['ready_ms']
        self.assertEqual(metric['distribution'], 'separated-clusters')
        self.assertEqual(metric['p50'], '')
        self.assertEqual((metric['low_n'], metric['high_n']), (23, 7))

    def test_reject_duplicate_missing_and_incorrect(self):
        rows = samples()
        for bad in (rows + [rows[0]], rows[1:], [dict(row, correctness='failed') for row in rows]):
            with self.assertRaises(ValueError):
                summarize(bad)

    def test_small_samples_do_not_report_p95(self):
        self.assertEqual(summarize(samples(2))['docker-cold']['ready_ms']['p95_reference'], '')

    def test_shell_quoting(self):
        argv = docker('run', 'image', '/bin/sh', '-c', 'printf "hello world"')
        self.assertEqual(argv[:3], ['sg', 'docker', '-c'])
        import shlex
        self.assertEqual(shlex.split(argv[3]), ['docker', 'run', 'image', '/bin/sh', '-c', 'printf "hello world"'])

    def test_exact_and_eof(self):
        left, right = socket.socketpair()
        try:
            right.sendall(b'abcdef')
            right.shutdown(socket.SHUT_WR)
            self.assertEqual(exact(left, 6), b'abcdef')
            with self.assertRaises(EOFError):
                exact(left, 1)
        finally:
            left.close()
            right.close()

    def test_percentile_interpolates(self):
        self.assertEqual(percentile([10, 20], .5), 15)

    def test_ubuntu_workload_and_cli_default_preserved(self):
        self.assertEqual(workload('ubuntu-shell'), (['/bin/sh', '-c', WORKLOAD], MARKER))
        self.assertEqual(MARKER, b'LAZY_READY ubuntu 26.04\n')
        self.assertIn('test "$ID" = ubuntu', WORKLOAD)
        self.assertIn('test "$VERSION_ID" = 26.04', WORKLOAD)
        # Stop before output creation or any service/preparation work.
        with patch.object(sys, 'argv', ['lazy_startup.py', '--output', 'unused']), \
                patch('lazy_startup.workload', side_effect=RuntimeError('stop before preparation')) as selected:
            with self.assertRaisesRegex(RuntimeError, 'stop before preparation'):
                main()
        selected.assert_called_once_with('ubuntu-shell')
        with self.assertRaisesRegex(ValueError, 'unknown workload'):
            workload('unknown')

    def test_torch_argv_and_pinned_source(self):
        argv, marker = workload('torch-import')
        self.assertEqual(argv, [
            '/usr/bin/env', 'OMP_NUM_THREADS=1', 'MKL_NUM_THREADS=1',
            'OPENBLAS_NUM_THREADS=1', 'PYTHONHASHSEED=0',
            '/opt/conda/bin/python', '-B', '-u', '-c', TORCH_WORKLOAD,
        ])
        self.assertEqual(marker, TORCH_MARKER)
        self.assertEqual(TORCH_MARKER, b'LAZY_READY python 3.10.14 torch 2.0.1+cpu cpu_sum 1240\n')
        self.assertEqual(TORCH_SOURCE, 'docker.io/determinedai/pytorch-cpu@sha256:875cbd3391016a74c42cfb0b3712d3b70f5b803a04b80eebd7f2a46b9d53d18d')

    def test_torch_workload_checks_before_ready(self):
        cases = (
            ('valid', (3, 10, 14), '2.0.1+cpu', None, 1240),
            ('python version', (3, 10, 13), '2.0.1+cpu', None, 1240),
            ('torch version', (3, 10, 14), '2.0.2+cpu', None, 1240),
            ('cuda build', (3, 10, 14), '2.0.1+cpu', '11.7', 1240),
            ('incorrect arithmetic', (3, 10, 14), '2.0.1+cpu', None, 1239),
        )
        for name, python_version, torch_version, cuda, result in cases:
            with self.subTest(name=name):
                scalar = SimpleNamespace(item=Mock(return_value=result))
                squared = SimpleNamespace(sum=Mock(return_value=scalar))
                tensor = SimpleNamespace(square=Mock(return_value=squared))
                torch = SimpleNamespace(
                    __version__=torch_version, version=SimpleNamespace(cuda=cuda),
                    int64=object(), set_num_threads=Mock(), set_num_interop_threads=Mock(),
                    arange=Mock(return_value=tensor),
                )
                fake_sys = SimpleNamespace(version_info=python_version, version=str(python_version))
                stdout = io.StringIO()
                with patch.dict(sys.modules, {'sys': fake_sys, 'torch': torch}), \
                        contextlib.redirect_stdout(stdout):
                    if name == 'valid':
                        exec(workload('torch-import')[0][-1], {})
                    else:
                        with self.assertRaises(AssertionError):
                            exec(workload('torch-import')[0][-1], {})
                if name == 'valid':
                    self.assertEqual(stdout.getvalue().encode(), TORCH_MARKER)
                    torch.set_num_threads.assert_called_once_with(1)
                    torch.set_num_interop_threads.assert_called_once_with(1)
                    torch.arange.assert_called_once_with(16, dtype=torch.int64, device='cpu')
                    tensor.square.assert_called_once_with()
                    squared.sum.assert_called_once_with()
                    scalar.item.assert_called_once_with()
                    self.assertEqual(sum(x * x for x in range(16)), result)
                else:
                    self.assertEqual(stdout.getvalue(), '')

    def test_numpy_argv_and_pinned_source(self):
        argv, marker = workload('numpy-script')
        self.assertEqual(argv, [
            '/usr/bin/env', 'OMP_NUM_THREADS=1', 'MKL_NUM_THREADS=1',
            'OPENBLAS_NUM_THREADS=1', 'PYTHONHASHSEED=0',
            '/usr/local/bin/python', '-B', '-u', '-c', NUMPY_WORKLOAD,
        ])
        self.assertEqual(marker, NUMPY_MARKER)
        self.assertEqual(NUMPY_MARKER, b'LAZY_READY python 3.13.14 numpy 2.5.2 sum 1240 dot 3680\n')
        self.assertEqual(NUMPY_SOURCE, 'docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159')

    def test_numpy_workload_checks_before_ready_without_pandas(self):
        cases = (
            ('valid', (3, 13, 14), '2.5.2', 1240, 3680),
            ('python version', (3, 13, 13), '2.5.2', 1240, 3680),
            ('numpy version', (3, 13, 14), '2.5.1', 1240, 3680),
            ('incorrect square sum', (3, 13, 14), '2.5.2', 1239, 3680),
            ('incorrect matrix sum', (3, 13, 14), '2.5.2', 1240, 3679),
        )
        for name, python_version, numpy_version, square_sum, matrix_sum in cases:
            with self.subTest(name=name):
                matrix = MagicMock()
                matrix.__matmul__.return_value.sum.return_value = matrix_sum
                array = SimpleNamespace(reshape=Mock(return_value=matrix))
                squared = SimpleNamespace(sum=Mock(return_value=square_sum))
                numpy = SimpleNamespace(
                    __version__=numpy_version, int64=object(),
                    arange=Mock(return_value=array), square=Mock(return_value=squared),
                )
                fake_sys = SimpleNamespace(version_info=python_version, version=str(python_version))
                stdout = io.StringIO()
                # A pandas import must fail even though the selected image contains it.
                with patch.dict(sys.modules, {'sys': fake_sys, 'numpy': numpy, 'pandas': None}), \
                        contextlib.redirect_stdout(stdout):
                    if name == 'valid':
                        exec(workload('numpy-script')[0][-1], {})
                    else:
                        with self.assertRaises(AssertionError):
                            exec(workload('numpy-script')[0][-1], {})
                if name == 'valid':
                    self.assertEqual(stdout.getvalue().encode(), NUMPY_MARKER)
                    numpy.arange.assert_called_once_with(16, dtype=numpy.int64)
                    array.reshape.assert_called_once_with(4, 4)
                    numpy.square.assert_called_once_with(matrix)
                    squared.sum.assert_called_once_with()
                    matrix.__matmul__.assert_called_once_with(matrix.T)
                    matrix.__matmul__.return_value.sum.assert_called_once_with()
                    rows = [list(range(i, i + 4)) for i in range(0, 16, 4)]
                    self.assertEqual(sum(x * x for row in rows for x in row), square_sum)
                    self.assertEqual(sum(sum(a * b for a, b in zip(left, right))
                                         for left in rows for right in rows), matrix_sum)
                else:
                    self.assertEqual(stdout.getvalue(), '')

    def test_numpy_cli_accepts_separate_binary_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'new-cohort'
            binary_dir = Path('target/lazy-torch-build/release').resolve()
            argv = ['lazy_startup.py', '--workload', 'numpy-script', '--output', str(output),
                    '--binary-dir', str(binary_dir)]
            # Stop at artifact hashing, before subprocesses or service preparation.
            with patch.object(sys, 'argv', argv), \
                    patch('lazy_startup.sha', side_effect=['binary-hash', RuntimeError('stop before services')]) as hashed:
                with self.assertRaisesRegex(RuntimeError, 'stop before services'):
                    main()
            self.assertEqual(hashed.call_args_list, [call(binary_dir / 'pvisor'), call(binary_dir / 'pvisor-cache')])

    def test_numpy_cli_cache_binary_override_preserves_launcher(self):
        root = Path(__file__).resolve().parents[2]
        cache_binary = Path('target/lazy-torch-build/release/pvisor-cache')
        for binary_dir in (None, Path('target/release')):
            with self.subTest(binary_dir=binary_dir), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / 'new-cohort'
                argv = ['lazy_startup.py', '--workload', 'numpy-script', '--output', str(output),
                        '--cache-binary', str(cache_binary)]
                if binary_dir is not None:
                    argv.extend(['--binary-dir', str(binary_dir)])
                launcher_dir = binary_dir.resolve() if binary_dir is not None else root / 'target/release'
                # Hash only the selected artifacts; never launch or disturb a Host listener.
                with patch.object(sys, 'argv', argv), \
                        patch('lazy_startup.sha', side_effect=['binary-hash', RuntimeError('stop before services')]) as hashed:
                    with self.assertRaisesRegex(RuntimeError, 'stop before services'):
                        main()
                self.assertEqual(hashed.call_args_list, [
                    call(launcher_dir / 'pvisor'), call(cache_binary.resolve()),
                ])

    def test_timed_default_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            result = timed([sys.executable, '-c', f'import sys; sys.stdout.buffer.write({MARKER!r})'],
                           folder, os.environ.copy(), folder, timeout=5)
            self.assertGreaterEqual(result['ready_ms'], 0)
            self.assertGreaterEqual(result['completion_ms'], result['ready_ms'])
            self.assertEqual((folder / 'stdout.log').read_bytes(), MARKER)

    def test_timed_requires_unique_selected_stdout_marker_and_success(self):
        custom = b'CUSTOM_READY\n'
        for marker in (NUMPY_MARKER, TORCH_MARKER, custom):
            cases = (
                ('valid', marker, b'', 0),
                ('missing', b'no ready output\n', b'', 0),
                ('wrong marker', MARKER, b'', 0),
                ('duplicate', marker * 2, b'', 0),
                ('stderr only', b'', marker, 0),
                ('failed exit', marker, b'', 7),
            )
            for name, stdout, stderr, code in cases:
                with self.subTest(marker=marker, name=name), tempfile.TemporaryDirectory() as directory:
                    folder = Path(directory)
                    script = (f'import sys; sys.stdout.buffer.write({stdout!r}); '
                              f'sys.stderr.buffer.write({stderr!r}); sys.exit({code})')
                    argv = [sys.executable, '-c', script]
                    if name == 'valid':
                        result = timed(argv, folder, os.environ.copy(), folder, timeout=5, marker=marker)
                        self.assertGreaterEqual(result['completion_ms'], result['ready_ms'])
                    else:
                        with self.assertRaisesRegex(RuntimeError, 'workload failed'):
                            timed(argv, folder, os.environ.copy(), folder, timeout=5, marker=marker)
                    self.assertEqual((folder / 'stdout.log').read_bytes(), stdout)
                    self.assertEqual((folder / 'stderr.log').read_bytes(), stderr)

    def test_blob_bytes_counts_unique_digests_including_config(self):
        config = dict(digest='sha256:config', size=10)
        layer = dict(digest='sha256:layer', size=100)
        empty = dict(digest='sha256:empty', size=20)
        self.assertEqual(blob_bytes(dict(config=config, layers=[])), 10)
        self.assertEqual(blob_bytes(dict(config=config, layers=[layer, empty, dict(empty), dict(layer)])), 130)
        self.assertEqual(blob_bytes(dict(config=config, layers=[layer, dict(config)])), 110)


if __name__ == '__main__':
    unittest.main()
