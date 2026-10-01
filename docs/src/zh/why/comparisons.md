# 与现成方案的对比

总表（细节与出处见 `benchmarks/compare-*`）：

| 方案 | 能做到 | 缺什么 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突拒绝覆盖、选择性合入、证据要自己搭 | 只要可复现环境 |
| Agent 自带沙箱 | 挡住部分命令 | 逐条或整块放行，不跨 Agent／执行器，无可核对记录 | 单会话、低风险 |
| git worktree | 文件层隔离 | 不管网络与凭据，也不产出证据 | 纯文件并行 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行 | 脱离本地工具链与工作区 | 需要远程隔离 |
| Kubernetes / Ray | 调度与编排 | 调度的是进程，不是「有界、可逆、可查」的执行语义 | 已有编排层 |
| gVisor / Firecracker / Kata | 隔离基座 | 不提供执行语义、审查与证据 | 需要更强隔离基座 |

- [对比：Agent 自带沙箱（规划中）](../benchmarks/compare-agent-sandboxes.md)
- [对比：Docker / devcontainer（规划中）](../benchmarks/compare-containers.md)
- [对比：云端沙箱（规划中）](../benchmarks/compare-cloud-sandboxes.md)
- [定位：gVisor / Firecracker / Kata（规划中）](../benchmarks/compare-runtimes.md)
- [对比：Agent RL rollout 基础设施（规划中）](../benchmarks/compare-rl-infra.md)

!!! note "TODO"
    每行补比较日期、各产品版本与出处。
    补「更正」入口（issue 模板）。
    能引用基准的地方引用基准数据。

