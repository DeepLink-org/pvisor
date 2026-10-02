# 与现成方案对比

| 方案 | 擅长 | 不解决 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突拒绝覆盖、选择性合入、证据 | 只需要可复现环境 |
| Agent 自带沙箱 | 挡住部分命令 | 逐条或整块放行；不跨 Agent／执行器；无可核对记录 | 单会话、低风险 |
| git worktree | 文件层并行 | 网络与凭据、执行证据 | 纯文件并行 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行 | 本地工作区集成与本地审查链路 | 需要远程隔离 |
| Kubernetes / Ray | 调度与编排 | 调度的是 Pod 和进程，不是「有界、可逆、可查」的执行语义 | 已有编排层 |
| gVisor / Firecracker / Kata | 隔离基座 | 执行语义、审查与证据 | 需要更强隔离基座 |

最常见的反驳是「Docker 加 `git diff` 就够了」。它能看出改了什么，但给不了冲突时拒绝覆盖、按路径选择性合入，也没有「实际生效了哪些限制」的证据——这些正是 pVisor 补的部分。

竞品描述暂未标注版本与日期，等各对比页补齐后回填；逐行的比较日期、版本与出处见各对比页（建设中）：[Agent 自带沙箱](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [云端沙箱](../benchmarks/compare-cloud-sandboxes.md) · [隔离基座](../benchmarks/compare-runtimes.md) · [RL 基础设施](../benchmarks/compare-rl-infra.md)。
