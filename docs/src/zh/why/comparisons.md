# 该不该用 pVisor？

pVisor 适合一种情形：**Agent 独立运行，结束后由你审查结果。** 以下情况不需要它：

- **就在眼前跑、全程盯着**：审批和纠错都发生在当下，后置审查没有意义。
- **改动用 git 就能轻松回退**：diff 小、随时 `git checkout` 就干净，暂存和选择性合入省不了多少事。
- **主要副作用在外部**：远程 API、数据库写入、已发出的消息，暂存管不了，回滚得靠别的手段。
- **需要敌对的强隔离**：面对不可信多租户，要的是 VM 级隔离基座，pVisor 只是其中一种执行器。

Agent 独立运行、结束后审查时，用 pVisor 暂存文件改动，核对执行记录，再选择合入的路径。

## 与现成方案对比

| 方案 | 擅长 | 需另行配置或集成 | 什么时候选它 |
| --- | --- | --- | --- |
| Docker / devcontainer | 隔离环境、依赖可复现 | 改动审查、冲突拒绝覆盖、选择性合入、证据 | 只需要可复现环境 |
| Agent 自带沙箱 | 系统沙箱、权限审批与该客户端的会话记录 | 跨客户端的 stage/apply 与能力记录需要额外工具 | 优先采用客户端原生保护 |
| git worktree | 文件层并行 | 网络与凭据、执行证据 | 纯文件并行 |
| 云端沙箱（E2B、Daytona、Modal） | 远程隔离执行 | 本地工作区集成与本地审查链路 | 需要远程隔离 |
| Kubernetes / Ray | 调度与编排 | 任务执行边界、文件暂存与执行记录 | 已有编排层 |
| gVisor / Firecracker / Kata | 隔离基座 | 执行语义、审查与证据 | 需要更强隔离基座 |

worktree、patch 检查与合并流程可以继续和 Docker、`git diff` 一起用。通过 bind mount 写入的内容需要自行恢复。pVisor 增加暂存、按路径提交、preimage 冲突检查和能力观察，运行成本见实测。

按整条任务流程比较额外成本：[端到端任务](../benchmarks/agent-tasks.md)用修复、文件工具、工作区审查和 CLI 兼容性的实测，比较 Agent 沙箱、Docker/devcontainer、VM 与云端沙箱；[强化学习训练](../benchmarks/compare-rl-infra.md)用活跃容量和回放数据，说明执行层如何影响 rollout 的环境预算。云端费用、默认双层 Agent 沙箱和端到端训练吞吐尚无同条件对照。

## 常见问题

**和 Agent 自带的沙箱有什么不同？**
自带沙箱解决「能不能挡住」。pVisor 在此之上提供事后选择性合入、冲突保护和可核对的执行记录，并且跨 Agent、跨执行器保持一致语义（见上表）。

**会不会拖慢 Agent？**
pVisor 增加的是准入、文件拦截和记录的开销。启动与文件系统开销的实测见[启动延迟](../benchmarks/startup.md)与[文件系统任务](../benchmarks/filesystem.md)。

**能在 CI 里跑吗？**
可以。今天就能按 L1 方式放进流水线：让 Agent 跑完、审查、只合入想要的。策略与证据驱动的免审和集群化（L2/L3）是方向，见[在 CI 中运行 Agent](../guides/ci.md)。

**支持哪些 Agent？**
任何命令都能跑；对 Claude Code、Codex 等提供按可执行文件名匹配的预设。见[接入你的 Agent](../guides/agents/index.md)。

**数据会离开本机吗？**
pVisor 自己不收集、也不上传使用数据。Agent 调用模型 API 的流量本来就会出网：`--safe` 下只放行该 Agent 的模型 API 白名单，其余出口按策略处理。只有启用 Gateway 捕获或使用远程镜像缓存服务时，才会引入 pVisor 自己的网络行为。见[安全概览](../security/index.md)。

**`last` 为什么找不到我刚跑的 Job？**
用 `--stage PATH` 时 Job 存在暂存目录里，`last` 只在默认存储中按工作区查找。把路径或 Job ID 显式传进去即可。见[Job 与存储](../concepts/jobs.md)。

**暂存能撤销外部副作用吗？**
不能。暂存只覆盖文件；外部 API 调用、数据库写入、已发出的消息不会因此回滚。见[暂存与 apply 语义](../concepts/staging.md)。
