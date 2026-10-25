# pVisor 选型与使用入口 {#selection}

<span id="pvisor"></span>

pVisor 适合需要本地受控执行、可审查改动，或希望用现有 SDK 管理本机 VM 沙箱的场景。先按需要的接口选择：

- **工作区改动要审查后合入**：用原生 Job 的暂存、冲突检查和按路径 apply。
- **脚本已有 Docker 命令**：核对实验性 CLI 子集和逐容器 VM 的差异。
- **应用已有 OpenSandbox/E2B SDK**：核对 daemon 已测试的核心协议、Linux/KVM 前提与不支持的 API。
- **需要托管云服务或敌对多租户保证**：服务运营、安全审计与隔离要求需要单独评估；当前本地接口不提供这些保证。

远程 API、数据库写入和已发送消息保留其副作用；文件暂存的可逆范围见[暂存](../concepts/staging.md)。

使用场景判断已合入 [pVisor 概览](index.md#when-to-use)。首次使用可以直接进入[操作演示](../start/first-run.md)或[基础设施接入指南](../guides/infrastructure/index.md)。原地址保留，便于已有链接继续访问。

<span id="_1"></span>

## 选型依据 {#comparison-evidence}

运行成本与方案选择见[端到端任务](../benchmarks/agent-tasks.md)、[强化学习执行环境](../benchmarks/compare-rl-infra.md)和[执行器边界](../security/executor-boundaries.md)。具体接口支持范围见[接入方式矩阵](../guides/infrastructure/integration.md)。

## 与 Anthropic Sandbox Runtime 对照 {#sandbox-runtime}

需要保留本机工具链、限制命令或 MCP Server 的访问范围时，可评估 SRT；需要执行后
审查并选择接受文件改动时，pVisor 的暂存与 Job 记录提供相应流程。

对照依据为 2026-10-25 的源码审阅：SRT
[`c66fb77`](https://github.com/anthropics/sandbox-runtime/tree/c66fb77b28fdbea8b9788aca72377238ae67ca20)
（主分支包版本 `0.0.81`）与 pVisor `b1d8d0c`。未执行同条件性能或攻击测试。

| 维度 | SRT | pVisor |
| --- | --- | --- |
| Linux host 选择性出口 | network namespace 隔离直接网络，经 Unix socket 桥接到宿主代理；忽略代理的客户端通常连接失败 | 选择性代理仍属 cooperative；强制路径需选 VM 或 host 全部拒绝模式 |
| 文件修改 | 在允许写入的路径直接修改 | 启用暂存后支持 review、按路径 apply、冲突检查与 drop |
| HTTPS 与凭据 | 可配置 TLS termination、请求过滤、凭据替身与出口注入 | Gateway 侧重 LLM 协议、供应商凭据路由和轨迹捕获 |
| 状态与恢复 | 进程内代理／配置管理及违规记录；持久化由嵌入方承担 | 持久化 Job、Run Bundle 与受条件约束的 checkpoint/restore |
| 接入 | npm CLI 与 TypeScript 库；另有独立代理接口 | 原生 CLI、Rust runtime 与单节点 daemon；运行 Agent CLI 不要求语言 SDK |

SRT 的机制见其
[Linux 隔离实现](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/linux-sandbox-utils.ts)、
[网络与凭据配置](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/sandbox-config.ts)和
[违规存储](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/sandbox-violation-store.ts)。
这些能力有配置与平台条件；例如 SRT 默认读取规则为空，敏感路径需要显式保护。

pVisor host 与 VM 分别提供不同控制，实际强度见[网络边界](../guides/policies/network.md#网络边界)。
暂存不提升网络隔离，VM 也不自动隐藏显式暴露的宿主文件。比较执行成本时，应匹配
平台、策略与任务，并单独计入暂存和 VM 的语义差异；源码规模和实现语言不能证明
性能或安全性领先。

<span id="_2"></span>

## 使用问题 {#usage-help}

- 接入 Agent 或 CI：[Agent 接入指南](../guides/agents/index.md)、[CI 工作流](../guides/ci.md)。
- 文件暂存、改动接受与 Job 查找：[暂存与 apply](../concepts/staging.md)、[Job 与存储](../concepts/jobs.md)。
- 网络访问与数据边界：[安全概览](../security/index.md)、[网络策略](../guides/policies/network.md)。
- 启动与文件操作开销：[启动延迟](../benchmarks/startup.md)、[文件系统任务](../benchmarks/filesystem.md)。

pVisor 不收集或上传使用统计。Agent 的模型请求、镜像下载、远程缓存与 Gateway 上游请求会按配置访问网络。`--safe` 预设的模型目标规则是否不可绕过，取决于执行器；Linux host 的选择性代理仍可被直接连接绕过。见[网络边界](../guides/policies/network.md#网络边界)。
