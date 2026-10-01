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
