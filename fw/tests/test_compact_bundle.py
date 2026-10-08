import ctypes
import importlib.util
import io
import shutil
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('bin2cbundle', SOURCE / 'bin2cbundle.py')
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class CompactBundleTests(unittest.TestCase):
    def test_padding_roundtrip(self):
        for data in (b'', bytes(range(256)), b'\0' * 65536,
                     b'\xcc' * 256, b'\xcc' * 255,
                     b'code' + b'\0' * 65536 + b'\xcc' * 65536 + b'end'):
            payload, chunks = bundle.compact_chunks(data)
            restored = b''.join(payload[source:source+length] if fill < 0
                                else bytes([fill]) * length
                                for length, source, fill in chunks)
            self.assertEqual(restored, data)
        payload, _ = bundle.compact_chunks(b'\0' * 65536 + b'\xcc' * 65536)
        self.assertEqual(payload, b'')

    def test_compiled_abi_matches_flat_image(self):
        if not shutil.which('cc'):
            self.skipTest('C compiler required')
        # Two PT_LOAD segments separated by an aligned hole, including trap bytes.
        segments = (b'entry' + b'\xcc' * 4096 + b'\x90\xc3', b'data\0\xff' * 49)
        addresses = (0x1000000, 0x1200000)
        elf = bytearray(4096)
        elf[:64] = struct.pack('<16sHHIQQQIHHHHHH',
            b'\x7fELF\x02\x01\x01' + bytes(9), 2, 62, 1,
            0xffffffff81000000, 64, 0, 0, 64, 56, 2, 64, 0, 0)
        offset = len(elf)
        for index, (data, address) in enumerate(zip(segments, addresses)):
            elf[64+56*index:120+56*index] = struct.pack('<IIQQQQQQ',
                1, 7, offset, 0xffffffff80000000 + address, address,
                len(data), len(data)+4096, 4096)
            elf.extend(data)
            offset += len(data)
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            flat = io.StringIO()
            bundle.write_header(flat, 'KERNEL', 65536)
            load, entry = bundle.write_elf_cbundle(io.BytesIO(elf), flat, 65536)
            bundle.write_footer_kernel(flat, load, entry)
            compact = io.StringIO()
            bundle.write_compact_elf_cbundle(io.BytesIO(elf), compact, 65536)
            results = []
            for name, source in (('flat', flat.getvalue()), ('compact', compact.getvalue())):
                c = directory / (name + '.c')
                so = directory / (name + '.so')
                c.write_text(source)
                subprocess.run(['cc', '-shared', '-fPIC', '-Wall', '-Wextra',
                                *(['-Werror'] if name == 'compact' else []),
                                '-DABI_VERSION=5', '-o', str(so), str(c)], check=True)
                lib = ctypes.CDLL(str(so))
                get = lib.krunfw_get_kernel
                get.restype = ctypes.c_void_p
                get.argtypes = [ctypes.POINTER(ctypes.c_size_t)] * 3
                address, entrypoint, size = (ctypes.c_size_t() for _ in range(3))
                pointer = get(ctypes.byref(address), ctypes.byref(entrypoint), ctypes.byref(size))
                self.assertEqual(pointer % 65536, 0)
                self.assertEqual(lib.krunfw_get_version(), 5)
                results.append((address.value, entrypoint.value,
                                ctypes.string_at(pointer, size.value)))
                self.assertEqual(pointer, get(ctypes.byref(address), ctypes.byref(entrypoint), ctypes.byref(size)))
                # The VM needs writable RAM, as in the original ABI.
                first = ctypes.c_ubyte.from_address(pointer)
                old = first.value
                first.value ^= 0xff
                self.assertEqual(first.value, old ^ 0xff)
                first.value = old
            self.assertEqual(results[0], results[1])
            self.assertLess((directory / 'compact.so').stat().st_size,
                            (directory / 'flat.so').stat().st_size // 4)


if __name__ == '__main__':
    unittest.main()
