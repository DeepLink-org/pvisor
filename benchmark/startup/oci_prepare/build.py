"""Build the pinned experimental umoci patch without network access or installation."""

import argparse
import hashlib
import json
import os
import subprocess
import tarfile
from pathlib import Path

ARCHIVE_SHA256 = "c01b36de6fdc513eb65add57bc882d72f94fc3b4b65a8f9ef59826fb754af93e"
SOURCE_NAME = "umoci-0.4.7"


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def build(archive, output, go):
    if digest(archive) != ARCHIVE_SHA256:
        raise ValueError("umoci source archive checksum mismatch")
    if output.exists():
        raise FileExistsError("output already exists; preserve prior build evidence")
    patch = Path(__file__).with_name("singlepass-metadata.patch")
    output.mkdir(parents=True)
    with tarfile.open(archive, "r:gz") as source:
        source.extractall(output, filter="data")
    root = output / SOURCE_NAME
    env = os.environ.copy()
    env.update(
        CGO_ENABLED="0",
        GOPROXY="off",
        GOTOOLCHAIN="local",
        GOCACHE=str(output / "go-cache"),
        GOPATH=str(output / "go-path"),
    )
    with (output / "build.log").open("x") as log:

        def run(args):
            subprocess.run(
                args,
                cwd=root,
                env=env,
                check=True,
                stdout=log,
                stderr=subprocess.STDOUT,
                timeout=900,
            )

        run(["patch", "--batch", "-p1", "--input", str(patch)])
        run([str(go), "version"])
        run(
            [
                str(go),
                "test",
                "-mod=vendor",
                "./oci/layer",
                "-run",
                "TestPvisor|TestUnpackEntrySanitise|TestUnpackEntrySymlinkScoping",
                "-count=1",
                "-v",
            ]
        )
        run(
            [
                str(go),
                "build",
                "-mod=vendor",
                "-trimpath",
                "-o",
                str(output / "umoci-experiment"),
                "./cmd/umoci",
            ]
        )
    receipt = {
        "archive_sha256": digest(archive),
        "patch_sha256": digest(patch),
        "binary_sha256": digest(output / "umoci-experiment"),
        "go": str(go),
        "tests_passed": True,
        "installed": False,
    }
    (output / "build-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--go", type=Path, required=True)
    args = parser.parse_args()
    print(
        json.dumps(
            build(args.archive.resolve(), args.output.resolve(), args.go.resolve()), indent=2
        )
    )


if __name__ == "__main__":
    main()
