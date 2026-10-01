#!/usr/bin/env python3
"""Record an isolated default core build and enforce its dependency budget."""
import argparse
import json
import re
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=Path)
parser.add_argument("--max-dependencies", type=int, required=True)
parser.add_argument("--max-bytes", type=int)
parser.add_argument("--max-workspace-lines", type=int)
parser.add_argument("--max-control-items", type=int)
args = parser.parse_args()
tree = subprocess.check_output(
    ["cargo", "tree", "--locked", "-p", "pvisor", "--edges", "normal",
     "--prefix", "none", "--format", "{p}"], text=True
)
packages = {" ".join(line.split()[:2]) for line in tree.splitlines() if line.strip()}
forbidden = {"pvisor-gateway", "pvisor-replay", "pvisor-tui", "vt100", "unicode-width"}
assert not forbidden & {package.split()[0] for package in packages}, "optional tools entered the core closure"
names = {package.split()[0] for package in packages}
workspace_lines = sum(
    len(source.read_text().splitlines())
    for crate in Path("crates").iterdir() if crate.name in names
    for source in (crate / "src").rglob("*.rs")
)
public_item = re.compile(r"^\s*pub\s+(?:async\s+)?(?:struct|enum|trait|type|const|fn)\b", re.MULTILINE)
control_items = sum(len(public_item.findall(source.read_text()))
                    for source in Path("crates/pvisor-control/src").rglob("*.rs"))
metrics = {
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "dependencies": len(packages) - 1,
    "binary_bytes": args.binary.stat().st_size,
    "workspace_source_lines": workspace_lines,
    "control_public_declarations": control_items,
}
print(json.dumps(metrics, indent=2))
assert metrics["dependencies"] <= args.max_dependencies, "core dependency budget exceeded"
if args.max_bytes is not None:
    assert metrics["binary_bytes"] <= args.max_bytes, "core binary size budget exceeded"

if args.max_workspace_lines is not None:
    assert workspace_lines <= args.max_workspace_lines, "core workspace source budget exceeded"
if args.max_control_items is not None:
    assert control_items <= args.max_control_items, "Control public API budget exceeded"
