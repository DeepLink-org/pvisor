#!/usr/bin/env python3
"""Extract a libkrunfw kernel bundle on a host that can load the firmware."""

from __future__ import annotations

import argparse
import ctypes
import json
from pathlib import Path


def extract(source: Path, destination: Path) -> None:
    library = ctypes.CDLL(str(source))
    get_kernel = library.krunfw_get_kernel
    get_kernel.argtypes = [
        ctypes.POINTER(ctypes.c_uint64),
        ctypes.POINTER(ctypes.c_uint64),
        ctypes.POINTER(ctypes.c_size_t),
    ]
    get_kernel.restype = ctypes.c_void_p

    guest_addr = ctypes.c_uint64()
    entry_addr = ctypes.c_uint64()
    size = ctypes.c_size_t()
    pointer = get_kernel(
        ctypes.byref(guest_addr),
        ctypes.byref(entry_addr),
        ctypes.byref(size),
    )
    if not pointer or not size.value:
        raise RuntimeError("krunfw_get_kernel returned an empty kernel bundle")

    destination.mkdir(parents=True, exist_ok=True)
    (destination / "kernel.bin").write_bytes(
        ctypes.string_at(pointer, size.value)
    )
    (destination / "kernel.json").write_text(
        json.dumps(
            {
                "guest_addr": guest_addr.value,
                "entry_addr": entry_addr.value,
                "size": size.value,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("libkrunfw", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    extract(args.libkrunfw.resolve(), args.destination.resolve())


if __name__ == "__main__":
    main()
