# pVisor 选型与使用入口 {#selection}

<span id="pvisor"></span>

使用场景判断已合入 [pVisor 概览](index.md#when-to-use)。首次使用可以直接进入[操作演示](../start/first-run.md)或[基础设施接入指南](../guides/infrastructure/index.md)。原地址保留，便于已有链接继续访问。

<span id="_1"></span>

## 选型依据 {#comparison-evidence}

运行成本与方案选择见[端到端任务](../benchmarks/agent-tasks.md)、[强化学习执行环境](../benchmarks/compare-rl-infra.md)和[执行器边界](../security/executor-boundaries.md)。具体接口支持范围见[接入方式矩阵](../guides/infrastructure/integration.md)。

<span id="_2"></span>

## 使用问题 {#usage-help}

- 接入 Agent 或 CI：[Agent 接入指南](../guides/agents/index.md)、[CI 工作流](../guides/ci.md)。
- 文件暂存、改动接受与 Job 查找：[暂存与 apply](../concepts/staging.md)、[Job 与存储](../concepts/jobs.md)。
- 网络访问与数据边界：[安全概览](../security/index.md)、[网络策略](../guides/policies/network.md)。
- 启动与文件操作开销：[启动延迟](../benchmarks/startup.md)、[文件系统任务](../benchmarks/filesystem.md)。
