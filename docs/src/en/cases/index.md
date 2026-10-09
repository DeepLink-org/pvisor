# Learn pVisor through tasks

Start with a trusted script, then stage files, inspect execution evidence, save proposals, and explore branches before setting boundaries for untrusted work. Each chapter explains when to use a capability, which command to run, and what result to expect.

| Order | Your question | Commands | Executable cases |
|---|---|---|---|
| [1. First Job](01-first-job.md) | How do I record scripts, failures, and deadlines? | `run`, `status`, `--config` | S-USE-001–004 |
| [2. Review and decide](02-review-and-decide.md) | What did the Agent change, and what should I accept? | `review`, `inspect`, `apply`, `drop`, `kill` | S-USE-005–008 |
| [3. Save and branch](03-checkpoint-and-fork.md) | How do I retain a proposal and compare independent attempts? | `checkpoint create/list/show`, `fork` | S-USE-009–011 |
| [4. Set boundaries](04-boundaries.md) | How do I restrict file access and network connections? | `--safe`, `--access`, `--overlaynet-deny-all` | S-USE-012–014 |
| [5. History and restoration](05-tools-and-restoration.md) | Should I restore files, Agent history, or the whole VM? | `replay`, capability inspection | S-USE-015–016 |

After the five chapters, [check staging, apply and drop contracts](06-stage-apply.md) with 14 STAGE checks. The USE gate selects only S-USE cases from the first five chapters.

## Complete one file review

After installation, run these commands in a test directory. Use an absolute stage path outside the workspace so execution records are not treated as project files. The original directory has no report.txt after the command; inspect reads the proposal, and apply creates the file in the original directory.

```bash
sandbox=$(mktemp -d)
mkdir "$sandbox/workspace"
cd "$sandbox/workspace"
pvisor run --stage "$sandbox/draft" -- /bin/sh -c 'printf proposal > report.txt'
pvisor review "$sandbox/draft" --diff
pvisor inspect "$sandbox/draft" -- /bin/cat report.txt
pvisor apply "$sandbox/draft" --path report.txt
cat report.txt
```

This stages workspace files while host file access remains ambient. Before running an untrusted Agent, continue to chapter 4, inspect actual isolation evidence, and read about [execution environments](../guides/executors/index.md).

## Choosing companion tools

| Need | Tool and entry point | Next step |
|---|---|---|
| Review and approve access interactively | `pvisor run --tui` / `--ask`, dispatched to adjacent `pvisor-tui` | [CLI reference](../reference/cli.md) |
| Route real model requests and capture trajectories | `pvisor run --gateway-mode capture --gateway-route ...` | [Capture guide](../guides/capture.md) and the local mock-request test in `just examples 04-gateway-llm-control` |
| Prepare restoration from native Agent history | `pvisor replay`, dispatched to adjacent `pvisor-replay` | [Chapter 5](05-tools-and-restoration.md), [replay guide](../guides/replay.md) |
| Select an OCI container or VM executor | `pvisor run --executor container` / `--executor vm` | [Containers](../guides/executors/container.md), [VMs](../guides/executors/vm.md); requires a runtime/rootfs |
| Save CPU, RAM, devices, and the complete file tree | Native VM execution checkpoints / snapshot storage SDK | [Execution-checkpoint contract](../reference/cli.md#full-vm-execution-checkpoints); requires a compatible owned-rootfs, no-network Job; not a daemon capability |
| Manage OCI file caching or a shared cold-page pool | `pvisor-cache` / `pvisor-daemon serve --memory-pool` | [Shared image cache](../reference/shared-image-cache.md), [daemon pool](../guides/daemon/index.md#memory-pool) |

Gateway capture requires a build with the gateway feature; wheels and `just build release` include it. Core Job commands work without companion binaries; `pvisor --help` lists installed optional commands in their object group. Full VM snapshots require KVM or Apple Silicon Hypervisor and a working FUSE backend. Ordinary VM Jobs support execution checkpoints with a compatible owned-rootfs, no-network profile; `status --json` reports capability and blockers.

## Execute the documentation

Each case has a Bash block preceded by a single `<!-- semspec: case id=S-USE-001 -->` comment (using that case’s ID), containing product commands, semantic assertions, and failure conditions. Preparation and assertion functions live in the Markdown block below, marked with `<!-- semspec: setup -->`. Case comments also accept timeout=60s, xfail-on=linux, and the paired xfail-reason. semspec extracts only marked code blocks, independently of heading levels, leaving ordinary tutorial examples unexecuted. Copy this directory’s Markdown documents to run them without configuration files or external shell scripts. Supply one or more Markdown files/directories explicitly on the command line; semspec does not infer search paths. Running one chapter also reads preparation from its sibling index.md.

```bash
just semspec list docs/src/zh/cases --domain USE
just semspec lint docs/src/zh/cases
just semspec run docs/src/zh/cases/01-first-job.md
just semspec run docs/src/zh/cases --domain USE --require-pass --subject-bin target/release/pvisor
just cases --suite use
just cases --suite use --case S-USE-005,S-USE-007 --keep
just cases --suite use --output target/pvisor-learning-report.json
```

`just cases --suite use` builds the release product and companions, then executes all 16 cases in isolated temporary workspaces, HOME, XDG, and Job data directories. Linux CI checks FUSE and user/mount/network namespaces first. Missing prerequisites do not become SKIP; environment and execution failures fail the gate. The report is target/pvisor-learning-report.json. Failures retain their workspaces; --keep also retains successful ones. Selected runs require exactly the requested IDs. Full runs discover all S-USE IDs from this documentation directory, automatically including new cases. Empty reports, missing cases, duplicates, SKIP, XFAIL, and any other non-PASS verdict fail the gate. The entry uses semspec’s `--require-pass` directly, sharing the same parsed inventory for discovery, selection, and report validation. With `--output`, valid selection clears an old report before atomically publishing fresh results; no additional validation script is needed.

Specifications and the complete Markdown documents containing preparation participate in semspec digests. New cases remain UNREVIEWED: execution success and human semantic approval are separate. After human review of the engine, vocabulary, and cases, add --require-reviewed. Test execution does not approve cases or update the review ledger.

[DOC cases](../reference/cases.md), [VM control cases](../reference/cases-vm.md), STAGE and USE scenarios run through `just cases`, selected with `--suite doc/stage/use/vm`; `just examples` runs examples/pvisor. CI runs existing isolation regressions and network/Gateway mock scenarios alongside this learning path. Retired standalone snapshot hardware records remain historical evidence. Legacy Controller/Worker acceptance records are not validation of the new daemon. Validate native execution restoration separately against the [execution-checkpoint contract](../reference/cli.md#full-vm-execution-checkpoints); the daemon's [runtime boundaries](../guides/daemon/boundaries.md) do not include VM restore. This learning path does not count unexecuted VM/Gateway capabilities as success.

The full execution gate currently targets Linux. On macOS, select applicable cases individually; loopback policy differs from Linux namespaces, so the host-loopback refusal checked by S-USE-014 is not a macOS guarantee.

## Preparation and assertion functions

These functions are shared by the chapters. For manual execution, install semspec and pvisor, set CASE_ROOT, WS, and SEMSPEC_BIN (the absolute semspec path) in a temporary workspace, copy this block, then run a selected case. The runner supplies these variables and an independent workspace for each case. pvisor resolves through PATH by default; --subject-bin selects a tested binary. Preparation reads no other repository scripts.

<!-- semspec: setup -->
```bash
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

# Independent learning-path fixtures. No simulated Jobs or product internals.
journey_setup() {
  export HOME="$CASE_ROOT/home"
  export XDG_CONFIG_HOME="$HOME/config" XDG_DATA_HOME="$HOME/data" XDG_CACHE_HOME="$HOME/cache"
  export PVISOR_RUN_HOME="$CASE_ROOT/jobs"
  mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$PVISOR_RUN_HOME"
}
pvisor() { command "${SUBJECT_BIN:-pvisor}" "$@"; }
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
```
