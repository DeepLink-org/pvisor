# 按任务学习 pVisor

从运行一个可信脚本开始，逐步让 pVisor 暂存文件、记录执行证据、保存提案和探索分支，再为不可信任务声明边界。每一章回答一个具体问题：现在为什么需要这个能力、用哪条命令、执行后应该看到什么。

| 顺序 | 你遇到的问题 | 使用的命令 | 可执行场景 |
|---|---|---|---|
| [1. 首次运行](01-first-job.md) | 如何记录脚本、处理失败和超时？ | `run`、`status`、`--config` | S-USE-001–004 |
| [2. 审查与决定](02-review-and-decide.md) | Agent 改了哪些文件，哪些可以接受？ | `review`、`inspect`、`apply`、`drop`、`kill` | S-USE-005–008 |
| [3. 保存与分支](03-checkpoint-and-fork.md) | 如何保留方案，比较两个独立尝试？ | `checkpoint create/list/show`、`fork` | S-USE-009–011 |
| [4. 声明边界](04-boundaries.md) | 如何限制文件访问和网络连接？ | `--safe`、`--access`、`--overlaynet-deny-all` | S-USE-012–014 |
| [5. 轨迹与恢复](05-tools-and-restoration.md) | 该恢复文件、Agent 历史还是整个 VM？ | `replay`、capability 查询 | S-USE-015–016 |

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

每条 S-USE 场景只有一个 Bash 检查块：产品命令、语义断言和失败条件放在一起。代码块中的 `journey_setup`、`json_expect` 等是仓库 runner 的夹具和检查函数，不能直接复制到普通 shell；学习时关注 `pvisor` 命令，完整验收使用下面的入口。

```bash
just semspec --config semspec-use.toml list --domain USE
just semspec --config semspec-use.toml lint
just cases-v2
just cases-v2 --case S-USE-005,S-USE-007 --keep
python3 scripts/cases/run.py --subject-bin target/release/pvisor --output target/pvisor-learning-report.json
```

`just cases-v2` 构建 release 产品及伴随工具，在独立临时工作区、HOME、XDG 和 Job 数据目录执行全部 16 条场景。Linux CI 预先检查 FUSE 和 user/mount/network namespace。此入口不把缺少前提当成 SKIP；环境或实际执行失败会使门禁失败。输出 `target/pvisor-learning-report.json`，失败保留现场，`--keep` 保留成功现场。逐条选择时门禁检查选定 ID 的精确集合；全量执行时从本目录文档发现全部 ID，新增场景自动纳入门禁。空报告、漏项、重复 ID、SKIP、XFAIL 或非 PASS 都不能通过。

规格、夹具中的 Python 断言和 Bash 词汇都参与 semspec 摘要。新规格保持 UNREVIEWED；执行成功与人工语义批准是两件事。人工完成引擎、词汇和规格审核后，可增加 `--require-reviewed`。测试执行不会批准规格或更新审核账本。

旧的 [DOC cases](../reference/cases.md)、[VM 控制 cases](../reference/cases-vm.md)、`just cases`、`just vm-cases` 和 `examples/pvisor` 保留原入口。CI 同时运行原有隔离回归、网络/Gateway mock 场景和新学习路线。旧独立 snapshot 的硬件记录保留作历史证据；旧 Controller/Worker 验收记录不验证新 daemon。完整原生执行恢复按 [execution checkpoint 契约](../reference/cli.md#full-vm-execution-checkpoints)单独验证；daemon 的[运行时边界](../guides/daemon/boundaries.md)不包含 VM 恢复；本学习路线没有把未运行的 VM/Gateway 能力算作成功。

全量执行门禁当前以 Linux 为目标。macOS 可以选择适用的场景运行；本机 loopback 的网络策略与 Linux namespace 不同，S-USE-014 的宿主 loopback 拒绝检查不作为 macOS 承诺。
