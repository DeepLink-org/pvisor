#!/usr/bin/env python3
"""Enforce the embeddable runtime budget and default application boundaries.

The workspace-line, dependency and public-API budgets compare against a
committed baseline (`scripts/ci/core-budget.json`, regenerated with `mise run
update-core-budget`). Any growth beyond the baseline fails CI; an intentional
growth regenerates the baseline in the same change so the exact delta stays
reviewable. The `--max-*` flags remain for ad-hoc local checks.
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
# remains opt-in; neither it nor a separate TUI package belongs in the default app.
APP_FORBIDDEN = {"pvisor-gateway", "pvisor-tui"}

BASELINE_NOTE = (
    "Regenerate with `mise run update-core-budget` in a change that intentionally "
    "grows the budget; CI fails whenever a metric exceeds the recorded value."
)
BASELINE_METRICS = ("dependencies", "workspace_source_lines", "core_public_declarations")


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


def source_lines(path):
    # Tolerate non-UTF-8 metadata such as AppleDouble files.
    return len(path.read_text(errors="replace").splitlines())


def enforce_baseline(metrics, baseline_path):
    baseline = json.loads(Path(baseline_path).read_text())
    exceeded = [
        f"{metric} {metrics[metric]} exceeds the committed budget baseline {baseline[metric]}"
        for metric in BASELINE_METRICS
        if metrics[metric] > baseline[metric]
    ]
    assert not exceeded, (
        "; ".join(exceeded) + f" ({baseline_path}); if the growth is intentional, "
        "regenerate the baseline with `mise run update-core-budget` and "
        "commit it in this change"
    )


def write_baseline(metrics, baseline_path):
    baseline = {metric: metrics[metric] for metric in BASELINE_METRICS}
    baseline["note"] = BASELINE_NOTE
    Path(baseline_path).write_text(json.dumps(baseline, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "binary",
        nargs="?",
        type=Path,
        help="default pvisor-cli application binary (informational byte size only)",
    )
    parser.add_argument("--max-dependencies", type=int)
    parser.add_argument("--max-bytes", type=int)
    parser.add_argument("--max-workspace-lines", type=int)
    parser.add_argument("--max-core-items", type=int)
    parser.add_argument(
        "--baseline",
        type=Path,
        help="committed budget baseline to enforce (see `mise run update-core-budget`)",
    )
    parser.add_argument(
        "--write-baseline",
        type=Path,
        help="write the current metrics as the new budget baseline",
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
    if args.write_baseline is not None:
        write_baseline(metrics, args.write_baseline)
        return
    if args.baseline is not None:
        enforce_baseline(metrics, args.baseline)
    if args.max_dependencies is not None:
        assert metrics["dependencies"] <= args.max_dependencies, "core dependency budget exceeded"
    if args.max_bytes is not None:
        assert metrics["binary_bytes"] <= args.max_bytes, "application binary size budget exceeded"
    if args.max_workspace_lines is not None:
        assert workspace_lines <= args.max_workspace_lines, "core workspace source budget exceeded"
    if args.max_core_items is not None:
        assert core_items <= args.max_core_items, "Core public API budget exceeded"


if __name__ == "__main__":
    main()
