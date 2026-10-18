"""Read-only, conservative evidence of target library pages shared across VMs.

Only present/file pagemap bits are used, never PFNs. A mapping is evidence only
if its inode/offset and file PTE survive rechecking and its canonical bytes match
one unique prepared payload page in full. Counts describe observed file sharing,
not net saved memory. Run outside the product cgroup, after timed first readback.
"""

import ctypes
import hashlib
import mmap
import os
from pathlib import Path

PAGE = 4096
PRESENT_FILE = (1 << 63) | (1 << 61)


def identity(path):
    stat = Path(path).stat()
    return stat.st_dev, stat.st_ino


def mapping_identity(source, metadata):
    """Calibrate maps' device/inode/path using a mapping of the pinned FD.

    Some filesystems expose different device numbers through stat and maps.
    Never guess that translation or fall back to inode-only matching. The
    observer maps one page without touching its bytes or consulting map_files.
    """
    if metadata.st_size <= 0:
        raise ValueError("canonical backing must be nonempty")
    with mmap.mmap(source.fileno(), min(PAGE, metadata.st_size), access=mmap.ACCESS_COPY) as view:
        probe = ctypes.c_ubyte.from_buffer(view)
        address = ctypes.addressof(probe)
        del probe
        with open("/proc/self/maps") as maps:
            for line in maps:
                words = line.split(maxsplit=5)
                low, high = (int(x, 16) for x in words[0].split("-"))
                if not low <= address < high:
                    continue
                if (
                    len(words) != 6
                    or int(words[4]) != metadata.st_ino
                    or int(words[2], 16) + address - low != 0
                ):
                    raise ValueError("canonical FD mapping identity is ambiguous")
                major, minor = (int(x, 16) for x in words[3].split(":"))
                return os.makedev(major, minor), int(words[4]), words[5].rstrip("\n")
    raise ValueError("canonical FD mapping missing from proc maps")


def present_file_mappings(pid, file_identity, mapping_path=None):
    result = {}
    with (
        open(f"/proc/{pid}/maps") as maps,
        open(f"/proc/{pid}/pagemap", "rb", buffering=0) as flags,
    ):
        for line in maps:
            words = line.split(maxsplit=5)
            major, minor = (int(x, 16) for x in words[3].split(":"))
            if (os.makedev(major, minor), int(words[4])) != file_identity:
                continue
            if mapping_path is not None and (
                len(words) != 6 or words[5].rstrip("\n") != mapping_path
            ):
                continue
            low, high = (int(x, 16) for x in words[0].split("-"))
            offset = int(words[2], 16)
            # Each proc read is bounded even for a large contiguous DAX mapping.
            for start in range(low, high, PAGE * 512):
                count = min(512, (high - start) // PAGE)
                raw = os.pread(flags.fileno(), count * 8, start // PAGE * 8)
                if len(raw) != count * 8:
                    raise ValueError("short pagemap observation")
                for index in range(count):
                    bits = int.from_bytes(raw[index * 8 : index * 8 + 8], "little")
                    if bits & PRESENT_FILE == PRESENT_FILE:
                        address = start + index * PAGE
                        result[address] = offset + address - low
    return result


def process_start(pid):
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def observe(pids, backing, payload):
    """Return exact target-page/inode/offset intersections; absent sharing is valid."""
    if not pids or len(set(pids)) != len(pids):
        raise ValueError("distinct live process identities required")
    data = Path(payload).read_bytes()
    if not data or len(data) % PAGE:
        raise ValueError("payload must contain full pages")
    expected = {}
    for index in range(len(data) // PAGE):
        page = data[index * PAGE : (index + 1) * PAGE]
        key = hashlib.sha256(page).digest()
        if key in expected:
            raise ValueError("target payload pages must have unique identities")
        expected[key] = index
    with open(backing, "rb", buffering=0) as source:
        stat = os.fstat(source.fileno())
        device, inode, mapping_path = mapping_identity(source, stat)
        file_id = device, inode
        starts = {pid: process_start(pid) for pid in pids}
        before = {pid: present_file_mappings(pid, file_id, mapping_path) for pid in pids}
        offsets = sorted({offset for row in before.values() for offset in row.values()})
        matched = {}
        for offset in offsets:
            if offset % PAGE or offset < 0 or offset >= stat.st_size:
                raise ValueError("mapping outside canonical backing")
            # ELF metadata can end in a valid partial file page. It cannot be
            # one of the controlled, aligned full payload pages; do not pad it
            # or read beyond EOF merely to construct a possible match.
            if offset + PAGE > stat.st_size:
                continue
            page = os.pread(source.fileno(), PAGE, offset)
            if len(page) != PAGE:
                raise ValueError("short canonical page read")
            index = expected.get(hashlib.sha256(page).digest())
            if index is not None and page == data[index * PAGE : (index + 1) * PAGE]:
                matched[offset] = index
        after = {pid: present_file_mappings(pid, file_id, mapping_path) for pid in pids}
        if any(process_start(pid) != starts[pid] for pid in pids):
            raise ValueError("process identity changed during evidence collection")
        rows, slots = [], []
        for pid in pids:
            observed = sorted(
                {
                    (matched[offset], offset)
                    for address, offset in before[pid].items()
                    if offset in matched and after[pid].get(address) == offset
                }
            )
            slots.append(set(observed))
            rows.append(
                dict(
                    pid=pid,
                    starttime=starts[pid],
                    payload_pages=len({index for index, _ in observed}),
                    payload_index_and_file_offset=[list(x) for x in observed],
                )
            )
        common = sorted(set.intersection(*slots))
        return dict(
            schema="pvisor-library-file-sharing/v2",
            scope="stable observed present file PTEs and full-byte canonical matches of controlled payload content in guest RAM; not guest virtual-address attribution or net savings",
            device=stat.st_dev,
            inode=stat.st_ino,
            payload_sha256=hashlib.sha256(data).hexdigest(),
            maps_device=device,
            mapping_path=mapping_path,
            target_pages=len(data) // PAGE,
            processes=rows,
            shared_pages_all_processes=len({index for index, _ in common}),
            shared_payload_index_and_file_offset=[list(x) for x in common],
        )


def pool_backing(pid):
    """Locate the one physical-pool arena, deduplicating its read/write FDs."""
    found = {}
    for fd in Path(f"/proc/{pid}/fd").iterdir():
        try:
            if "memfd:pvisor-shared-ram" in os.readlink(fd):
                found[identity(fd)] = fd
        except FileNotFoundError:
            continue
    if len(found) != 1:
        raise ValueError("one physical pool arena required")
    return next(iter(found.values()))


def file_mapping_stats(text, file_identity, mapping_path):
    """Retain kernel RSS/PSS for one exact file view; never alias overlay devices."""
    mappings = []
    selected = None
    for line in text.splitlines():
        words = line.split(maxsplit=5)
        if len(words) >= 5 and "-" in words[0] and ":" in words[3]:
            major, minor = (int(x, 16) for x in words[3].split(":"))
            selected = None
            if (
                (os.makedev(major, minor), int(words[4])) == file_identity
                and len(words) == 6
                and words[5] == mapping_path
            ):
                selected = dict(header=line)
                mappings.append(selected)
        elif (
            selected is not None
            and words
            and words[0]
            in (
                "Rss:",
                "Pss:",
                "Shared_Clean:",
                "Shared_Dirty:",
                "Private_Clean:",
                "Private_Dirty:",
            )
        ):
            selected[words[0][:-1].lower() + "_bytes"] = int(words[1].split()[0]) * 1024
    if not mappings or any("rss_bytes" not in row or "pss_bytes" not in row for row in mappings):
        raise ValueError("complete exact-file smaps accounting required")
    rss = sum(row["rss_bytes"] for row in mappings)
    pss = sum(row["pss_bytes"] for row in mappings)
    if not 0 < pss <= rss:
        raise ValueError("invalid exact-file RSS/PSS accounting")
    return dict(
        mappings=mappings,
        rss_bytes=rss,
        pss_bytes=pss,
        scope="kernel smaps for the complete library view, including ELF metadata; PSS has per-VMA KiB rounding",
    )
