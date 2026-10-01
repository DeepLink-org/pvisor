# 与现成方案的对比

本页是对比的唯一总表；README 和"PolicyVisor 是什么"只引用它。每个方案都写了它擅长什么、什么时候应该选它。

| 方案 | 能做到 | 缺什么 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突时拒绝覆盖、按路径选择性合入、实际生效限制的证据都要自己搭 | 只需要可复现的环境 |
| Docker + `git diff` | 隔离加改动查看 | 不保护你在运行期间的修改；分批合入、中断恢复、网络与文件控制的证据仍需自建 | 改动小、你本来就逐行审查 |
| Agent 自带沙箱 | 挡住部分命令或路径 | 逐条或整块放行；只对该 Agent 有效，不跨执行器；没有可核对的记录 | 单会话、低风险、你在旁边看着 |
| git worktree | 文件层隔离，便于并行 | 不管网络与凭据，也不产出证据 | 纯文件层面的并行实验 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行、弹性扩容 | 脱离本地工具链与工作区，本地审查链路缺失 | 需要远程隔离或大规模弹性资源 |
| Kubernetes / Ray | 调度与编排 | 调度的是进程或容器，不提供"有界、可逆、可查"的执行语义 | 已有编排层；未来可与 pVisor 配合 |
| gVisor / Firecracker / Kata | 更强的隔离基座 | 不提供暂存、选择性合入与证据 | 需要更强的隔离；可作为执行器后端 |

## 逐项对比（建设中）

下面的页面会给出带日期、版本和出处的详细对比，并尽量引用[基准](../benchmarks/index.md)数据：

- [Agent 自带沙箱（规划中）](../benchmarks/compare-agent-sandboxes.md)
- [Docker / devcontainer（规划中）](../benchmarks/compare-containers.md)
- [云端沙箱（规划中）](../benchmarks/compare-cloud-sandboxes.md)
- [gVisor / Firecracker / Kata（规划中）](../benchmarks/compare-runtimes.md)
- [Agent RL rollout 基础设施（规划中）](../benchmarks/compare-rl-infra.md)

!!! note "待补充：日期与出处"
    总表中关于其他产品的描述尚未逐条标注比较日期、产品版本与出处。发现不准确之处，请[提交 issue](https://github.com/DeepLink-org/pvisor/issues) 更正。
