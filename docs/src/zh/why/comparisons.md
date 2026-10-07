# 与现成方案对比

| 方案 | 擅长 | 需另行配置或集成 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突拒绝覆盖、选择性合入、证据 | 只需要可复现环境 |
| Agent 自带沙箱 | 系统沙箱、权限审批与该客户端的会话记录 | 跨客户端统一 stage/apply 与执行器能力记录需另行组织 | 优先采用客户端原生保护 |
| git worktree | 文件层并行 | 网络与凭据、执行证据 | 纯文件并行 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行 | 本地工作区集成与本地审查链路 | 需要远程隔离 |
| Kubernetes / Ray | 调度与编排 | 任务执行边界、文件暂存与执行记录 | 已有编排层 |
| gVisor / Firecracker / Kata | 隔离基座 | 执行语义、审查与证据 | 需要更强隔离基座 |

已有 worktree、patch 检查和合并流程时，可以继续使用 Docker 与 `git diff`。通过 bind mount 写入的内容需要自行恢复。pVisor 提供暂存、按路径提交、preimage 冲突检查和能力观察，运行成本见实测。

第一版对比于 2026-10-04 核对官方文档；本机 Podman/CLI 实测和云端未测范围分别标注。来源、配置与适用边界见：[Agent 自带沙箱](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [云端沙箱](../benchmarks/compare-cloud-sandboxes.md) · [隔离基座](../benchmarks/compare-runtimes.md) · [RL 基础设施](../benchmarks/compare-rl-infra.md)。
