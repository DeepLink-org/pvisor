#!/usr/bin/env python3
"""Publish immutable releases; retry only files whose existing bytes agree."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch_json(url: str, *, token: str | None = None) -> dict | None:
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def require_digest(path: Path, digest: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{64}", digest) or sha256(path) != digest:
        raise RuntimeError(f"immutable release mismatch: {path.name}; publish a new version")


def pypi_existing(directory: Path, version: str) -> list[Path]:
    """Verify all overlapping wheels before returning any files to skip."""
    release = fetch_json(
        f"https://pypi.org/pypi/pvisor-sandbox/{urllib.parse.quote(version, safe='')}/json"
    )
    remote = {
        item["filename"]: item["digests"]["sha256"] for item in (release or {}).get("urls", [])
    }
    existing = []
    for path in sorted(directory.glob("*.whl")):
        if path.name in remote:
            require_digest(path, remote[path.name])
            existing.append(path)
    return existing


def github_publish(directory: Path, version: str, repository: str, tag: str) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("repository must be owner/name")
    if tag != f"v{version}":
        raise ValueError("tag must match the release version")
    paths = sorted(
        [
            *directory.glob("*.whl"),
            *directory.glob("pvisor-*.tar.gz"),
            *[
                directory / name
                for name in ("SHA256SUMS", "pvisor.rb")
                if (directory / name).is_file()
            ],
        ]
    )
    if not paths or any(not re.fullmatch(r"[A-Za-z0-9_.-]+", path.name) for path in paths):
        raise ValueError("invalid or empty release artifact set")
    # Check both channels before the first publication mutation.
    pypi_existing(directory, version)
    url = f"https://api.github.com/repos/{repository}/releases/tags/{urllib.parse.quote(tag, safe='')}"
    release = fetch_json(url, token=os.environ.get("GH_TOKEN"))
    assets = {asset["name"]: asset for asset in (release or {}).get("assets", [])}
    missing = []
    for path in paths:
        asset = assets.get(path.name)
        if asset is None:
            missing.append(path)
            continue
        digest = asset.get("digest")
        if digest and digest.startswith("sha256:"):
            require_digest(path, digest.removeprefix("sha256:"))
        else:
            # Older assets may lack the API digest. Compare their actual bytes.
            with tempfile.TemporaryDirectory(prefix="pvisor-release-compare-") as temporary:
                subprocess.run(
                    [
                        "gh",
                        "release",
                        "download",
                        tag,
                        "--repo",
                        repository,
                        "--pattern",
                        path.name,
                        "--dir",
                        temporary,
                    ],
                    check=True,
                    timeout=300,
                )
                require_digest(path, sha256(Path(temporary) / path.name))
    if release is None:
        subprocess.run(
            [
                "gh",
                "release",
                "create",
                tag,
                *map(str, paths),
                "--repo",
                repository,
                "--verify-tag",
                "--generate-notes",
                "--notes",
                f"Install from PyPI: `python -m pip install pvisor-sandbox=={version}`.\n\n"
                "Native downloads need neither Python nor Rust. Download the matching "
                f"`pvisor-{version}-linux-x86_64.tar.gz` or "
                f"`pvisor-{version}-macos-arm64.tar.gz` and `SHA256SUMS`. "
                "Verify the archive SHA-256, extract it, then add its `bin` directory "
                "to PATH. Keep the CLI, companions, daemon and firmware together.\n\n"
                "The generated Homebrew formula is attached as `pvisor.rb`; the "
                "project tap is updated when configured and after Homebrew acceptance.\n\n"
                "Corresponding firmware sources are attached. See the "
                "[installation guide](https://deeplink-org.github.io/pvisor/start/installation/) "
                "for platform and virtualization requirements.",
                "--latest",
                "--title",
                f"pVisor {version}",
            ],
            check=True,
            timeout=300,
        )
    elif missing:
        # No clobber: a competing upload must fail, never replace a winner.
        subprocess.run(
            ["gh", "release", "upload", tag, *map(str, missing), "--repo", repository],
            check=True,
            timeout=300,
        )


def prepare_pypi(directory: Path, version: str) -> bool:
    """Remove only checksum-identical wheel copies from this job's staging area."""
    existing = pypi_existing(directory, version)
    for path in existing:
        path.unlink()
    return bool(list(directory.glob("*.whl")))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("channel", choices=("github", "pypi"))
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository")
    parser.add_argument("--tag")
    args = parser.parse_args()
    if args.channel == "github":
        if not args.repository or not args.tag:
            parser.error("github requires --repository and --tag")
        github_publish(args.directory, args.version, args.repository, args.tag)
    else:
        pending = prepare_pypi(args.directory, args.version)
        if output := os.environ.get("GITHUB_OUTPUT"):
            with open(output, "a", encoding="utf-8") as stream:
                stream.write(f"pending={str(pending).lower()}\n")
        print(f"pending={str(pending).lower()}")


if __name__ == "__main__":
    main()
