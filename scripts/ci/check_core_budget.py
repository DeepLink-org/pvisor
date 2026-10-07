#!/usr/bin/env python3
"""Enforce the embeddable runtime budget and default application boundaries."""

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
# remains opt-in; neither it nor a separate TUI package belongs in the default app.
APP_FORBIDDEN = {"pvisor-gateway", "pvisor-tui"}


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
        "optional Gateway or retired TUI package entered the default app closure"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="default pvisor-cli application binary")
    parser.add_argument("--max-dependencies", type=int, required=True)
    parser.add_argument("--max-bytes", type=int)
    parser.add_argument("--max-workspace-lines", type=int)
    parser.add_argument("--max-core-items", type=int)
    args = parser.parse_args()
    packages = dependency_closure("pvisor")
    app_packages = dependency_closure("pvisor-cli")
    check_boundaries(packages, app_packages)
    names = {package.split()[0] for package in packages}
    workspace_lines = sum(
        len(source.read_text().splitlines())
        for crate in Path("crates").iterdir()
        if crate.name in names
        for source in (crate / "src").rglob("*.rs")
    )
    public_item = re.compile(
        r"^\s*pub\s+(?:async\s+)?(?:struct|enum|trait|type|const|fn)\b", re.MULTILINE
    )
    core_items = sum(
        len(public_item.findall(source.read_text()))
        for source in Path("crates/pvisor-core/src").rglob("*.rs")
    )
    metrics = {
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "runtime_package": "pvisor",
        "application_package": "pvisor-cli",
        "dependencies": len(packages) - 1,
        "application_dependencies": len(app_packages) - 1,
        "binary_bytes": args.binary.stat().st_size,
        "workspace_source_lines": workspace_lines,
        "core_public_declarations": core_items,
    }
    print(json.dumps(metrics, indent=2))
    assert metrics["dependencies"] <= args.max_dependencies, "core dependency budget exceeded"
    if args.max_bytes is not None:
        assert metrics["binary_bytes"] <= args.max_bytes, "application binary size budget exceeded"
    if args.max_workspace_lines is not None:
        assert workspace_lines <= args.max_workspace_lines, "core workspace source budget exceeded"
    if args.max_core_items is not None:
        assert core_items <= args.max_core_items, "Core public API budget exceeded"


if __name__ == "__main__":
    main()
