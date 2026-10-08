#!/usr/bin/env python3
"""Build the Linux daemon with the same native VM pipeline as the CLI."""

from __future__ import annotations

import argparse
import os
from pathlib import Path

from stage_wheel_binaries import ROOT, BuildOptions, _build_component, copy_artifact


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("dev", "release", "performance"), default="dev")
    parser.add_argument("--target-dir", default=os.getenv("CARGO_TARGET_DIR", str(ROOT / "target")))
    args = parser.parse_args()
    options = BuildOptions(
        target="x86_64-unknown-linux-musl",
        profile=args.profile,
        target_dir=args.target_dir,
    )
    source = _build_component(options, daemon=True)["pvisor-daemon"]
    destination = (
        Path(args.target_dir)
        / ("debug" if args.profile == "dev" else args.profile)
        / "pvisor-daemon"
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    if source.resolve() != destination.resolve():
        copy_artifact(source, destination)


if __name__ == "__main__":
    main()
