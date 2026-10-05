# Cluster 上手：从首个任务到双 Worker

在 Linux 上启动一个 Controller 和两个 Worker，提交 shell 任务、下载证据，再运行依赖图与取消流程。配套 `scripts/cluster-quickstart.py` 使用真实二进制验证这些步骤，并为服务和任务设置资源限制。

| 路径 | 用途 |
| --- | --- |
| 当前指南 | 构建、准备、启动、任务、证据、DAG、取消与清理 |
| [恢复与存储操作](operations.md) | drain、重启对账、失联、配额、证据退休与 GC |
| [VM 与模型 Gateway](vm-and-gateway.md) | 真实 VM 控制/分叉、离线模型接入、正式模型配置 |
| [完整设计](../../design/cluster/index.md) | 状态权威、协议、实现与扩展约束 |

## 前置条件与资源预算 {#prerequisites}

从仓库根目录操作。需要 Linux、Bash、Python 3.11+、Rust/Cargo、just、`/bin/sh`、`/bin/sleep`、cgroup v2 和运行中的用户 systemd manager；构建依赖按[安装](../../start/installation.md)准备。VM 额外需要 x86-64、KVM/FUSE、`ldd` 与明确的固件目录，见[VM 准备](vm-and-gateway.md#vm)。

```bash
python3 --version
cargo --version
just --version
systemctl --user is-system-running
test -f /sys/fs/cgroup/cgroup.controllers
```

用户 manager 应返回 `running`。受限脚本要求 `MemAvailable` 至少 2 GiB，不满足就拒绝启动；它不会取消 cgroup 限制来绕过环境问题。这套已验证路径是 Linux 用户服务部署；macOS、容器内无用户 manager 等环境需单独配置并验证资源控制。

| 对象 | 并发 / 内存 | CPU 与其他界限 |
| --- | --- | --- |
| 构建 | 1 个 Cargo 编译任务；整个构建 scope 3 GiB | 1 核上限，禁用 swap |
| Controller | 256 MiB cgroup 上限 | 0.25 核上限 |
| 每个 Worker 及其全部子进程 | 1 个执行槽位；512 MiB cgroup 上限 | 0.5 核上限，禁用 swap，最多 128 个进程/线程 |
| 每个 host 任务 | 64 MiB 地址空间限制；最多保留 4 KiB 输出 | 每进程 2 秒 CPU 时间，60 秒墙钟超时 |
| 每个 VM | 128 MiB guest RAM、1 vCPU | 所在单槽位 Worker 全进程树仍限 0.5 核；guest 命令 2 秒 CPU 时间 |
| 可选离线模型服务 | 128 MiB cgroup 上限 | 0.1 核上限 |

一套会话最多两个沙箱并行，普通任务示例按步骤等待，实际更低；不要同时启动多套会话。两个 Worker 加 Controller 的内存硬上限合计 1.25 GiB，Gateway 示例增加 128 MiB；launcher、编译器和会话外的系统开销另计。`resources` 是调度预留，实际限制分别来自 `run.runtime.resource_limits` 和 cgroup。host 是可信进程后端；隔离任务使用后面的 VM 路径。

## 构建与一键验证 {#verify}

构建包含可选 Gateway 的二进制，使后面的模型例子使用同一构建：

```bash
systemd-run --user --scope --quiet \
  --property=MemoryMax=3G --property=MemorySwapMax=0 --property=CPUQuota=100% \
  env CARGO_BUILD_JOBS=1 just --tempdir /tmp cluster-build-gateway
```

先完整验证，再进入手工流程。状态目录必须不存在且位于 checkout 外；使用独立的新目录，不复用之前的任务 ID 历史。

```bash
QS_ROOT=$(mktemp -d /tmp/pvisor-quickstart.XXXXXX)
python3 scripts/cluster-quickstart.py verify --backend host --state "$QS_ROOT/verify-host"
cat "$QS_ROOT/verify-host/report.json"
```

预期每项输出 `PASS`，最终 `report.json` 的 `passed` 为 `true`。脚本核对实际内核 `memory.max` / `cpu.max` / `memory.swap.max`、OOM 计数、任务输出、节点身份、幂等、下载、DAG、取消、drain、重启同 key 对账、存储退休与 GC。成功或失败都会尝试停止自己创建的服务，保留状态便于诊断。

验证是实际运行，不是只解析 JSON。每次使用随机 token、端口和 systemd unit 名，复制固定二进制到状态目录，避免并发 Cargo 构建替换 VM 重入程序。端口被其他进程抢占时会明确失败，换一个新目录重试。

### 已保留的通过性记录 {#evidence-results}

2026-10-05 在同一 Linux x86-64 主机顺序实跑，一套停止后才启动下一套；内核 OOM / OOM-kill 计数均为零。峰值为整个 Worker cgroup，包含原生子进程与记入该组的文件缓存，不是 guest RAM 大小或通用性能基准。

| 路径 | 实际检查 | 最大 Worker cgroup 内存峰值 | 记录 |
| --- | --- | --- | --- |
| host | 基础任务、放置、DAG、取消、drain、重启、GC | 约 13 MiB | [host 报告](../../../assets/cluster-quickstart/host-20261005.json) |
| VM | 同上及 pause/offload/resume/suspend、封存分叉恢复 | 约 272 MiB | [VM 报告](../../../assets/cluster-quickstart/vm-20261005.json) |
| Gateway | 基础流程及真实 HTTP、模型授权、凭据隔离 | 约 23 MiB | [Gateway 报告](../../../assets/cluster-quickstart/gateway-20261005.json) |

手工代码块也从 Markdown 提取后按顺序执行，包含资源查询、host 重启标记核对、退休/GC、VM suspended 状态等待与分叉，以及 Gateway 提交/下载/停止；[手工验证记录](../../../assets/cluster-quickstart/walkthrough-20261005.json)保存执行代码块编号与哈希。未使用超过两个并行沙箱；这些记录不代表多主机生产容量或外部模型服务验收。

## 准备并启动手工会话 {#start}

```bash
QS_STATE="$QS_ROOT/manual-host"
python3 scripts/cluster-quickstart.py prepare --backend host --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" workers
```

`prepare` 创建私有配置、任务与图样例、独立 workspace、配额和固定位于 `bin/` 的两个二进制。`env.sh` 和 `session.json` 为 `0600`，只 `source` 自己生成的文件。`start` 用上述硬限制启动三项用户服务；`limits` 从实际 cgroup 文件验证限制并输出 peak / CPU 使用与 OOM 计数。

预期注册 `qs-a` 与 `qs-b`，各 `capacity.slots = 1`、`memory_bytes = 134217728`、`cpu_millis = 500`。节点标签分别为 `quickstart-node=a/b`。Controller 使用 30 秒 lease、64 MiB metadata quota 和 128 MiB artifact quota，避免示例默认容量扩大。

## 提交与解读第一个任务 {#task}

```bash
cat "$QS_STATE/inputs/hello.json"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/hello.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task hello
"$QS_BIN/pvisor-cluster" show hello
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/hello.json"
```

预期最终 `phase = succeeded`、`result.exit_code = 0`、`result.output.stdout = "hello from Cluster\n"`，归属 `lease.key.worker_id = qs-a`。再次提交返回原任务，不执行第二次；相同 ID 修改命令会冲突。想运行不同工作，修改 `id` 与 `run.run_id`，保留旧任务。

关键字段分工：

| 字段 | 含义 |
| --- | --- |
| `execution` | 必须匹配 Worker 后端；host 为 process/host_process |
| `resources` | 调度槽位、RAM/CPU 预留，不是系统硬限制 |
| `run.runtime.resource_limits` | 原生内存/CPU 时间/文件等限制；实际效果读 Bundle |
| `labels` | 节点要求，本例固定为 a |
| `retain_artifacts` | 要求 trace 与 Bundle 交付；VM 例子另外要求私有 upper |
| `reconciliation_pending` | 重启对账尚未完成，phase 只能当历史提示 |

`wait` 默认等聚合终态、最多 60 秒；等待 `running` 等特定状态时若提前失败会立即报错。`submit` 成功只说明意图已接受，不能作为原生成功或产物已上传。

## 验证另一节点并下载证据 {#evidence}

```bash
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/other-node.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task other-node
"$QS_BIN/pvisor-cluster" artifacts hello
"$QS_BIN/pvisor-cluster" artifacts hello --out "$QS_STATE/downloads/manual-hello"
python3 - "$QS_STATE/downloads/manual-hello/run-bundle.json" <<'PY'
import json, pathlib, sys
bundle = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert bundle["run"]["run_id"] == "hello"
print("verified Run ID:", bundle["run"]["run_id"])
PY
```

第二个任务应在 `qs-b` 成功，stdout 为 `node-b\n`。下载目录内应有 `run-bundle.json` 和 `trace`；客户端校验引用与对象完整性，并在下载期间保持保护 lease。目标文件已存在时拒绝覆盖，重复下载换一个新目录。Bundle 的 Worker 本地路径不等于已下载文件。

## DAG 与取消 {#dag}

```bash
"$QS_BIN/pvisor-cluster" graph submit "$QS_STATE/inputs/graph.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task dag-3
"$QS_BIN/pvisor-cluster" graph show quickstart-dag
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/cancel-me.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task cancel-me --phase running
"$QS_BIN/pvisor-cluster" cancel cancel-me
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task cancel-me
```

图的三个节点按前驱聚合成功推进，最终 graph 为 `succeeded`；它不自动传递前驱文件。取消任务先进入 `cancelling`，原生结束与交付收敛后应为 `cancelled`。取消请求被接受不表示停止已经完成。

## 停止、保留与下一步 {#cleanup}

```bash
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

`stop` 只停止这套会话的随机 unit，不删除状态，也不影响其他项目服务。保留目录包含凭据、结果与可能的快照，不要整目录提交或共享；需要共享时只取去除凭据的报告和产物。确认全部服务停止、证据不再需要后再删除自己的临时目录。

接着完成[恢复与存储](operations.md)，或用新的状态目录执行[VM 与 Gateway](vm-and-gateway.md)。固定版本、输入和兼容条件后才扩展到跨主机部署；这里的两个 Worker 是同主机独立服务。
