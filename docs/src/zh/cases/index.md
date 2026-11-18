# 按任务学习 pVisor

从运行一个可信脚本开始，逐步让 pVisor 暂存文件、记录执行证据、保存提案和探索分支，再为不可信任务声明边界。每一章回答一个具体问题：现在为什么需要这个能力、用哪条命令、执行后应该看到什么。

| 顺序 | 你遇到的问题 | 使用的命令 | 可执行场景 |
|---|---|---|---|
| [1. 首次运行](01-first-session.md) | 如何记录脚本、处理失败和超时？ | `run`、`status`、`--config` | S-USE-001–004 |
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
pvisor spawn --stage "$sandbox/draft" -- /bin/sh -c 'printf proposal > report.txt'
pvisor review "$sandbox/draft" --diff
pvisor inspect "$sandbox/draft" -- /bin/cat report.txt
pvisor apply "$sandbox/draft" --path report.txt
cat report.txt
```

这一步暂存工作区文件，host 文件访问仍然是 ambient。运行不可信 Agent 前继续第四章，检查实际隔离 Evidence，并阅读[执行环境](../guides/executors/index.md)。

## 什么时候用伴随工具

| 需求 | 工具与入口 | 下一步 |
|---|---|---|
| 为真实模型请求路由、捕获轨迹 | `pvisor spawn --gateway-mode capture --gateway-route ...` | [捕获指南](../guides/capture.md)，以及 `mise run examples -- 04-gateway-llm-control` 的本地 mock 请求测试 |
| 从原生 Agent 轨迹准备恢复 | `pvisor replay`，派发到同目录的 `pvisor-replay` | [第五章](05-tools-and-restoration.md)、[回放指南](../guides/replay.md) |
| 选择 OCI 容器或 VM 执行 | `pvisor spawn --executor container` / `--executor vm` | [容器](../guides/executors/container.md)、[VM](../guides/executors/vm.md)；需要对应 runtime/rootfs |
| 保存 CPU、RAM、设备和完整文件树 | 原生 VM execution checkpoint / 快照存储 SDK | [Execution checkpoint 契约](../reference/cli.md#full-vm-execution-checkpoints)；要求兼容的独立 rootfs、无网络 Session，不是 daemon 能力 |
| 管理 OCI 文件缓存或共享冷页池 | `pvisor-daemon serve --cache` / `pvisor-daemon serve --sandbox --memory-pool` | [共享镜像缓存](../reference/shared-image-cache.md)、[daemon 池](../guides/daemon/index.md#memory-pool) |

Gateway 捕获需要启用 `gateway` feature 的构建；wheel 和 `mise run build -- release` 包含该能力。没有安装伴随二进制时，核心 Session 命令仍能使用；`pvisor --help` 按操作对象列出已安装的可选命令。完整 VM 快照需要 KVM 或 Apple Silicon Hypervisor 和可用 FUSE 后端；普通 VM Session 在兼容的独立 rootfs、无网络 profile 下支持 execution checkpoint；用 `status --json` 检查能力和拒绝原因。

## 同一份文档，执行同一套检查

本章保留学习顺序和语义说明；每条场景链接到 `tests/spec/<feature>/S-ID.md` 中唯一的 Scrut 原生 Markdown 检查。测试按功能归属存放。Scrut 0.4.3 负责发现、执行和匹配；少量产品 fixture 管理独立工作区、严格 Bash、超时与子进程清理。检查脚本和准备函数在 `tests/spec/` 下只维护一份。

```bash
mise run cases -- --suite use --list
mise run scrut -- test tests/spec/session/S-USE-001.md
mise run cases -- --suite use
mise run cases -- --suite use --case S-USE-005,S-USE-007 --keep
mise run cases -- --suite use --output target/pvisor-learning-report.json
```

`mise run cases -- --suite use` 构建 release 产品及伴随工具，在独立临时工作区、HOME、XDG 和 Session 数据目录执行全部 16 条场景。Linux CI 预先检查 FUSE 和 user/mount/network namespace。缺少前提会使门禁失败，不算通过。输出 Scrut 原生 JSON 到 `target/pvisor-learning-report.json`；失败保留现场，`--keep` 也保留成功现场。入口按文件名选择 ID，校验原生报告中的源文件集合恰好等于选定集合；空报告、漏项、重复结果、SKIP、XFAIL 和其他非 PASS 不能通过 USE 门禁。合法选择后先清理旧报告，再原子发布本次结果。`--feature stage` 等功能选择会包含该功能下所有历史域的场景。

这些规格与 fixture 仍待人工语义审核；Scrut 通过不等于批准。历史审核台账保持原样。Scrut 没有原生摘要审核机制，审核策略尚待维护者决定；`--require-reviewed` 当前明确拒绝执行，不能悄悄跳过这一门禁。测试不会写入批准记录。

[DOC cases](../reference/cases.md)、[VM 控制 cases](../reference/cases-vm.md)、STAGE 和 USE 场景统一由 `mise run cases` 执行，用 `--suite doc/stage/use/vm` 选择；`examples/pvisor` 由 `mise run examples` 执行。CI 同时运行原有隔离回归、网络/Gateway mock 场景和新学习路线。旧独立 snapshot 的硬件记录保留作历史证据；旧 Controller/Worker 验收记录不验证新 daemon。完整原生执行恢复按 [execution checkpoint 契约](../reference/cli.md#full-vm-execution-checkpoints)单独验证；daemon 的[运行时边界](../guides/daemon/index.md)不包含 VM 恢复；本学习路线没有把未运行的 VM/Gateway 能力算作成功。

全量执行门禁当前以 Linux 为目标。macOS 可以选择适用的场景运行；本机 loopback 的网络策略与 Linux namespace 不同，S-USE-014 的宿主 loopback 拒绝检查不作为 macOS 承诺。

## 准备步骤与检查函数

产品 fixture 为每条场景创建独立的 `CASE_ROOT`、`WS`、HOME、XDG 和 Session 数据目录。检查在新的 `bash -euo pipefail` 中执行，不继承前一条 Scrut 检查的 shell 状态。`--subject-bin` 选择已有待测二进制；直接运行 Scrut 时可通过 `SUBJECT_BIN` 传入绝对路径，否则 USE 场景从 PATH 查找 `pvisor`。

准备与断言源码：[fixtures/use.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/use.bash)。
