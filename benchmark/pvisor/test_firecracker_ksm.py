"""Conventional tests; no VM, KSM writes or administrator approval required."""
import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location('firecracker_ksm', Path(__file__).with_name('firecracker_ksm.py'))
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)


def smaps(size=256 * 1024 * 1024, flags='rd wr mr mw me ac sd', path=''):
    return (f'10000000-{0x10000000 + size:x} rw-p 00000000 00:00 0{path}\n'
            f'Size: {size // 1024} kB\nRss: 100000 kB\nPss: 99999 kB\nKSM: 0 kB\nVmFlags: {flags}\n')


class PatternTests(unittest.TestCase):
    def test_repeated(self):
        self.assertEqual(f.page('repeated', 1, 0, 0), bytes(range(256)) * 16)
        self.assertEqual(f.page('repeated', 1, 100, 0), f.page('repeated', 4, 0, 0))

    def test_random_shared_and_unique(self):
        self.assertEqual(f.page('random-shared', 1, 123, 0), f.page('random-shared', 4, 123, 0))
        self.assertNotEqual(f.page('random-unique', 1, 123, 0), f.page('random-unique', 4, 123, 0))
        self.assertNotEqual(f.page('random-shared', 1, 123, 0), f.page('random-shared', 1, 124, 0))

    def test_exact_lcg_first_word(self):
        x = f.SEED ^ ((3 * 0xd1b54a32d192ed03) & f.MASK) ^ ((123 * 0x9e3779b97f4a7c15) & f.MASK)
        x = (x * 6364136223846793005 + 1442695040888963407) & f.MASK
        self.assertEqual(f.page('random-unique', 3, 123, 0)[:8], struct.pack('<Q', x))

    def test_mutation_boundary_and_peer_isolation(self):
        boundary = (f.SIZE // f.PAGE) // 4
        self.assertNotEqual(f.page('repeated', 1, boundary - 1, 25), f.page('repeated', 1, boundary - 1, 0))
        self.assertEqual(f.page('repeated', 1, boundary, 25), f.page('repeated', 1, boundary, 0))
        for pattern in f.PATTERNS:
            self.assertNotEqual(f.page(pattern, 1, 0, 25), f.page(pattern, 2, 0, 25))
            self.assertEqual(f.page(pattern, 1, 0, 25), f.page(pattern, 1, 0, 100))


class EvidenceTests(unittest.TestCase):
    def test_ram_not_whole_process(self):
        result = f.ram_vmas(smaps() + '30000000-30001000 rw-p 00000000 00:00 0\nSize: 4 kB\nPss: 4 kB\n')
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]['fields']['Pss'], '99999 kB')
        self.assertNotIn('mg', result[0]['fields']['VmFlags'].split())

    def test_reject_ambiguous_file_or_wrong_size(self):
        for text in (smaps() + smaps(), smaps(path=' /file'), smaps(size=128 * 1024 * 1024)):
            with self.assertRaises(ValueError):
                f.ram_vmas(text)

    def test_ksm_readonly_guard(self):
        good = dict(run='1\n', pages_to_scan='100\n', sleep_millisecs='20\n')
        with patch.object(f, 'ksm', return_value=good):
            self.assertEqual(f.scanner_guard(), good)
        for key in good:
            with patch.object(f, 'ksm', return_value=good | {key: '0\n'}):
                with self.assertRaises(ValueError):
                    f.scanner_guard()

    def test_save_digest(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / 'value.json'
            f.save(p, {'status': 'failed'})
            self.assertEqual(f.digest(p), hashlib.sha256(p.read_bytes()).hexdigest())
            self.assertFalse(p.with_suffix('.json.tmp').exists())

    def test_api_persistent_connection_content_length(self):
        sock = MagicMock()
        sock.__enter__.return_value = sock
        sock.recv.side_effect = [b'HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\n\r\n{}', TimeoutError('must not wait for EOF')]
        with patch.object(f.socket, 'socket', return_value=sock):
            response = f.api('/not-used', 'GET', '/machine-config')
        self.assertEqual(response['status'], 400)
        self.assertEqual(sock.recv.call_count, 1)

    def test_api_fragmented_response(self):
        sock = MagicMock()
        sock.__enter__.return_value = sock
        sock.recv.side_effect = [b'HTTP/1.1 200 OK\r\nContent-Length: 2\r\n', b'\r\n{', b'}']
        with patch.object(f.socket, 'socket', return_value=sock):
            response = f.api('/not-used', 'GET', '/machine-config')
        self.assertEqual(response['status'], 200)
        self.assertEqual(sock.recv.call_count, 3)

    def test_worker_checks_every_byte_and_sha(self):
        self.assertIn('full-byte mismatch page', f.WORKER)
        self.assertIn('sha(data)', f.WORKER)
        self.assertIn('assert_eq!(requested,percent)', f.WORKER)


if __name__ == '__main__':
    unittest.main()
