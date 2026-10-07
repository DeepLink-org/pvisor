#!/usr/bin/env python3
"""Build pVisor's in-tree firmware and optionally export its source materials."""

import argparse
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "packaging"))
import firmware
from stage_wheel_binaries import BuildOptions, _build_firmware, _host_target


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default=os.getenv("CARGO_BUILD_TARGET"))
    parser.add_argument("--target-dir", default=os.getenv("CARGO_TARGET_DIR"))
    parser.add_argument("--jobs", "-j")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--source-output", type=Path)
    parser.add_argument("--source-archive", type=Path)
    args = parser.parse_args()
    options = BuildOptions(
        target=args.target or _host_target(),
        target_dir=args.target_dir,
        jobs=args.jobs,
        offline=args.offline,
    )
    source = _build_firmware(options, "")
    if args.source_output:
        args.source_output.parent.mkdir(parents=True, exist_ok=True)
        args.source_output.write_text(firmware.source_record(source))
    if args.source_archive:
        firmware.source_archive(source, args.source_archive)
    print(source)


if __name__ == "__main__":
    main()
