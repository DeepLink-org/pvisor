#!/usr/bin/env python3
"""Build nextest binaries and sign native executor tests for macOS HVF access."""

import json
import subprocess
import sys
from pathlib import Path


def main():
    root = Path(__file__).resolve().parents[1]
    result = subprocess.run(
        ["cargo", "nextest", "list", "--locked", "--message-format", "json", *sys.argv[1:]],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    for suite in json.loads(result.stdout)["rust-suites"].values():
        if suite["package-name"] in {"pvisor-vm", "pvisor", "nativepvisor"}:
            subprocess.run(
                [
                    "codesign",
                    "--force",
                    "--sign",
                    "-",
                    "--entitlements",
                    str(root / "crates/pvisor/macos-hypervisor.entitlements"),
                    suite["binary-path"],
                ],
                check=True,
            )


if __name__ == "__main__":
    main()
