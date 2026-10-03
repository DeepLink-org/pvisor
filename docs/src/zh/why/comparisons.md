# 与现成方案对比

| 方案 | 擅长 | 不解决 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突拒绝覆盖、选择性合入、证据 | 只需要可复现环境 |
| Agent 自带沙箱 | 系统沙箱、权限审批与该客户端的会话记录 | 跨客户端统一 stage/apply 与执行器能力记录需另行组织 | 优先采用客户端原生保护 |
| git worktree | 文件层并行 | 网络与凭据、执行证据 | 纯文件并行 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行 | 本地工作区集成与本地审查链路 | 需要远程隔离 |
| Kubernetes / Ray | 调度与编排 | 调度的是 Pod 和进程，不是「有界、可逆、可查」的执行语义 | 已有编排层 |
| gVisor / Firecracker / Kata | 隔离基座 | 执行语义、审查与证据 | 需要更强隔离基座 |

最常见的反驳是「Docker 加 `git diff` 就够了」。已有 worktree、patch 检查和合并协议时确实可以够用。仅看 diff 不会撤回已写入 bind mount 的内容；pVisor 把暂存、按路径提交、preimage 冲突和能力观察统一成运行协议，代价见实测。

第一版对比于 2026-10-04 核对官方文档；本机 Podman/CLI 实测和云端未测范围分别标注。来源、配置与适用边界见：[Agent 自带沙箱](../benchmarks/compare-agent-sandboxes.md) · [Docker/devcontainer](../benchmarks/compare-containers.md) · [云端沙箱](../benchmarks/compare-cloud-sandboxes.md) · [隔离基座](../benchmarks/compare-runtimes.md) · [RL 基础设施](../benchmarks/compare-rl-infra.md)。
