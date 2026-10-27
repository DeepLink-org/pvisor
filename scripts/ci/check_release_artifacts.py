#!/usr/bin/env python3
"""Check that a stable release contains one wheel for every supported platform."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import zipfile
from email.parser import BytesParser
from pathlib import Path

DEFAULT_MAX_BYTES = 100_000_000
PLATFORM_PATTERNS = {
    "linux-x86_64": re.compile(r"^manylinux.*_x86_64$"),
    "macos-arm64": re.compile(r"^macosx.*_arm64$"),
}


class ArtifactValidationError(RuntimeError):
    """Raised when the release artifact set is incomplete or inconsistent."""


def _metadata_version(wheel: Path) -> str:
    with zipfile.ZipFile(wheel) as archive:
        metadata_files = [
            name for name in archive.namelist() if name.endswith(".dist-info/METADATA")
        ]
        if len(metadata_files) != 1:
            raise ArtifactValidationError(
                f"{wheel.name}: expected one METADATA file, found {len(metadata_files)}"
            )
        metadata = BytesParser().parsebytes(archive.read(metadata_files[0]))
    if metadata.get("Name", "").lower() != "pvisor-sandbox":
        raise ArtifactValidationError(
            f"{wheel.name}: expected METADATA Name 'pvisor-sandbox', got {metadata.get('Name')!r}"
        )
    version = metadata.get("Version")
    if not version:
        raise ArtifactValidationError(f"{wheel.name}: METADATA has no Version")
    return version


def validate_artifacts(
    directory: Path,
    version: str,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
) -> dict[str, Path]:
    wheels = sorted(directory.glob("*.whl"))
    if len(wheels) != len(PLATFORM_PATTERNS):
        names = ", ".join(wheel.name for wheel in wheels) or "none"
        raise ArtifactValidationError(
            f"expected {len(PLATFORM_PATTERNS)} wheels, found {len(wheels)}: {names}"
        )

    prefix = f"pvisor_sandbox-{version}-py3-none-"
    found: dict[str, Path] = {}
    for wheel in wheels:
        if wheel.stat().st_size > max_bytes:
            raise ArtifactValidationError(
                f"{wheel.name}: {wheel.stat().st_size} bytes exceeds {max_bytes}"
            )
        if not wheel.name.startswith(prefix) or not wheel.name.endswith(".whl"):
            raise ArtifactValidationError(
                f"{wheel.name}: expected filename prefix {prefix!r} and a .whl suffix"
            )
        platform_tag = wheel.name[len(prefix) : -len(".whl")]
        matches = [
            name for name, pattern in PLATFORM_PATTERNS.items() if pattern.fullmatch(platform_tag)
        ]
        if len(matches) != 1:
            raise ArtifactValidationError(
                f"{wheel.name}: unsupported or ambiguous platform tag {platform_tag!r}"
            )
        platform_name = matches[0]
        if platform_name in found:
            raise ArtifactValidationError(
                f"duplicate {platform_name} wheels: {found[platform_name].name}, {wheel.name}"
            )
        metadata_version = _metadata_version(wheel)
        if metadata_version != version:
            raise ArtifactValidationError(
                f"{wheel.name}: METADATA version {metadata_version!r} != {version!r}"
            )
        found[platform_name] = wheel

    missing = set(PLATFORM_PATTERNS) - set(found)
    if missing:
        raise ArtifactValidationError(f"missing wheels for: {', '.join(sorted(missing))}")
    return found


def validate_release_set(
    directory: Path, version: str, *, max_bytes: int = DEFAULT_MAX_BYTES
) -> dict[str, Path]:
    """Require the complete immutable stable set before resuming a channel."""
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ArtifactValidationError("recovery requires a stable X.Y.Z version")
    manifest = directory / "SHA256SUMS"
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_size > 65536:
        raise ArtifactValidationError("missing or invalid SHA256SUMS")
    checksums: dict[str, str] = {}
    for line in manifest.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in checksums:
            raise ArtifactValidationError("invalid or duplicate SHA256SUMS entry")
        checksums[match[2]] = match[1]
    paths = {path.name: path for path in directory.iterdir() if path.name != "SHA256SUMS"}
    if set(paths) != set(checksums):
        raise ArtifactValidationError("SHA256SUMS does not cover exactly the downloaded set")
    for name, path in paths.items():
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ArtifactValidationError(f"{name}: expected a nonempty regular file")
        with path.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != checksums[name]:
            raise ArtifactValidationError(f"{name}: SHA-256 mismatch")
    wheels = validate_artifacts(directory, version, max_bytes=max_bytes)
    expected = {wheel.name for wheel in wheels.values()} | {"pvisor.rb"}
    for platform in PLATFORM_PATTERNS:
        expected.add(f"pvisor-{version}-{platform}.tar.gz")
        expected.add(f"pvisor-firmware-source-{platform}.tar.gz")
    if set(paths) != expected:
        raise ArtifactValidationError("expected both native archives, firmware sources and formula")
    return wheels


def validate_recovery_source(
    run: dict, jobs: list[dict], repository: str, version: str, tag_sha: str
) -> None:
    """A failed publisher is recoverable only after the original tag's gates passed."""
    if (
        run.get("repository", {}).get("full_name") != repository
        or run.get("head_repository", {}).get("full_name") != repository
        or run.get("path") != ".github/workflows/release.yml"
        or run.get("event") != "push"
        or run.get("status") != "completed"
        or run.get("head_branch") != f"v{version}"
        or not re.fullmatch(r"[0-9a-f]{40}", tag_sha)
        or run.get("head_sha") != tag_sha
    ):
        raise ArtifactValidationError("source must be a completed release run at the unchanged tag")
    verified = [
        job
        for job in jobs
        if job.get("name") == "Build and verify release / Verify release artifact set"
    ]
    if len(verified) != 1 or verified[0].get("conclusion") != "success":
        raise ArtifactValidationError("source run did not pass the complete distribution gates")


def restore_artifacts(directory: Path, version: str, repository: str, run_id: int) -> None:
    """Download an accepted run's original bytes, never rebuild an immutable version."""
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or run_id <= 0:
        raise ArtifactValidationError("expected owner/repository and a positive source run ID")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ArtifactValidationError("recovery requires a stable X.Y.Z version")

    def api(endpoint: str, *options: str):
        return json.loads(
            subprocess.check_output(
                ["gh", "api", f"repos/{repository}/{endpoint}", *options],
                timeout=60,
            )
        )

    run = api(f"actions/runs/{run_id}")
    pages = api(f"actions/runs/{run_id}/jobs?per_page=100&filter=latest", "--paginate", "--slurp")
    tag_sha = api(f"commits/v{version}")["sha"]
    validate_recovery_source(
        run, [job for page in pages for job in page["jobs"]], repository, version, tag_sha
    )
    subprocess.run(
        [
            "gh",
            "run",
            "download",
            str(run_id),
            "--repo",
            repository,
            "--name",
            "release-wheels",
            "--name",
            "release-native",
            "--dir",
            str(directory),
        ],
        check=True,
        timeout=600,
    )
    # gh separates multiple named artifacts; merge only after checking for collisions.
    for name in ("release-wheels", "release-native"):
        staged = directory / name
        for path in staged.iterdir():
            destination = directory / path.name
            if destination.exists() or destination.is_symlink():
                raise ArtifactValidationError(f"duplicate restored asset: {path.name}")
            path.rename(destination)
        staged.rmdir()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    parser.add_argument(
        "--complete", action="store_true", help="Verify the full stable release set"
    )
    parser.add_argument("--source-run", type=int, help="Restore a verified tag run's artifacts")
    parser.add_argument("--repository", help="Source owner/repository (required with --source-run)")
    args = parser.parse_args()
    if args.source_run is not None and not args.repository:
        parser.error("--source-run requires --repository")

    try:
        if args.source_run is not None:
            restore_artifacts(args.directory, args.version, args.repository, args.source_run)
        validate = (
            validate_release_set
            if args.complete or args.source_run is not None
            else validate_artifacts
        )
        found = validate(args.directory, args.version, max_bytes=args.max_bytes)
    except ArtifactValidationError as error:
        print(f"release artifact validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

    for platform_name, wheel in sorted(found.items()):
        print(f"{platform_name}={wheel.name} bytes={wheel.stat().st_size}")


if __name__ == "__main__":
    main()
