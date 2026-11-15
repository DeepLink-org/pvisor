#!/usr/bin/env python3
"""Build a fixed official MinIO source release locally; never publish an image."""

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TAG = "RELEASE.2025-09-07T16-13-09Z"
URL = f"https://codeload.github.com/minio/minio/tar.gz/refs/tags/{TAG}"
SOURCE_SHA256 = "c9598dcce3440977e79f787f2ba0e7e4d92c8d556bd51e7cef3785bafd6635f3"


def digest(path):
    with path.open("rb") as stream:
        checksum = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
        return checksum.hexdigest()


def main():
    target = ROOT / "target/compose-acceptance/minio-source"
    target.mkdir(parents=True, exist_ok=True)
    archive = target / f"{TAG}.tar.gz"
    if not archive.exists():
        temporary = archive.with_suffix(".partial")
        with urllib.request.urlopen(URL, timeout=90) as source, temporary.open("wb") as output:
            shutil.copyfileobj(source, output)
        temporary.replace(archive)
    if digest(archive) != SOURCE_SHA256:
        raise RuntimeError("MinIO source archive SHA-256 mismatch")
    binary = target / "minio"
    env = dict(
        os.environ,
        CGO_ENABLED="0",
        GOOS="linux",
        GOARCH="amd64",
        GOTOOLCHAIN="local",
        GOMAXPROCS="4",
        GOCACHE=str(target / "go-build-cache"),
        GOMODCACHE=str(target / "go-module-cache"),
        GOPROXY="https://proxy.golang.org,direct",
    )
    version = subprocess.check_output(["go", "version"], env=env, text=True).strip()
    command = ["go", "build", "-p", "4", "-trimpath", "-ldflags=-s -w", "-o", str(binary), "."]
    print(f"Building {TAG} from {URL}", flush=True)
    # Always build a fresh extraction, not an editable cached source directory.
    with tempfile.TemporaryDirectory(prefix="source-", dir=target) as temporary:
        with tarfile.open(archive) as stream:
            stream.extractall(temporary, filter="data")
        source = Path(temporary) / f"minio-{TAG}"
        if not (source / "go.mod").is_file():
            raise RuntimeError("source archive did not contain the expected Go module")
        subprocess.run(command, cwd=source, env=env, check=True, timeout=1800)
        module_digest = digest(source / "go.mod")
        shutil.copyfile(source / "LICENSE", target / "LICENSE")
    manifest = {
        "schema": 1,
        "source_tag": TAG,
        "source_url": URL,
        "source_sha256": digest(archive),
        "go_mod_sha256": module_digest,
        "go_version": version,
        "command": command,
        "binary_path": str(binary),
        "binary_sha256": digest(binary),
        "license_path": str(target / "LICENSE"),
    }
    path = target / "build.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Build manifest: {path}", flush=True)


if __name__ == "__main__":
    main()
