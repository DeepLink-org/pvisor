import mmap
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import unittest

from library_evidence import PAGE, observe


@unittest.skipUnless(Path('/proc/self/pagemap').exists(), 'Linux pagemap required')
class EvidenceTests(unittest.TestCase):
    def test_partial_elf_tail_is_not_a_full_target_page(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = root / 'payload'; payload.write_bytes(b'A' * PAGE)
            backing = root / 'library'; backing.write_bytes(b'H' * PAGE + payload.read_bytes() + b'tail')
            with backing.open('rb') as source, mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY) as mapping:
                self.assertEqual(mapping[:], backing.read_bytes())
                result = observe([os.getpid()], backing, payload)
                self.assertEqual(result['shared_payload_index_and_file_offset'], [[0, PAGE]])

    def test_repository_filesystem_uses_the_actual_mapping_identity(self):
        # Btrfs can expose a different device number through stat and maps.
        # Exercise the real repository filesystem, not only /tmp's filesystem.
        parent = Path(__file__).resolve().parent / '.data'
        parent.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as directory:
            root = Path(directory)
            payload = root / 'payload'; payload.write_bytes(b'A' * PAGE + b'B' * PAGE)
            backing = root / 'library'; backing.write_bytes(payload.read_bytes())
            with backing.open('rb') as source, mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY) as mapping:
                self.assertEqual(mapping[:], payload.read_bytes())
                self.assertEqual(observe([os.getpid()], backing, payload)['shared_pages_all_processes'], 2)

    def test_two_process_intersection_excludes_each_private_cow(self):
        script = '''
import mmap, sys
with open(sys.argv[1], 'rb') as source:
    with mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY) as mapping:
        assert mapping[:] == source.read()
        print('ready', flush=True)
        assert sys.stdin.readline() == 'cow\\n'
        mapping[8192] = ord('C')
        print('cow', flush=True)
        sys.stdin.readline()
'''
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = root / 'payload'
            payload.write_bytes(b'A' * PAGE + b'B' * PAGE)
            backing = root / 'library'
            backing.write_bytes(b'H' * PAGE + payload.read_bytes())
            with backing.open('rb') as source, mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY) as mapping:
                self.assertEqual(mapping[:], backing.read_bytes())
                child = subprocess.Popen([sys.executable, '-c', script, str(backing)],
                                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                def wait_line(wanted):
                    self.assertTrue(select.select([child.stdout], [], [], 10)[0], 'child handshake deadline')
                    self.assertEqual(child.stdout.readline(), wanted)
                try:
                    wait_line(b'ready\n')
                    self.assertEqual(observe([os.getpid(), child.pid], backing, payload)['shared_pages_all_processes'], 2)
                    child.stdin.write(b'cow\n'); child.stdin.flush()
                    wait_line(b'cow\n')
                    self.assertEqual(observe([os.getpid(), child.pid], backing, payload)['shared_payload_index_and_file_offset'], [[0, PAGE]])
                    mapping[PAGE] = ord('D')
                    self.assertEqual(observe([os.getpid(), child.pid], backing, payload)['shared_pages_all_processes'], 0)
                    child.stdin.write(b'exit\n'); child.stdin.flush()
                    self.assertEqual(child.wait(timeout=5), 0)
                    self.assertEqual(backing.read_bytes(), b'H' * PAGE + b'A' * PAGE + b'B' * PAGE)
                finally:
                    if child.poll() is None:
                        child.kill(); child.wait(timeout=5)
                    for pipe in (child.stdin, child.stdout, child.stderr):
                        pipe.close()

    def test_exact_file_offsets_and_private_cow(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = root / 'payload'
            payload.write_bytes(b'A' * PAGE + b'B' * PAGE)
            backing = root / 'library'
            backing.write_bytes(b'H' * PAGE + payload.read_bytes())
            with backing.open('rb') as source:
                with mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_COPY) as mapping:
                    self.assertEqual(mapping[:], backing.read_bytes())  # fault all source PTEs
                    result = observe([os.getpid()], backing, payload)
                    self.assertEqual(result['shared_pages_all_processes'], 2)
                    self.assertEqual(result['shared_payload_index_and_file_offset'], [[0, PAGE], [1, 2 * PAGE]])
                    mapping[PAGE] = ord('C')
                    changed = observe([os.getpid()], backing, payload)
                    self.assertEqual(changed['shared_payload_index_and_file_offset'], [[1, 2 * PAGE]])
                    self.assertEqual(backing.read_bytes(), b'H' * PAGE + b'A' * PAGE + b'B' * PAGE)

    def test_rejects_ambiguous_target_and_duplicate_process(self):
        with tempfile.TemporaryDirectory() as directory:
            payload = Path(directory) / 'payload'
            payload.write_bytes(b'A' * PAGE * 2)
            with self.assertRaisesRegex(ValueError, 'distinct'):
                observe([os.getpid()] * 2, payload, payload)
            with self.assertRaisesRegex(ValueError, 'unique'):
                observe([os.getpid()], payload, payload)


if __name__ == '__main__':
    unittest.main()
