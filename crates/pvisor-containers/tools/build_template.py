#!/usr/bin/env python3
"""Build the installation's sparse empty ext4 template on Linux."""
import gzip
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: build_template.py OUTPUT")
    output = Path(sys.argv[1])
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="pvisor-ext4-") as directory:
        image = Path(directory) / "empty.ext4"
        with image.open("wb") as stream:
            stream.truncate(4 << 30)
        env = dict(os.environ, E2FSPROGS_FAKE_TIME="1704067200")
        subprocess.run(
            [
                "mkfs.ext4", "-F", "-q", "-b", "4096", "-U",
                "c396200a-65f7-4781-bbf0-6ac302246225",
                "-O", "^metadata_csum_seed,^orphan_file",
                "-E", "lazy_itable_init=0,lazy_journal_init=0",
                str(image),
            ],
            check=True, env=env,
        )
        with output.open("wb") as target:
            with gzip.GzipFile(fileobj=target, mode="wb", mtime=0) as packed:
                packed.write(b"PVROOT1\0")
                packed.write(struct.pack("<Q", 4 << 30))
                with image.open("rb") as source:
                    offset = 0
                    while chunk := source.read(65536):
                        if chunk.count(b"\0") != len(chunk):
                            packed.write(struct.pack("<QI", offset, len(chunk)))
                            packed.write(chunk)
                        offset += len(chunk)
                packed.write(struct.pack("<QI", (1 << 64) - 1, 0))
        print(f"Created {output}: {output.stat().st_size} bytes, 4 GiB sparse ext4")


if __name__ == "__main__":
    main()
