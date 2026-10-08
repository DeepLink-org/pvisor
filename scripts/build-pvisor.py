#!/usr/bin/env python3
"""Build the CLI, preserving the target/<profile>/pvisor task interface."""

import argparse
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "packaging"))
from stage_wheel_binaries import ROOT, BuildOptions, _build, copy_artifact


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", default="release")
    parser.add_argument("--target-dir", default=os.getenv("CARGO_TARGET_DIR", str(ROOT / "target")))
    parser.add_argument("--target", default=os.getenv("CARGO_BUILD_TARGET"))
    parser.add_argument("--shim-vm", action="store_true")
    args = parser.parse_args()
    options = BuildOptions(
        target="x86_64-unknown-linux-musl" if args.shim_vm else args.target,
        profile=args.profile,
        target_dir=args.target_dir,
    )
    artifacts = _build(options, shim_vm=args.shim_vm)
    for name, source in artifacts.items():
        destination = (
            Path(args.target_dir) / ("debug" if args.profile == "dev" else args.profile) / name
        )
        destination.parent.mkdir(parents=True, exist_ok=True)
        if source.resolve() != destination.resolve():
            copy_artifact(source, destination)
    if sys.platform == "darwin":
        directory = artifacts["pvisor"].parent
        for name in ("libkrunfw.5.dylib", "libkrunfw.SOURCE"):
            source = directory / name
            destination = (
                Path(args.target_dir) / ("debug" if args.profile == "dev" else args.profile) / name
            )
            if source.resolve() != destination.resolve():
                copy_artifact(source, destination)


if __name__ == "__main__":
    main()
