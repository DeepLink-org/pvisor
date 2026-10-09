# 命令与场景参考

把一次任务接入脚本时，分别处理启动参数、执行结果和文件接受决定。普通 host 运行直接写入工作区；需要评审后再接受文件时，使用 `--safe`、`--ask` 或显式 `--stage`。

## 从运行到接受文件

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

运行结束后，`report.txt` 留在暂存区；`inspect` 读取提案，`apply` 才接受所选文件。不接受时用 `drop` 丢弃剩余暂存改动。运行与 apply 的退出码分别检查，不能因为任务返回 0 就自动接受文件；冲突和失败处理见[退出码与错误](exit-codes.md)。参数语法与存储默认值由 [CLI 参考](cli.md)定义。

## 读取记录与使用协议

`status --json` 提供状态概况；`status --review --json` 提供完整 Run Bundle。自动化先识别格式版本和必需字段，再检查执行状态、实际控制与净文件改动；缺失观察不能当成零或通过。字段与查询分别见 [Run Bundle](run-bundle.md)和[机器可读输出](json-output.md)。

共享镜像缓存的 handle、认证和失败行为遵循[缓存协议](shared-image-cache.md)，与 Job 的暂存和 apply 生命周期分开。完整机器状态的保存与恢复还需要对应能力；workspace checkpoint 只保存文件提案，不继续旧进程内存，使用前按 [CLI 的 execution checkpoint 契约](cli.md#full-vm-execution-checkpoints)检查支持条件。

## 核对可执行行为

[场景附录](cases.md)保留产品命令、语义声明与断言，也是 semspec 的 DOC 规格源。代码块中的 runner 夹具不能直接当作普通 shell 命令运行；仓库入口 `just cases` 执行对应检查。检查成功与人工批准分别记录，不因 PASS 改写人工审核账本。

## 场景检查准备

下列 Markdown 函数供场景附录和 VM 检查共用。semspec 自动抽取标记的准备块，每个 case 使用独立 CASE_ROOT、WS 和显式 SUBJECT_BIN；准备与断言全部参与审核摘要。

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
