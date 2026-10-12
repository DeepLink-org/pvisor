"""Behavioral checks for token rejection, process cleanup, and comparison failure."""
import importlib.util
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path

from prepare_replay_rootfs import restore_virtual_identity
from trajectory_benchmark import comparable_output, historical_output

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location('trajectory_worker', HERE / 'fixtures/trajectory_worker.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class WorkerContracts(unittest.TestCase):
    def test_output_comparison_preserves_failures_and_requires_warning_evidence(self):
        self.assertEqual(comparable_output(b'random seed: 1\nin 1.2 seconds\n', 7),
                         comparable_output(b'random seed: 2\nin 9.8 seconds\n', 7))
        with self.assertRaises(ValueError):
            historical_output(b'81 passed, 1 failed\n', b'82 passed\n', 7, [])
        with self.assertRaises(ValueError):
            historical_output(b'', b'unexpected warning\n', 6, ['unexpected warning'])
        with self.assertRaises(ValueError):
            historical_output(b'', b'RuntimeError\n', 1, [])

    @unittest.skipUnless(os.uname().sysname == 'Linux', 'Linux virtual identity xattr')
    def test_opaque_metadata_preserves_new_layer_and_original_modes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            blobs = root / 'oci/blobs/sha256'
            blobs.mkdir(parents=True)
            for name, entries in [('lower', [('d', 25, True), ('d/old', 26, False)]),
                                  ('upper', [('d/new', 27, False), ('d/.wh..wh..opq', 0, False)])]:
                with tarfile.open(blobs / name, 'w') as archive:
                    for path, uid, directory in entries:
                        info = tarfile.TarInfo(path)
                        info.uid, info.gid, info.mode = uid, 31, 0o444 if not directory else 0o755
                        info.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
                        archive.addfile(info)
            (blobs / 'manifest').write_text(json.dumps({'layers': [{'digest': 'sha256:lower'}, {'digest': 'sha256:upper'}]}))
            (root / 'oci/index.json').write_text(json.dumps({'manifests': [{'digest': 'sha256:manifest'}]}))
            final = root / 'rootfs'
            (final / 'd').mkdir(parents=True)
            (final / 'd/new').write_text('umoci extracted bytes')
            (final / 'd/new').chmod(0o444)
            restore_virtual_identity(root / 'oci', final)
            self.assertEqual(os.getxattr(final / 'd', 'user.containers.override_stat'), b'25:31:755')
            self.assertEqual(os.getxattr(final / 'd/new', 'user.containers.override_stat'), b'27:31:444')
            self.assertEqual((final / 'd/new').stat().st_mode & 0o777, 0o444)
            self.assertEqual((final / 'd/new').read_text(), 'umoci extracted bytes')

    def test_rejects_old_token_and_out_of_order_release(self):
        identity = dict(schema=worker.SCHEMA, run_token='run', execution_token='process', pid=5)
        valid = dict(identity, kind='go', step=17, next_step=18)
        self.assertTrue(worker.matching_go(valid, identity, 17))
        for key, value in [('run_token', 'old'), ('execution_token', 'restarted'),
                           ('step', 1), ('next_step', 19), ('pid', 6), ('kind', 'ready')]:
            with self.subTest(key=key):
                self.assertFalse(worker.matching_go(dict(valid, **{key: value}), identity, 17))

    def test_timeout_kills_group_and_preserves_output(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'stdout'
            result = worker.execute('echo before-timeout; sleep 30', output, timeout=.1, cwd=temp)
            self.assertTrue(result['timed_out'])
            self.assertNotEqual(result['returncode'], 0)
            self.assertEqual(output.read_bytes(), b'before-timeout\n')

    def test_foreground_nonzero_is_a_result(self):
        with tempfile.TemporaryDirectory() as temp:
            result = worker.execute('echo expected-error >&2; exit 19', Path(temp) / 'stdout', cwd=temp)
            self.assertEqual(result['returncode'], 19)
            self.assertFalse(result['timed_out'])

    def test_atomic_messages_replace_complete_contents(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'go.json'
            worker.publish(path, {'token': 'bad'})
            worker.publish(path, {'token': 'good', 'step': 17})
            self.assertEqual(json.loads(path.read_text()), {'token': 'good', 'step': 17})
            self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ['go.json'])


if __name__ == '__main__':
    unittest.main()
