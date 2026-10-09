# Commands and scenarios

When integrating a task into a script, handle launch arguments, execution results, and file acceptance separately. Ordinary host execution writes directly to the workspace; use `--safe`, `--ask`, or an explicit `--stage` to review files before accepting them.

## From execution to file acceptance

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

After execution, `report.txt` stays in staging; `inspect` reads the proposal and `apply` accepts the selected file. Use `drop` to discard remaining staged changes you do not accept. Check execution and apply exit codes separately; workload exit 0 does not authorize automatic file acceptance. See [Exit codes and errors](exit-codes.md) for conflicts and failures. The [CLI reference](cli.md) defines argument syntax and storage defaults.

## Read records and use protocols

`status --json` provides a status overview; `status --review --json` provides the complete Run Bundle. Automation first checks format versions and required fields, then execution state, installed controls, and net file changes. Missing observations must not become zero or acceptance. See [Run Bundle](run-bundle.md) and [Machine-readable output](json-output.md) for fields and queries.

Shared image cache handles, authentication, and failures follow the [cache protocol](shared-image-cache.md), separately from Job staging and apply. Saving and restoring full machine state also requires the corresponding capability. A workspace checkpoint saves file proposals without continuing old process memory; check prerequisites against the [CLI execution-checkpoint contract](cli.md#full-vm-execution-checkpoints) before use.

## Check executable behavior

The [scenario appendix](cases.md) retains product commands, semantic claims, and assertions, and serves as the DOC semspec source. Runner fixtures in its code blocks cannot be run as ordinary shell commands; the repository entry `just cases` executes the checks. Execution success and human approval are recorded separately; PASS does not authorize changes to human review ledgers.

## Scenario check preparation

These Markdown functions are shared by the scenario appendix and VM checks. semspec extracts the marked preparation; each case has independent CASE_ROOT and WS and an explicit SUBJECT_BIN. All preparation and assertions participate in review digests.

<!-- semspec: setup -->
````bash
export RUST_LOG=warn
# Migrated documented-case vocabulary; fixtures and artifact access are sealed.
case_setup() {
  export PVISOR_CASE_ROOT="$CASE_ROOT" PVISOR_CASE_WORKSPACE="$WS"
  export PVISOR_CASE_RECORDS="$CASE_ROOT/records" PVISOR_CASE_STDOUT="$CASE_ROOT/command.log"
  export PVISOR_RUN_HOME="$PVISOR_CASE_RECORDS"
  export XDG_CONFIG_HOME="$CASE_ROOT/config" XDG_DATA_HOME="$CASE_ROOT/data"
  export CASE_TRUE="$(type -P true)"
  export CASE_ROOTFS="${PVISOR_CASE_ROOTFS:-/}" CASE_IMAGE="${PVISOR_CASE_IMAGE:-ubuntu:latest}"
  export CASE_CONTAINER_IMAGE="${PVISOR_CASE_CONTAINER_IMAGE:-ubuntu:latest}"
  export CASE_CONTAINER_RUNTIME="${PVISOR_CASE_CONTAINER_RUNTIME:-$(command -v crun || command -v runc || :)}"
  export CASE_AGENT="${PVISOR_CASE_AGENT:-$CASE_TRUE}" CASE_TMP_PATH="/tmp/pvisor-root-change-${CASE_ROOT##*/}"
  mkdir -p "$CASE_ROOT/bin" "$PVISOR_CASE_RECORDS" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
  # Python/PTY examples must resolve the same subject as direct Bash invocations.
  cat > "$CASE_ROOT/bin/pvisor" <<'SH'
#!/usr/bin/env bash
exec "$SUBJECT_BIN" "$@"
SH
  chmod 755 "$CASE_ROOT/bin/pvisor"
  export PATH="$CASE_ROOT/bin:$PATH"
  python3 - "$WS" "$CASE_TRUE" > "$CASE_ROOT/ports" <<'PYTHON'
import json, pathlib, socket, sys
ws = pathlib.Path(sys.argv[1]); program = sys.argv[2]
config = '[run]\ncommand = [' + json.dumps(program) + ']\n'
(ws/'pvisor.toml').write_text(config)
(ws/'config-without-extension').write_text(config)
(ws/'run-spec.json').write_text(json.dumps({'run_id':'case-i02','agent':{'name':'case-i02'},'invocation':{'kind':'process','program':program}})+'\n')
# Keep both sockets open during allocation so their ports cannot be identical.
with socket.socket() as proxy, socket.socket() as gateway:
    proxy.bind(('127.0.0.1', 0)); gateway.bind(('127.0.0.1', 0))
    print(f'127.0.0.1:{proxy.getsockname()[1]} 127.0.0.1:{gateway.getsockname()[1]}')
PYTHON
  # ponytail: release ports before product bind; retry allocation if CI demonstrates collisions.
  read -r CASE_PROXY_LISTEN CASE_GATEWAY_LISTEN < "$CASE_ROOT/ports"
  export CASE_PROXY_LISTEN CASE_GATEWAY_LISTEN
}
case_run() {
  local expectation=$1 status=0
  cat > "$CASE_ROOT/command.bash"
  # A separate Bash preserves errexit even when its exit status is inspected.
  bash -euo pipefail "$CASE_ROOT/command.bash" > "$PVISOR_CASE_STDOUT" 2>&1 || status=$?
  cat "$PVISOR_CASE_STDOUT"
  case "$expectation" in
    success) [ "$status" -eq 0 ] || fail "expected success, exit=$status" ;;
    nonzero) [ "$status" -ne 0 ] || fail 'expected nonzero exit' ;;
    *) fail "unknown documented expectation: $expectation" ;;
  esac
}
_pvisor_helper() {
  python3 - "$@" <<'PYTHON'
#!/usr/bin/env python3
"""Sealed Run Bundle accessors for the semspec DOC catalog."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys


FAMILIES = {"bundle": "run-bundle.json", "record": "run.json"}


def newest(root: Path, filename: str) -> Path:
    candidates = [path for path in root.rglob(filename) if path.is_file()]
    if not candidates:
        raise SystemExit(f"assert: no {filename} under {root}")
    return max(candidates, key=lambda path: path.stat().st_mtime)


def lookup(document: object, dotted: str) -> object:
    cursor = document
    for segment in dotted.split("."):
        if isinstance(cursor, list):
            try:
                cursor = cursor[int(segment)]
            except (ValueError, IndexError) as error:
                raise SystemExit(f"assert: {dotted}: bad list index {segment!r}") from error
        elif isinstance(cursor, dict):
            if segment not in cursor:
                raise SystemExit(f"assert: {dotted}: missing key {segment!r}")
            cursor = cursor[segment]
        else:
            raise SystemExit(f"assert: {dotted}: {segment!r} has no container to index")
    return cursor


def render(value: object) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return value
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    if isinstance(value, (int, float)):
        return str(value)
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[0] not in FAMILIES:
        raise SystemExit("assert: usage: helper <bundle|record> <get|expect|contains> ...")
    filename = FAMILIES[argv[0]]
    command, arguments = argv[1], argv[2:]
    default_root = os.environ.get("PVISOR_CASE_ROOT", ".")

    def artifact(index: int) -> Path:
        root = Path(arguments[index] if len(arguments) > index else default_root)
        return newest(root, filename)

    if command == "get":
        if not arguments:
            raise SystemExit("assert: usage: helper get <dotted.path> [ROOT]")
        document = json.loads(artifact(1).read_text(encoding="utf-8"))
        print(render(lookup(document, arguments[0])))
        return 0

    if command == "expect":
        if len(arguments) < 2:
            raise SystemExit("assert: usage: helper expect <dotted.path> <value> [ROOT]")
        source = artifact(2)
        document = json.loads(source.read_text(encoding="utf-8"))
        actual = render(lookup(document, arguments[0]))
        if actual != arguments[1]:
            raise SystemExit(
                f"assert: {arguments[0]} is {actual!r}, expected {arguments[1]!r} ({source})"
            )
        return 0

    if command == "contains":
        if len(arguments) < 2:
            raise SystemExit("assert: usage: helper contains <dotted.path> <substring> [ROOT]")
        source = artifact(2)
        document = json.loads(source.read_text(encoding="utf-8"))
        haystack = render(lookup(document, arguments[0]))
        if arguments[1] not in haystack:
            raise SystemExit(
                f"assert: {arguments[0]} does not contain {arguments[1]!r}; "
                f"value is {haystack!r} ({source})"
            )
        return 0

    raise SystemExit(f"assert: unknown helper command {command!r}")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
PYTHON
}


bundle_get() { _pvisor_helper bundle get "$@"; }
bundle_expect() { _pvisor_helper bundle expect "$@"; }
bundle_contains() { _pvisor_helper bundle contains "$@"; }
record_get() { _pvisor_helper record get "$@"; }
record_expect() { _pvisor_helper record expect "$@"; }
record_contains() { _pvisor_helper record contains "$@"; }
stdout_has() {
  if ! grep -Fq -- "$1" "$PVISOR_CASE_STDOUT"; then
    printf 'assert: command output does not contain %s\n' "$1" >&2
    return 1
  fi
}

# Sealed assertion vocabulary. Exact bytes matter, including final newlines.
fail() { printf 'SEMANTIC VIOLATION: %s\n' "$*" >&2; exit 1; }
expect_exit() {
  local want=$1 got=0; shift
  "$@" || got=$?
  [ "$got" -eq "$want" ] || fail "expected exit $want, got $got: $*"
}
expect_refused() { if "$@"; then fail "expected refusal: $*"; fi; }
snapshot() {
  [[ $1 =~ ^[a-zA-Z0-9_-]+$ ]] || fail 'invalid snapshot name'
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/snapshot.$1"
}
assert_unchanged() {
  [[ $1 =~ ^[a-zA-Z0-9_-]+$ ]] || fail 'invalid snapshot name'
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/current.tree"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/snapshot.$1" "$CASE_ROOT/current.tree" || fail "tree changed: $2"
}
assert_same_tree() {
  "$SEMSPEC_BIN" helper tree-state "$1" > "$CASE_ROOT/a.tree"
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/b.tree"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/a.tree" "$CASE_ROOT/b.tree" || fail 'trees differ'
}
assert_content() {
  [ -f "$1" ] && [ ! -L "$1" ] || fail "not a regular file: $1"
  cmp -s -- "$1" <(printf '%s' "$2") || fail "content differs: $1"
}
assert_absent() { if [ -e "$1" ] || [ -L "$1" ]; then fail "path exists: $1"; fi; }

# Public CLI only; all personal settings and Job records stay in CASE_ROOT.
pvisor() { XDG_DATA_HOME="$CASE_ROOT/data" XDG_CONFIG_HOME="$CASE_ROOT/config" "$SUBJECT_BIN" "$@"; }
stage() { pvisor run --no-agent-defaults --executor host --gateway-mode off --stage "$1" -- /bin/sh -eu -c "$2"; }
review_changes() {
  pvisor status --review --json "$1" > "$CASE_ROOT/review.json"
  python3 - "$CASE_ROOT/review.json" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    changes = json.load(f)["filesystem"]["changes"]
for row in sorted(f'{item["kind"]} {item["path"]}' for item in changes):
    print(row)
PY
}
assert_changes() {
  cat > "$CASE_ROOT/expected.changes"
  review_changes "$1" > "$CASE_ROOT/actual.changes"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/expected.changes" "$CASE_ROOT/actual.changes" || fail 'review differs from net changes'
}

# Environment prerequisites: exit 77 skips this check, never changes its assertions.
skip() { printf 'SKIP: %s\n' "$*" >&2; exit 77; }
require_python3() { python3 --version >/dev/null 2>&1 || skip 'python3 unavailable'; }
require_linux() { [ "$(uname -s)" = Linux ] || skip 'Linux required'; }
require_stage() {
  require_python3
  case "$(uname -s)" in
    Darwin) [ -e /Library/Filesystems/macfuse.fs ] || skip 'macFUSE unavailable' ;;
    Linux) [ -e /dev/fuse ] && unshare -Ur -m true || skip 'FUSE/user namespaces unavailable' ;;
    *) skip 'stage unsupported on this OS' ;;
  esac
}
require_rootless() { require_linux; unshare --user --mount --pid --fork true || skip 'user namespaces unavailable'; }
require_kvm() { require_linux; [ -e /dev/kvm ] || skip '/dev/kvm unavailable'; }
require_rootfs() { require_linux; [ -d "${PVISOR_CASE_ROOTFS:-/}" ] || skip 'rootfs unavailable'; }
require_image() { require_linux; [ -n "${PVISOR_CASE_IMAGE:-ubuntu:latest}" ] || skip 'image unavailable'; }
require_agent() { require_linux; [ -n "${PVISOR_CASE_AGENT:-}" ] || skip 'agent unavailable'; }
require_container() {
  require_linux
  if [ -n "${PVISOR_CASE_CONTAINER_RUNTIME:-}" ]; then
    command -v "$PVISOR_CASE_CONTAINER_RUNTIME" || skip 'container runtime unavailable'
  else
    command -v crun || command -v runc || skip 'container runtime unavailable'
  fi
}
require_container_runtime() { require_container; }
require_runc() { require_linux; command -v runc || skip 'runc unavailable'; }
require_curl() { curl --version >/dev/null 2>&1 || skip 'curl unavailable'; }
````
