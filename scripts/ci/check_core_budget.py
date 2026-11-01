#!/usr/bin/env python3
"""Report runtime size metrics and enforce default application boundaries.

Source lines, dependency counts, public declarations and binary bytes are
informational. Growth does not fail CI. Forbidden dependency edges remain
architecture errors, independently of the reported metric values.
"""

import argparse
import json
import re
import subprocess
from pathlib import Path

RUNTIME_FORBIDDEN = {
    "pvisor-cli",
    "pvisor-gateway",
    "pvisor-replay",
    "pvisor-tui",
    "clap",
    "ratatui",
    "crossterm",
    "vt100",
    "unicode-width",
}
# The replay executable lives in the app and needs the replay engine. Gateway
# remains opt-in; terminal UI dependencies do not belong in the application.
APP_FORBIDDEN = {
    "pvisor-gateway",
    "pvisor-tui",
    "ratatui",
    "crossterm",
    "vt100",
}


def dependency_closure(package):
    tree = subprocess.check_output(
        [
            "cargo",
            "tree",
            "--locked",
            "-p",
            package,
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        text=True,
    )
    return {" ".join(line.split()[:2]) for line in tree.splitlines() if line.strip()}


def check_boundaries(runtime_packages, app_packages):
    runtime_names = {package.split()[0] for package in runtime_packages}
    app_names = {package.split()[0] for package in app_packages}
    assert not RUNTIME_FORBIDDEN & runtime_names, "application tools entered the runtime closure"
    assert not APP_FORBIDDEN & app_names, (
        "optional Gateway or terminal UI dependency entered the default app closure"
    )


def source_lines(path):
    # Tolerate non-UTF-8 metadata such as AppleDouble files.
    return len(path.read_text(errors="replace").splitlines())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "binary",
        nargs="?",
        type=Path,
        help="default pvisor-cli application binary (informational byte size only)",
    )
    args = parser.parse_args()
    packages = dependency_closure("pvisor")
    app_packages = dependency_closure("pvisor-cli")
    check_boundaries(packages, app_packages)
    names = {package.split()[0] for package in packages}
    workspace_lines = sum(
        source_lines(source)
        for crate in Path("crates").iterdir()
        if crate.name in names
        for source in (crate / "src").rglob("*.rs")
    )
    public_item = re.compile(
        r"^\s*pub\s+(?:async\s+)?(?:struct|enum|trait|type|const|fn)\b", re.MULTILINE
    )
    core_sources = list(Path("crates/pvisor-core/src").rglob("*.rs"))
    core_items = sum(
        len(public_item.findall(source.read_text(errors="replace"))) for source in core_sources
    )
    metrics = {
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "runtime_package": "pvisor",
        "application_package": "pvisor-cli",
        "dependencies": len(packages) - 1,
        "application_dependencies": len(app_packages) - 1,
        "binary_bytes": args.binary.stat().st_size if args.binary else None,
        "workspace_source_lines": workspace_lines,
        "core_public_declarations": core_items,
    }
    print(json.dumps(metrics, indent=2))


if __name__ == "__main__":
    main()
