#!/usr/bin/env python3
"""Derive reproducible native archives and a Homebrew formula from verified wheels."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import re
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path

from check_release_artifacts import validate_artifacts

ROOT = Path(__file__).resolve().parents[2]
BINARIES = ("pvisor", "pvisor-tui", "pvisor-replay", "pvisor-daemon")


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def native_archive(wheel: Path, destination: Path) -> None:
    """Copy only the complete native payload; never extract wheel-controlled paths."""
    names = (*BINARIES, "libkrunfw.SOURCE")
    payloads = {}
    with zipfile.ZipFile(wheel) as archive:
        for name in names:
            matches = [
                info
                for info in archive.infolist()
                if info.filename.endswith(f".data/scripts/{name}")
            ]
            if len(matches) != 1:
                raise ValueError(f"{wheel.name}: expected exactly one native payload {name}")
            payloads[f"bin/{name}"] = archive.read(matches[0])
    for name in ("LICENSE", "NOTICE"):
        payloads[name] = (ROOT / name).read_bytes()
    # Fixed timestamps, ownership and ordering make retries byte-identical.
    with destination.open("wb") as output:
        with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for name, content in sorted(payloads.items()):
                    info = tarfile.TarInfo(name)
                    info.size = len(content)
                    info.mode = (
                        0o755
                        if name.startswith("bin/") and name != "bin/libkrunfw.SOURCE"
                        else 0o644
                    )
                    archive.addfile(info, io.BytesIO(content))


def homebrew_formula(version: str, repository: str, archives: dict[str, Path]) -> str:
    """Render a project tap formula; signed macOS binaries are copied unchanged."""
    base = f"https://github.com/{repository}/releases/download/v{version}"
    macos = archives["macos-arm64"]
    linux = archives["linux-x86_64"]
    return f'''# Generated from the verified release wheels; do not edit checksums by hand.
class Pvisor < Formula
  desc "Scaling autonomous agent execution"
  homepage "https://deeplink-org.github.io/pvisor/"
  version "{version}"
  license "Apache-2.0"

  on_macos do
    depends_on arch: :arm64
    url "{base}/{macos.name}"
    sha256 "{digest(macos)}"
  end

  on_linux do
    depends_on arch: :x86_64
    url "{base}/{linux.name}"
    sha256 "{digest(linux)}"
  end

  def install
    # Keep companions and firmware beside the real executable, behind brew symlinks.
    libexec.install Dir["bin/*"]
    bin.install_symlink libexec/"pvisor", libexec/"pvisor-tui", libexec/"pvisor-replay", libexec/"pvisor-daemon"
    prefix.install "LICENSE", "NOTICE"
  end

  test do
    %w[pvisor pvisor-tui pvisor-replay pvisor-daemon].each do |name|
      assert_match version.to_s, shell_output("#{{bin}}/#{{name}} --version")
    end
    assert_match "hello", shell_output("#{{bin}}/pvisor spawn --executor host -- /bin/echo hello")
    system bin/"pvisor", "tui", "--help"
    system bin/"pvisor", "replay", "--help"
    system bin/"pvisor-daemon", "protocol"
  end
end
'''


def build_assets(directory: Path, version: str, repository: str, *, stable: bool) -> None:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:\+g[0-9]+\.[0-9a-f]+)?", version):
        raise ValueError("expected a stable or repository nightly version")
    if stable and "+" in version:
        raise ValueError("Homebrew requires a stable version")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("repository must be owner/name")
    wheels = validate_artifacts(directory, version)
    for platform in wheels:
        source = directory / f"pvisor-firmware-source-{platform}.tar.gz"
        if not source.is_file() or source.stat().st_size == 0:
            raise ValueError(f"missing corresponding firmware sources: {source.name}")
    archives = {}
    for platform, wheel in wheels.items():
        archive = directory / f"pvisor-{version}-{platform}.tar.gz"
        native_archive(wheel, archive)
        archives[platform] = archive
    if stable:
        (directory / "pvisor.rb").write_text(homebrew_formula(version, repository, archives))
    paths = sorted(
        [*wheels.values(), *archives.values(), *directory.glob("pvisor-firmware-source-*.tar.gz")]
    )
    if stable:
        paths.append(directory / "pvisor.rb")
    (directory / "SHA256SUMS").write_text(
        "".join(f"{digest(path)}  {path.name}\n" for path in sorted(paths))
    )


def smoke_archive(wheel: Path, version: str) -> None:
    """Check native archive contents through the symlink layout used by Homebrew."""
    smoke_root = ROOT / "target" / "native-smoke"
    smoke_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="install-", dir=smoke_root) as temporary:
        root = Path(temporary)
        archive = root / "native.tar.gz"
        native_archive(wheel, archive)
        with tarfile.open(archive) as bundle:
            bundle.extractall(root / "libexec", filter="data")
        links = root / "bin"
        links.mkdir()
        env = os.environ.copy()
        env["PVISOR_RUN_HOME"] = str(root / "runs")
        env["PATH"] = str(links) + os.pathsep + env.get("PATH", "")
        for name in BINARIES:
            executable = links / name
            executable.symlink_to(root / "libexec" / "bin" / name)
            result = subprocess.run(
                [str(executable), "--version"],
                check=True,
                capture_output=True,
                text=True,
                timeout=60,
                env=env,
            )
            if version not in result.stdout:
                raise RuntimeError(f"{name}: archive version does not match {version}")
        subprocess.run(
            [str(links / "pvisor"), "spawn", "--executor", "host", "--", "/bin/echo", "hello"],
            cwd=root,
            env=env,
            check=True,
            timeout=60,
        )
        for command in ("tui", "replay"):
            subprocess.run(
                [str(links / "pvisor"), command, "--help"], env=env, check=True, timeout=60
            )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository")
    parser.add_argument(
        "--smoke-wheel",
        action="store_true",
        help="Treat directory as one native wheel and test its archive",
    )
    parser.add_argument("--stable", action="store_true", help="Also generate the Homebrew formula")
    args = parser.parse_args()
    if args.smoke_wheel:
        smoke_archive(args.directory, args.version)
    else:
        if not args.repository:
            parser.error("--repository is required when building a release set")
        build_assets(args.directory, args.version, args.repository, stable=args.stable)


if __name__ == "__main__":
    main()
