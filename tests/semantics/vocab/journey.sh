# Independent learning-path fixtures. No simulated Jobs or product internals.
journey_setup() {
  export HOME="$CASE_ROOT/home"
  export XDG_CONFIG_HOME="$HOME/config" XDG_DATA_HOME="$HOME/data" XDG_CACHE_HOME="$HOME/cache"
  export PVISOR_RUN_HOME="$CASE_ROOT/jobs"
  mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$PVISOR_RUN_HOME"
}
pvisor() { "$SUBJECT_BIN" "$@"; }
journey_json() { journey_tools get "$@"; }
json_expect() { journey_tools expect "$@"; }
json_length() { journey_tools length "$@"; }
json_paths() { journey_tools paths "$@"; }
journey_bundle() { journey_tools bundle "$PVISOR_RUN_HOME"; }
journey_contains() {
  if ! LC_ALL=C grep -Fq -- "$2" "$1"; then fail "missing '$2' in $1"; fi
}
journey_wait_file() {
  local path=$1 pid=$2
  for ((attempt=0; attempt<200; attempt++)); do
    [ ! -f "$path" ] || return 0
    kill -0 "$pid" 2>/dev/null || fail "process exited before creating $path"
    sleep 0.025
  done
  fail "timed out waiting for $path"
}

# Assertions and fixture code participate in the semspec vocabulary digest.
journey_tools() {
  python3 - "$@" <<'PYTHON'
#!/usr/bin/env python3
"""Fixture services and assertions for the executable learning path."""

import json
import socket
import sys
import time
from pathlib import Path


def lookup(document, pointer):
    for key in pointer.removeprefix("/").split("/") if pointer else []:
        key = key.replace("~1", "/").replace("~0", "~")
        document = document[int(key)] if isinstance(document, list) else document[key]
    return document


def main(args):
    action, filename, *rest = args
    path = Path(filename)
    if action == "listen":
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            path.write_text(str(listener.getsockname()[1]))
            # Accept and close all probes; no public network or API key needed.
            listener.settimeout(0.2)
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                try:
                    connection, _ = listener.accept()
                    connection.close()
                except TimeoutError:
                    pass
        return
    if action == "bundle":
        paths = list(path.rglob("run-bundle.json"))
        if len(paths) != 1:
            raise AssertionError(f"expected one Run Bundle, found {len(paths)}")
        print(paths[0].read_text())
        return
    document = json.loads(path.read_text())
    if action == "paths":
        actual = sorted(item["path"] for item in document["filesystem"]["changes"])
        expected = sorted(rest)
    else:
        value = lookup(document, rest[0])
        if action == "get":
            print(value if isinstance(value, str) else json.dumps(value))
            return
        if action == "expect":
            actual, expected = value, json.loads(rest[1])
        elif action == "length":
            actual, expected = len(value), int(rest[1])
        else:
            raise ValueError(f"unknown assertion: {action}")
    if actual != expected or type(actual) is not type(expected):
        raise AssertionError(f"{path}: {action} {rest}: got {actual!r}, wanted {expected!r}")


if __name__ == "__main__":
    main(sys.argv[1:])
PYTHON
}
