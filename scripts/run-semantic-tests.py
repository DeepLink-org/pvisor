#!/usr/bin/env python3
"""Run the human-reviewed semantic specifications in tests/semantics/.

A semantic case states a property pVisor must preserve, why it matters, what
a violating implementation would do, and one bash check that drives the real
CLI as a black box. Cases are reviewed by people; the ledger in
tests/semantics/REVIEWED binds each reviewed case text, and this runner's own
source, to a SHA-256 digest. Any edit invalidates the review until a person
approves it again with the `approve` subcommand.

    python3 scripts/run-semantic-tests.py list
    python3 scripts/run-semantic-tests.py run --pvisor target/debug/pvisor
    python3 scripts/run-semantic-tests.py review
    python3 scripts/run-semantic-tests.py approve S-STAGE-001 --reviewer NAME

The runner and its assertion vocabulary are deliberately small: a reviewer
must be able to read both to trust a PASS.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC_DIR = ROOT / "tests" / "semantics"
LEDGER = SPEC_DIR / "REVIEWED"
RUNNER_ID = "@runner"
CASE_TIMEOUT_SECONDS = 180

HEADING_RE = re.compile(r"^(#{1,3}) ")
CASE_RE = re.compile(r"^### (S-[A-Z]+-[0-9]{3})[：:]\s*(\S.*)$")
META_RE = re.compile(r"^<!--\s*semantic-case:(.*?)-->$")
FENCE_RE = re.compile(r"^```(\w*)\s*$")
REQUIRED_PROSE = ("**语义**", "**违反示例**")
KNOWN_REQUIREMENTS = {"stage"}
KNOWN_PLATFORMS = {"macos", "linux"}

# Assertion vocabulary available to every case. A case passes only when its
# script exits 0; `fail` names the violated property.
PREAMBLE = r"""
set -euo pipefail
fail() { printf 'SEMANTIC VIOLATION: %s\n' "$*" >&2; exit 1; }
pvisor() { "$PVISOR_BIN" "$@"; }
_semantic() { "$SEMANTIC_PYTHON" "$SEMANTIC_RUNNER" "$@"; }
expect_exit() {
  local want=$1 got=0; shift
  "$@" || got=$?
  [ "$got" -eq "$want" ] || fail "expected exit status $want, got $got: $*"
}
expect_refused() {
  if "$@"; then fail "expected refusal: $*"; fi
}
tree_state() { _semantic _tree "$1"; }
snapshot() { tree_state "$2" > "$CASE_ROOT/snapshot.$1"; }
assert_unchanged() {
  tree_state "$2" | diff -u "$CASE_ROOT/snapshot.$1" - || fail "$2 changed since snapshot $1"
}
assert_same_tree() {
  diff -u <(tree_state "$1") <(tree_state "$2") || fail "$1 and $2 differ"
}
assert_content() {
  [ -f "$1" ] && [ ! -L "$1" ] && [ "$(cat "$1")" = "$2" ] || fail "$1 does not contain exactly '$2'"
}
assert_absent() {
  if [ -e "$1" ] || [ -L "$1" ]; then fail "$1 exists"; fi
}
review_changes() { _semantic _changes "$1"; }
assert_changes() {
  diff -u - <(review_changes "$1") || fail "review of $1 differs from the expected net changes"
}
"""


@dataclasses.dataclass
class Case:
    id: str
    title: str
    source: Path
    line: int
    text: str
    script: str
    requires: list[str]
    xfail_on: list[str]
    xfail_reason: str

    @property
    def digest(self) -> str:
        return "sha256:" + hashlib.sha256(self.text.encode()).hexdigest()


@dataclasses.dataclass
class Approval:
    digest: str
    reviewer: str
    date: str


def normalize(lines: list[str]) -> str:
    stripped = [line.rstrip() for line in lines]
    while stripped and not stripped[-1]:
        stripped.pop()
    return "\n".join(stripped) + "\n"


def parse_spec(path: Path) -> list[Case]:
    lines = path.read_text(encoding="utf-8").splitlines()
    sections: list[tuple[int, list[str]]] = []
    current: list[str] | None = None
    in_fence = False
    for number, line in enumerate(lines, 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
        if not in_fence and HEADING_RE.match(line):
            current = None
            if CASE_RE.match(line):
                current = []
                sections.append((number, current))
        if current is not None:
            current.append(line)
    if in_fence:
        raise SystemExit(f"{path}: unterminated code fence")
    return [parse_case(path, number, body) for number, body in sections]


def parse_case(path: Path, number: int, body: list[str]) -> Case:
    where = f"{path.relative_to(ROOT)}:{number}"
    heading = CASE_RE.match(body[0])
    assert heading
    meta: dict[str, str] = {}
    scripts: list[list[str]] = []
    fence: list[str] | None = None
    for line in body[1:]:
        if fence is not None:
            if FENCE_RE.match(line):
                scripts.append(fence)
                fence = None
            else:
                fence.append(line)
            continue
        opening = FENCE_RE.match(line)
        if opening:
            if opening.group(1) != "bash":
                raise SystemExit(f"{where}: only bash fences are allowed in a case")
            fence = []
            continue
        if found := META_RE.match(line.strip()):
            if meta:
                raise SystemExit(f"{where}: duplicate semantic-case annotation")
            for item in shlex.split(found.group(1)):
                key, sep, value = item.partition("=")
                if not sep or key not in {"requires", "xfail-on", "xfail-reason"}:
                    raise SystemExit(f"{where}: invalid annotation {item!r}")
                meta[key] = value
    if len(scripts) != 1:
        raise SystemExit(f"{where}: a case needs exactly one bash fence")
    text = "\n".join(body)
    for marker in REQUIRED_PROSE:
        if marker not in text:
            raise SystemExit(f"{where}: missing {marker} paragraph")
    requires = [item for item in meta.get("requires", "").split(",") if item]
    xfail_on = [item for item in meta.get("xfail-on", "").split(",") if item]
    if unknown := set(requires) - KNOWN_REQUIREMENTS:
        raise SystemExit(f"{where}: unknown requirement {sorted(unknown)}")
    if unknown := set(xfail_on) - KNOWN_PLATFORMS:
        raise SystemExit(f"{where}: unknown platform {sorted(unknown)}")
    if bool(xfail_on) != bool(meta.get("xfail-reason")):
        raise SystemExit(f"{where}: xfail-on and xfail-reason must be given together")
    return Case(
        id=heading.group(1),
        title=heading.group(2),
        source=path,
        line=number,
        text=normalize(body),
        script="\n".join(scripts[0]) + "\n",
        requires=requires,
        xfail_on=xfail_on,
        xfail_reason=meta.get("xfail-reason", ""),
    )


def load_cases() -> list[Case]:
    cases: list[Case] = []
    for path in sorted(SPEC_DIR.glob("*.md")):
        if path.name != "README.md":
            cases.extend(parse_spec(path))
    seen: set[str] = set()
    for case in cases:
        if case.id in seen:
            raise SystemExit(f"duplicate semantic case {case.id}")
        seen.add(case.id)
    return cases


def runner_digest() -> str:
    source = Path(__file__).resolve().read_text(encoding="utf-8")
    return "sha256:" + hashlib.sha256(normalize(source.splitlines()).encode()).hexdigest()


def load_ledger() -> dict[str, Approval]:
    approvals: dict[str, Approval] = {}
    if not LEDGER.exists():
        return approvals
    for number, line in enumerate(LEDGER.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 4 or not fields[1].startswith("sha256:"):
            raise SystemExit(f"{LEDGER.relative_to(ROOT)}:{number}: malformed entry")
        approvals[fields[0]] = Approval(fields[1], fields[2], fields[3])
    return approvals


def review_state(identity: str, digest: str, ledger: dict[str, Approval]) -> str:
    approval = ledger.get(identity)
    if approval is None:
        return "UNREVIEWED"
    return "REVIEWED" if approval.digest == digest else "STALE"


def current_platform() -> str:
    return {"Darwin": "macos", "Linux": "linux"}.get(platform.system(), "other")


def missing_requirement(case: Case) -> str | None:
    for requirement in case.requires:
        if requirement == "stage":
            if current_platform() == "macos":
                if not Path("/Library/Filesystems/macfuse.fs").exists():
                    return "macFUSE is not installed"
            elif current_platform() == "linux":
                if not Path("/dev/fuse").exists():
                    return "/dev/fuse is unavailable"
                probe = subprocess.run(
                    ["unshare", "--user", "--map-root-user", "--mount", "true"],
                    capture_output=True,
                )
                if probe.returncode != 0:
                    return "user/mount namespaces are unavailable"
            else:
                return f"stage is unsupported on {platform.system()}"
    return None


def run_case(case: Case, pvisor: Path, keep: bool) -> tuple[str, str]:
    if reason := missing_requirement(case):
        return "SKIP", reason
    root = Path(tempfile.mkdtemp(prefix=f"pvisor-{case.id.lower()}-")).resolve()
    workspace = root / "ws"
    workspace.mkdir()
    env = dict(os.environ)
    env.update(
        PVISOR_BIN=str(pvisor),
        CASE_ROOT=str(root),
        WS=str(workspace),
        SEMANTIC_PYTHON=sys.executable,
        SEMANTIC_RUNNER=str(Path(__file__).resolve()),
    )
    log = root / "case.log"
    try:
        with log.open("wb") as output:
            completed = subprocess.run(
                ["bash", "-c", PREAMBLE + case.script],
                cwd=workspace,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                timeout=CASE_TIMEOUT_SECONDS,
            )
        passed = completed.returncode == 0
        detail = "" if passed else tail(log)
    except subprocess.TimeoutExpired:
        passed, detail = False, f"timed out after {CASE_TIMEOUT_SECONDS}s"
    expected_failure = current_platform() in case.xfail_on
    if expected_failure:
        status = "XPASS" if passed else "XFAIL"
        detail = detail if passed else case.xfail_reason
    else:
        status = "PASS" if passed else "FAIL"
    if keep or status in {"FAIL", "XPASS"}:
        detail = f"{detail}\n  kept: {root}".strip()
    else:
        shutil.rmtree(root, ignore_errors=True)
    return status, detail


def tail(path: Path, lines: int = 15) -> str:
    text = path.read_text(encoding="utf-8", errors="replace").splitlines()
    return "\n".join("  | " + line for line in text[-lines:])


def tree_state(directory: Path) -> list[str]:
    """Canonical state of a tree: type, permission bits, content or link target."""
    entries = []
    for current, directories, files in os.walk(directory):
        directories.sort()
        for name in sorted(directories + files):
            path = Path(current) / name
            relative = path.relative_to(directory).as_posix()
            info = path.lstat()
            mode = f"{stat.S_IMODE(info.st_mode):04o}"
            if stat.S_ISLNK(info.st_mode):
                entries.append(f"link {relative} -> {os.readlink(path)}")
            elif stat.S_ISDIR(info.st_mode):
                entries.append(f"dir  {mode} {relative}")
            elif stat.S_ISREG(info.st_mode):
                digest = hashlib.sha256(path.read_bytes()).hexdigest()[:16]
                entries.append(f"file {mode} {relative} {digest}")
            else:
                entries.append(f"other {relative}")
        directories[:] = [d for d in directories if not (Path(current) / d).is_symlink()]
    return entries


def review_changes(stage: str) -> list[str]:
    output = subprocess.run(
        [os.environ["PVISOR_BIN"], "status", "--review", "--json", stage],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    changes = json.loads(output)["filesystem"]["changes"]
    return sorted(f"{change['kind']} {change['path']}" for change in changes)


def command_list(_: argparse.Namespace) -> int:
    ledger = load_ledger()
    for case in load_cases():
        state = review_state(case.id, case.digest, ledger)
        print(f"{case.id}  {state:<10}  {case.title}")
    return 0


def command_review(args: argparse.Namespace) -> int:
    ledger = load_ledger()
    rows = [(RUNNER_ID, review_state(RUNNER_ID, runner_digest(), ledger), "runner and assertion vocabulary")]
    rows += [(case.id, review_state(case.id, case.digest, ledger), case.title) for case in load_cases()]
    for identity, state, title in rows:
        approval = ledger.get(identity)
        by = f"  ({approval.reviewer}, {approval.date})" if approval and state == "REVIEWED" else ""
        print(f"{identity:<12}  {state:<10}  {title}{by}")
    pending = [identity for identity, state, _ in rows if state != "REVIEWED"]
    if pending:
        print(f"\n{len(pending)} item(s) need human review: {', '.join(pending)}")
    return 1 if pending and args.strict else 0


def command_approve(args: argparse.Namespace) -> int:
    if not sys.stdin.isatty():
        print("approve requires an interactive terminal: a person must confirm each item", file=sys.stderr)
        return 2
    cases = {case.id: case for case in load_cases()}
    ledger = load_ledger()
    for identity in args.ids:
        if identity == RUNNER_ID:
            digest, text = runner_digest(), f"(review the source of {Path(__file__).relative_to(ROOT)})\n"
        elif identity in cases:
            digest, text = cases[identity].digest, cases[identity].text
        else:
            print(f"unknown item {identity}", file=sys.stderr)
            return 2
        print("=" * 72)
        print(text, end="")
        print("=" * 72)
        answer = input(f"Type {identity} to confirm you reviewed this text as {args.reviewer}: ")
        if answer.strip() != identity:
            print(f"skipped {identity}")
            continue
        ledger[identity] = Approval(digest, args.reviewer, dt.date.today().isoformat())
        print(f"approved {identity}")
    write_ledger(ledger)
    return 0


def write_ledger(ledger: dict[str, Approval]) -> None:
    header = [
        "# Human review ledger for tests/semantics. Edited only by",
        "# `scripts/run-semantic-tests.py approve`, run by a person.",
        "# <id> <digest> <reviewer> <date>",
    ]
    entries = [f"{key} {value.digest} {value.reviewer} {value.date}" for key, value in sorted(ledger.items())]
    LEDGER.write_text("\n".join(header + entries) + "\n", encoding="utf-8")


def command_run(args: argparse.Namespace) -> int:
    pvisor = Path(args.pvisor).resolve()
    if not os.access(pvisor, os.X_OK):
        print(f"pvisor binary not found: {pvisor}", file=sys.stderr)
        return 2
    cases = load_cases()
    if args.case:
        wanted = set(args.case.split(","))
        if unknown := wanted - {case.id for case in cases}:
            print(f"unknown case(s): {', '.join(sorted(unknown))}", file=sys.stderr)
            return 2
        cases = [case for case in cases if case.id in wanted]
    ledger = load_ledger()
    counts: dict[str, int] = {}
    unreviewed: list[str] = []
    runner_state = review_state(RUNNER_ID, runner_digest(), ledger)
    if runner_state != "REVIEWED":
        unreviewed.append(RUNNER_ID)
    for case in cases:
        status, detail = run_case(case, pvisor, args.keep)
        state = review_state(case.id, case.digest, ledger)
        if state != "REVIEWED":
            unreviewed.append(case.id)
        counts[status] = counts.get(status, 0) + 1
        print(f"{status:<5}  {case.id}  [{state}]  {case.title}")
        if detail:
            print(f"  {detail}" if not detail.startswith("  ") else detail)
    summary = ", ".join(f"{count} {status}" for status, count in sorted(counts.items()))
    print(f"\n{summary}; runner {runner_state}")
    failed = counts.get("FAIL", 0) + counts.get("XPASS", 0)
    if unreviewed:
        print(f"not human-reviewed: {', '.join(unreviewed)}")
        if args.require_reviewed:
            failed += len(unreviewed)
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="list cases and their review state").set_defaults(func=command_list)
    review = commands.add_parser("review", help="show the human review ledger state")
    review.add_argument("--strict", action="store_true", help="exit 1 when any item is not reviewed")
    review.set_defaults(func=command_review)
    approve = commands.add_parser("approve", help="record a human review (interactive)")
    approve.add_argument("ids", nargs="+", help=f"case IDs or {RUNNER_ID}")
    approve.add_argument("--reviewer", required=True)
    approve.set_defaults(func=command_approve)
    run = commands.add_parser("run", help="execute cases against a pvisor binary")
    run.add_argument("--pvisor", default=str(ROOT / "target" / "debug" / "pvisor"))
    run.add_argument("--case", help="comma-separated case IDs")
    run.add_argument("--keep", action="store_true", help="keep every case directory")
    run.add_argument("--require-reviewed", action="store_true", help="fail when an item lacks a current human review")
    run.set_defaults(func=command_run)
    tree = commands.add_parser("_tree")
    tree.add_argument("directory", type=Path)
    tree.set_defaults(func=lambda a: print("\n".join(tree_state(a.directory))) or 0)
    changes = commands.add_parser("_changes")
    changes.add_argument("stage")
    changes.set_defaults(func=lambda a: print("\n".join(review_changes(a.stage))) or 0)
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
