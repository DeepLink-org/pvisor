"""Exercise the shell installer against the exact candidate release files."""

import argparse
import os
import platform
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = {("Linux", "x86_64"): "linux-x86_64", ("Darwin", "arm64"): "macos-arm64"}


def smoke(artifacts: Path, channel: str, version: str) -> None:
    host = (platform.system(), platform.machine())
    if host not in PLATFORMS:
        raise ValueError(f"unsupported installer acceptance host: {' '.join(host)}")
    nightly = channel == "nightly"
    tag = "nightly" if nightly else f"v{version}"
    index = "download/nightly/SHA256SUMS" if nightly else "latest/download/SHA256SUMS"
    archive = f"pvisor-{version}-{PLATFORMS[host]}.tar.gz"

    with tempfile.TemporaryDirectory(prefix="pvisor-install-smoke-") as temporary:
        root = Path(temporary).resolve()
        home = root / "home"
        home.mkdir()
        curl = root / "curl"
        curl.write_text(
            """#!/bin/sh
set -eu
while [ "$#" -gt 0 ] && [ "$1" != --output ]; do shift; done
[ "$#" -eq 3 ]
base=https://github.com/DeepLink-org/pvisor/releases
case "$3" in
    "$base/$INDEX") file=SHA256SUMS ;;
    "$base/$ARCHIVE_URL") file=$ARCHIVE ;;
    *) printf 'unexpected release asset URL: %s\\n' "$3" >&2; exit 1 ;;
esac
cp "$ARTIFACTS/$file" "$2"
"""
        )
        curl.chmod(0o755)
        env = {
            "HOME": str(home),
            "PATH": f"{root}{os.pathsep}{os.defpath}",
            "PVISOR_RUN_HOME": str(root / "runs"),
            "ARTIFACTS": str(artifacts.resolve()),
            "INDEX": index,
            "ARCHIVE_URL": f"download/{tag}/{archive}",
            "ARCHIVE": archive,
        }
        subprocess.run(
            ["sh", "-s", "--", *(["--nightly"] if nightly else [])],
            input=(ROOT / "scripts/install.sh").read_bytes(),
            cwd=root,
            env=env,
            check=True,
            timeout=90,
        )
        slot = "cli-nightly" if nightly else "cli"
        bin_dir = home / ".local/share/pvisor" / slot / "current/bin"
        for args, expected in (
            (("--version",), f"pvisor {version}"),
            (("replay", "--version"), f"pvisor-replay {version}"),
            (("daemon", "--version"), f"pvisor-daemon {version}"),
            (("spawn", "--executor", "host", "--", "/bin/echo", "hello"), "hello"),
        ):
            result = subprocess.run(
                [bin_dir / "pvisor", *args],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                timeout=60,
            )
            if result.returncode != 0 or result.stdout.strip() != expected:
                raise RuntimeError(
                    f"{' '.join(args)}: expected {expected!r}; got exit {result.returncode}, "
                    f"stdout {result.stdout!r}, stderr {result.stderr!r}"
                )
        print(
            f"PASS: {channel} {version} on {PLATFORMS[host]} (versions, companions and Host task)"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("--channel", required=True, choices=("stable", "nightly"))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    smoke(args.artifacts, args.channel, args.version)
