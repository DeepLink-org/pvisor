"""Common dlopen/full-content/COW oracle for virtio-fs and virtio-blk guests."""

import ctypes
import hashlib
import os
import sys

pattern, identity, mode = sys.argv[1], int(sys.argv[2]), sys.argv[3]
assert pattern == "random-shared" and mode in ("buffered", "dax", "rootfs", "block")
c = ctypes.CDLL(None, use_errno=True)


def mount(source, target, kind, flags, data):
    if c.mount(source, target, kind, ctypes.c_ulong(flags), data):
        raise OSError(ctypes.get_errno(), target)


if mode in ("buffered", "dax"):
    mount(b"tmpfs", b"/tmp", b"tmpfs", 0, None)
    os.mkdir("/tmp/libraries")
    mount(
        b"bench-libraries",
        b"/tmp/libraries",
        b"virtiofs",
        1,
        b"dax=inode" if mode == "dax" else b"dax=never",
    )
    library = b"/tmp/libraries/libpayload.so"
else:
    library = b"/libraries/libpayload.so"
stat = ctypes.create_string_buffer(256)
assert c.statx(-100, library, 0, 0x7FF, stat) == 0
attributes = int.from_bytes(stat.raw[8:16], "little")
attribute_mask = int.from_bytes(stat.raw[56:64], "little")
if mode == "dax":
    assert attribute_mask & 0x200000 and attributes & 0x200000
else:
    assert not attributes & 0x200000
print(
    "LIBRARY_MODE",
    mode,
    "statx_attributes",
    attributes,
    "statx_attributes_mask",
    attribute_mask,
    flush=True,
)
lib = ctypes.CDLL(os.fsdecode(library))
lib.payload_addr.restype = ctypes.c_void_p
lib.payload_length.restype = ctypes.c_size_t
lib.payload_mutate.argtypes = [ctypes.c_uint64, ctypes.c_uint]
size = lib.payload_length()
assert size == 64 * 1024 * 1024
address = lib.payload_addr()
assert address % 4096 == 0
payload = (ctypes.c_ubyte * size).from_address(address)
percent = 0


def ack(token):
    digest = hashlib.sha256(payload).hexdigest()
    print("F4K", token, identity, percent, digest, flush=True)


ack("ready")
for line in sys.stdin:
    token, op, wanted = line.split()
    wanted = int(wanted)
    if op == "mutate":
        assert wanted in (25, 100) and wanted >= percent
        assert lib.payload_mutate(identity, wanted) == 0
        percent = wanted
    else:
        assert op in ("read", "exit") and wanted == percent
    ack(token)
    if op == "exit":
        break
if mode == "block":
    # Firecracker's host reaps PID 1 after the explicit final acknowledgement.
    while True:
        c.pause()
