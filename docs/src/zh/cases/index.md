# 按任务学习 pVisor

从运行一个可信脚本开始，逐步让 pVisor 暂存文件、记录执行证据、保存提案和探索分支，再为不可信任务声明边界。每一章回答一个具体问题：现在为什么需要这个能力、用哪条命令、执行后应该看到什么。

| 顺序 | 你遇到的问题 | 使用的命令 | 可执行场景 |
|---|---|---|---|
| [1. 首次运行](01-first-job.md) | 如何记录脚本、处理失败和超时？ | `run`、`status`、`--config` | S-USE-001–004 |
| [2. 审查与决定](02-review-and-decide.md) | Agent 改了哪些文件，哪些可以接受？ | `review`、`inspect`、`apply`、`drop`、`kill` | S-USE-005–008 |
| [3. 保存与分支](03-checkpoint-and-fork.md) | 如何保留方案，比较两个独立尝试？ | `checkpoint create/list/show`、`fork` | S-USE-009–011 |
| [4. 声明边界](04-boundaries.md) | 如何限制文件访问和网络连接？ | `--safe`、`--access`、`--overlaynet-deny-all` | S-USE-012–014 |
| [5. 轨迹与恢复](05-tools-and-restoration.md) | 该恢复文件、Agent 历史还是整个 VM？ | `replay`、capability 查询 | S-USE-015–016 |

完成五章后，可继续[验证暂存、应用与丢弃契约](06-stage-apply.md)，运行 14 条 STAGE 检查；USE 门禁只选择前五章的 S-USE 场景。

## 先走完一次文件审查

安装后，在一个测试目录执行下面的命令。`--stage` 使用工作区外的绝对路径，避免把运行记录当成项目文件。命令结束后原目录没有 report.txt；inspect 可以读取提案，apply 后原目录才出现文件。

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

这一步暂存工作区文件，host 文件访问仍然是 ambient。运行不可信 Agent 前继续第四章，检查实际隔离 Evidence，并阅读[执行环境](../guides/executors/index.md)。

## 什么时候用伴随工具

| 需求 | 工具与入口 | 下一步 |
|---|---|---|
| 在终端中审查、审批路径访问 | `pvisor run --tui` / `--ask`，派发到同目录的 `pvisor-tui` | [CLI 参考](../reference/cli.md) |
| 为真实模型请求路由、捕获轨迹 | `pvisor run --gateway-mode capture --gateway-route ...` | [捕获指南](../guides/capture.md)，以及 `just examples 04-gateway-llm-control` 的本地 mock 请求测试 |
| 从原生 Agent 轨迹准备恢复 | `pvisor replay`，派发到同目录的 `pvisor-replay` | [第五章](05-tools-and-restoration.md)、[回放指南](../guides/replay.md) |
| 选择 OCI 容器或 VM 执行 | `pvisor run --executor container` / `--executor vm` | [容器](../guides/executors/container.md)、[VM](../guides/executors/vm.md)；需要对应 runtime/rootfs |
| 保存 CPU、RAM、设备和完整文件树 | 原生 VM execution checkpoint / 快照存储 SDK | [Execution checkpoint 契约](../reference/cli.md#full-vm-execution-checkpoints)；要求兼容的独立 rootfs、无网络 Job，不是 daemon 能力 |
| 管理 OCI 文件缓存或共享冷页池 | `pvisor-cache` / `pvisor-daemon serve --memory-pool` | [共享镜像缓存](../reference/shared-image-cache.md)、[daemon 池](../guides/daemon/index.md#memory-pool) |

Gateway 捕获需要启用 `gateway` feature 的构建；wheel 和 `just build release` 包含该能力。没有安装伴随二进制时，核心 Job 命令仍能使用；`pvisor --help` 按操作对象列出已安装的可选命令。完整 VM 快照需要 KVM 或 Apple Silicon Hypervisor 和可用 FUSE 后端；普通 VM Job 在兼容的独立 rootfs、无网络 profile 下支持 execution checkpoint；用 `status --json` 检查能力和拒绝原因。

## 同一份文档，执行同一套检查

每条场景的 Bash 检查块前用一行 `<!-- semspec: case id=S-USE-001 -->` 标记（ID 按场景填写）：产品命令、语义断言和失败条件放在一起。准备步骤和检查函数都在下方 Markdown 代码块中，用 `<!-- semspec: setup -->` 标记。case 注释还可填写 `timeout=60s`、`xfail-on=linux` 和配对的 `xfail-reason`。semspec 自动抽取标记后的代码块，标题层级与测试发现无关；普通教程示例不会被执行；复制本目录的 Markdown 文档即可运行，不需要配置文件或外部 shell 脚本。命令行必须明确提供一个或多个 Markdown 文件/目录；semspec 不推断搜索路径。单独运行一章时，自动读取同目录 index.md 的准备块。

```bash
just semspec list docs/src/zh/cases --domain USE
just semspec lint docs/src/zh/cases
just semspec run docs/src/zh/cases/01-first-job.md
just semspec run docs/src/zh/cases --domain USE --require-pass --subject-bin target/release/pvisor
just cases --suite use
just cases --suite use --case S-USE-005,S-USE-007 --keep
just cases --suite use --output target/pvisor-learning-report.json
```

`just cases --suite use` 构建 release 产品及伴随工具，在独立临时工作区、HOME、XDG 和 Job 数据目录执行全部 16 条场景。Linux CI 预先检查 FUSE 和 user/mount/network namespace。此入口不把缺少前提当成 SKIP；环境或实际执行失败会使门禁失败。输出 `target/pvisor-learning-report.json`，失败保留现场，`--keep` 保留成功现场。逐条选择时门禁检查选定 ID 的精确集合；全量执行时从本目录文档发现全部 S-USE ID，新增场景自动纳入门禁。空报告、漏项、重复 ID、SKIP、XFAIL 或非 PASS 都不能通过。入口直接使用 semspec 的 `--require-pass`；发现、选择和报告校验使用同一份解析结果。指定 `--output` 时，选择校验通过后先清理旧报告，再原子写入本次结果；不依赖额外的验证脚本。

规格和包含准备块的完整 Markdown 文档都参与 semspec 摘要。新规格保持 UNREVIEWED；执行成功与人工语义批准是两件事。人工完成引擎、词汇和规格审核后，可增加 `--require-reviewed`。测试执行不会批准规格或更新审核账本。

[DOC cases](../reference/cases.md)、[VM 控制 cases](../reference/cases-vm.md)、STAGE 和 USE 场景统一由 `just cases` 执行，用 `--suite doc/stage/use/vm` 选择；`examples/pvisor` 由 `just examples` 执行。CI 同时运行原有隔离回归、网络/Gateway mock 场景和新学习路线。旧独立 snapshot 的硬件记录保留作历史证据；旧 Controller/Worker 验收记录不验证新 daemon。完整原生执行恢复按 [execution checkpoint 契约](../reference/cli.md#full-vm-execution-checkpoints)单独验证；daemon 的[运行时边界](../guides/daemon/boundaries.md)不包含 VM 恢复；本学习路线没有把未运行的 VM/Gateway 能力算作成功。

全量执行门禁当前以 Linux 为目标。macOS 可以选择适用的场景运行；本机 loopback 的网络策略与 Linux namespace 不同，S-USE-014 的宿主 loopback 拒绝检查不作为 macOS 承诺。

## 准备步骤与检查函数

下面的函数供各章共用。手动执行时，先安装 semspec 和 pvisor，在临时目录设置 `CASE_ROOT`、`WS` 和 `SEMSPEC_BIN`（semspec 的绝对路径），复制此块，再执行选定场景。自动执行时 runner 为每个场景提供这些变量和独立工作区；`pvisor` 默认从 PATH 查找，也可用 `--subject-bin` 选择待测二进制。准备块不读仓库中的其他脚本。

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
