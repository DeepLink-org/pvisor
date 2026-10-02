# pVisor 文档信息架构设计（提案）

状态：提案，待评审。采纳后，第 2、3、8 节并入 `docs/README.md` 的 Information architecture 一节，本文件删除。

## 0. 目标与对标

文档要同时完成三件事：

1. **让人愿意试**：五分钟内理解愿景、自己能得到什么，以及和现成方案的差别。
2. **让人敢用**：保证的范围、威胁模型、基准数据和语义规格都可以核对。
3. **让人愿意参与**：架构、设计决策、研究方向和贡献路径清楚可见。

对标项目，以及从每个项目借鉴的做法：

| 项目 | 借鉴的做法 |
| --- | --- |
| uv / Ruff | 首屏放基准对比图；"为什么快"有独立页面 |
| gVisor / Firecracker | 安全模型、威胁模型和"不保护什么"有独立栏目；有生产使用者列表 |
| Ray / vLLM | 用例按场景组织；架构与论文入口清楚；有路线图和社区栏目 |
| Tailscale / Kubernetes | 概念页只讲用户心智模型，实现细节进入设计栏目 |
| Diátaxis 框架 | 教程、任务指南、参考、解释四类内容分开 |

## 1. 设计原则

1. **价值先于机制，机制先于实现。** 栏目顺序就是读者的认知顺序：为什么 → 开始 → 指南 → 概念 → 参考 → 设计。
2. **每个主题只有一篇权威页面。** 保证范围只在"能力与证据"和"安全模型"两页定义，其他页面一句话加链接。
3. **按读者分路。** 用户、平台工程师、安全评审、贡献者与研究者各有入口，互不混杂（见第 2 节）。
4. **愿景与现状分开写。** 每个"方向"类内容都标注所处的信任阶梯级别（L1/L2/L3）。
5. **用 TODO 页面提需求。** 应有但还没有的页面先建占位，写清需求和验收标准（见第 8 节），由研发认领补齐。
6. **可核对。** 基准写明方法和环境，可以复现；对比写明日期和版本，接受更正；行为声明尽量对应 semspec 用例。

> 与现行规则的冲突：`docs/README.md` 规定"推测性设计不进入当前产品导航与搜索"。本提案需要把它改为：**TODO 页可以进入导航，但必须带"规划中"标识，并从站内搜索中排除**（Zensical 的 `search: exclude: true` front matter）。

## 2. 读者与入口

| 读者 | 想知道 | 入口 | 主路径 |
| --- | --- | --- | --- |
| 使用 Agent 的开发者 | 能不能放心让 Agent 全自动跑 | 首页 | 为什么 → 第一次运行 → 接入你的 Agent → 审查与合入 |
| 平台或 DevOps 工程师 | 能否接入 CI 和团队流水线，开销多大 | 首页 → 用例 | 用例 → 基准 → CI 集成 → 配置参考 |
| 安全评审 | 边界在哪里，什么不保护 | 安全 | 威胁模型 → 执行器边界矩阵 → 能力与证据 → 漏洞披露 |
| 研究者（MLSys、Agentic RL） | 能否作为 rollout 和评测的执行基座 | 用例 → 研究 | 研究与训练用例 → 回放与分叉 → 架构 → 研究方向 |
| 贡献者 | 代码怎么组织，怎么验证 | 社区 | 贡献指南 → 开发环境 → 测试与 semspec → 架构 |

## 3. 顶层栏目

Zensical 的顶部 tabs 不超过 9 个：

| 栏目 | 目录 | Diátaxis 类型 | 主要读者 |
| --- | --- | --- | --- |
| 首页 | `index.md` | — | 全部 |
| 为什么 | `why/` | 解释 | 新读者、决策者 |
| 开始使用 | `start/` | 教程 | 新用户 |
| 指南 | `guides/` | 任务指南 | 用户、平台工程师 |
| 概念 | `concepts/` | 解释 | 用户 |
| 基准与对比 | `benchmarks/` | 证据 | 决策者、平台、研究者 |
| 安全 | `security/` | 解释与政策 | 安全评审 |
| 参考 | `reference/` | 参考 | 全部 |
| 设计与研究 | `design/` | 解释 | 贡献者、研究者 |
| 社区 | `community/` | 政策与流程 | 贡献者 |

与现状相比的变化：新增"为什么""基准与对比""安全""社区"四个栏目；"参与开发"并入"社区"；"实现设计"更名为"设计与研究"，并承接从概念页移出的实现概念。

## 4. 完整目录树

标记说明：

- `[现有]` 内容基本保留
- `[改写]` 页面已有，需按本设计改写或拆分
- `[新建]` 需要现在写，现有内容足够支撑
- `[TODO]` 需要研发产出数据或功能，先建占位页提需求

```text
zh/
├── index.md                          [改写] 首页
├── why/
│   ├── index.md                      [新建] 为什么是 pVisor：愿景、监督带宽论证、三种性质
│   ├── trust-ladder.md               [新建] 信任阶梯与规模轴：L0–L3 × 单机/并行/集群
│   ├── use-cases.md                  [新建] 按规模展开的用例：个人、多 Agent、团队与平台、研究与训练
│   ├── comparisons.md                [新建] 与现成方案对比总表（细节在 benchmarks/compare-*）
│   ├── when-not-to-use.md            [新建] 什么时候不需要 pVisor
│   └── faq.md                        [新建] 常见问题
├── start/
│   ├── index.md                      [改写] 从这里开始：三条路径（试一下 / 接入 Agent / 接入 CI）
│   ├── what-is-pvisor.md             [改写] 一页说明，与 why/index 去重，只保留"是什么 + 今天能做什么"
│   ├── installation.md               [现有] 安装与平台要求
│   ├── first-run.md                  [现有] 假 Agent 演示：拦截、审查、选择性合入
│   └── next-steps.md                 [新建] 第一次运行之后：按读者推荐下一页
├── guides/
│   ├── index.md                      [改写] 按任务索引
│   ├── agents/
│   │   ├── index.md                  [新建] Agent 接入总览与兼容矩阵
│   │   ├── claude-code.md            [新建] Claude Code
│   │   ├── codex.md                  [新建] Codex CLI
│   │   ├── gemini-cli.md             [TODO] Gemini CLI
│   │   ├── aider.md                  [TODO] aider
│   │   ├── opencode.md               [TODO] OpenCode
│   │   └── custom-scripts.md         [新建] 任意脚本与自动化命令
│   ├── review-apply.md               [现有] 审查、选择性合入、丢弃、冲突处理
│   ├── fork-checkpoint.md            [新建] 逻辑检查点与分叉（从 review-apply 拆出）
│   ├── policies/
│   │   ├── files.md                  [新建] 文件策略：只读、拒绝、敏感路径预设
│   │   ├── network.md                [改写] 网络策略（现 guides/network.md）
│   │   └── credentials.md            [新建] 凭据与环境变量投影
│   ├── executors/
│   │   ├── index.md                  [改写] 选择执行器（现 guides/execution.md）
│   │   ├── host.md                   [新建] host：Linux namespace/Landlock，macOS Seatbelt
│   │   ├── container.md              [新建] 原生 OCI 容器
│   │   └── vm.md                     [新建] libkrun VM
│   ├── capture.md                    [现有] Gateway 与模型流量捕获
│   ├── replay.md                     [改写] 回放（现 sandbox-replay.md）
│   ├── ci.md                         [TODO] 在 CI 中运行 Agent（GitHub Actions 示例）
│   ├── parallel-agents.md            [TODO] 单机多 Agent 并行与批量审查（L2）
│   ├── rl-rollouts.md                [TODO] 作为 Agentic RL rollout 与评测的执行层
│   └── troubleshooting.md            [现有] 故障排查
├── concepts/
│   ├── index.md                      [改写] 用户心智模型总览（一张图）
│   ├── jobs.md                       [改写] Job 与存储：Job、Run ID、last 的解析规则（从 run-model 拆出用户部分）
│   ├── staging.md                    [新建] 暂存与 apply 语义：事务、冲突拒绝、幂等、哪些不可逆
│   ├── capabilities-and-evidence.md  [现有] 能力、证据与保证边界（权威页）
│   ├── policy-model.md               [新建] 策略模型：请求、准入、降级、--safe/--strict
│   └── glossary.md                   [新建] 术语表
├── benchmarks/
│   ├── index.md                      [新建] 总览：首屏数字、方法、环境声明
│   ├── methodology.md                [新建] 方法、环境与复现（对应 benchmark/）
│   ├── startup.md                    [改写] 启动延迟（已有 guest init 数据，迁入并补全）
│   ├── filesystem.md                 [TODO] 文件系统开销
│   ├── network.md                    [TODO] 网络开销
│   ├── apply.md                      [TODO] apply/drop 成本与崩溃一致性
│   ├── agent-tasks.md                [TODO] 端到端 Agent 任务开销
│   ├── supervision-cost.md           [TODO] 监督成本（愿景指标）
│   ├── density.md                    [TODO] 单机并发密度与资源占用
│   ├── isolation-tests.md            [TODO] 隔离有效性测试结果
│   ├── replay-fidelity.md            [改写] 回放保真度（已有 benchmark/replay 数据）
│   ├── compare-agent-sandboxes.md    [TODO] 对比：Agent 自带沙箱
│   ├── compare-containers.md         [TODO] 对比：Docker / devcontainer
│   ├── compare-cloud-sandboxes.md    [TODO] 对比：E2B / Daytona / Modal
│   ├── compare-runtimes.md           [TODO] 定位：gVisor / Firecracker / Kata
│   └── compare-rl-infra.md           [TODO] 对比：Agent RL rollout 基础设施
├── security/
│   ├── index.md                      [新建] 安全概览：保护什么、不保护什么
│   ├── threat-model.md               [新建] 威胁模型：攻击者、资产、信任边界
│   ├── executor-boundaries.md        [改写] 执行器边界矩阵（从 network guide 与 isolation 汇总）
│   ├── hardening.md                  [新建] 加固建议
│   ├── known-limitations.md          [新建] 已知限制与不变量缺口（如 macOS ln -s EPERM）
│   ├── disclosure.md                 [新建] 漏洞披露政策（与仓库根 SECURITY.md 同源）
│   └── audits.md                     [TODO] 第三方审计与渗透测试结果
├── reference/
│   ├── index.md                      [现有]
│   ├── cli.md                        [现有] CLI 参考
│   ├── config.md                     [新建] 配置文件参考（RunConfig TOML）
│   ├── policy.md                     [新建] 策略字段与预设参考
│   ├── run-bundle.md                 [新建] Run Bundle schema（当前版本 4）
│   ├── json-output.md                [新建] status --json 等机器可读输出
│   ├── env-vars.md                   [新建] PVISOR_* 环境变量
│   ├── exit-codes.md                 [新建] 退出码与错误类型
│   ├── platforms.md                  [新建] 平台与执行器支持矩阵、成熟度等级
│   ├── stability.md                  [新建] 稳定性与兼容承诺（CLI、schema、版本策略）
│   ├── cases.md                      [现有] 语义用例（S-DOC，semspec 语义源）
│   └── shared-image-cache.md         [改写] 现在导航里有，但 src 下没有此文件（从 docs/shared-image-cache.md 迁入）
├── design/
│   ├── index.md                      [改写] 设计总览与阅读顺序
│   ├── principles.md                 [现有]
│   ├── architecture.md               [现有]
│   ├── execution-model.md            [改写] Operation、Attempt、Session（从 concepts/run-model 移入）
│   ├── operations-events.md          [现有]
│   ├── isolation.md                  [现有]
│   ├── overlayfs.md                  [新建] OverlayCore：写时复制、首次触达原像、apply 状态机
│   ├── overlaynet.md                 [现有]
│   ├── gateway.md                    [现有]
│   ├── journal.md                    [新建] 日志、fsync、因果与污染（poisoning）
│   ├── replay.md                     [新建] 回放设计：工具前缀回放与实时续跑
│   ├── cli.md                        [现有]
│   ├── decisions/                    [新建] ADR 目录（index + 编号决策）
│   └── research/
│       ├── index.md                  [新建] 研究方向总览
│       ├── cluster-execution.md      [TODO] 集群化执行（L3）：与调度器的边界、缺口清单
│       ├── rl-execution-substrate.md [TODO] 作为 Agentic RL 执行基座
│       └── publications.md           [TODO] 论文、报告与演讲
└── community/
    ├── index.md                      [新建] 社区总览、沟通渠道
    ├── contributing.md               [新建] 贡献指南（与根 CONTRIBUTING.md 同源）
    ├── development.md                [改写] 开发环境与工程说明（现 development/engineering.md）
    ├── testing.md                    [新建] just test、nextest、semspec 与人工审核流程
    ├── examples.md                   [现有] 示例（现 development/examples.md）
    ├── releasing.md                  [现有]
    ├── roadmap.md                    [现有] 路线图（信任阶梯 + L1 工作）
    ├── changelog.md                  [新建] 引用根 CHANGELOG.md
    ├── governance.md                 [TODO] 治理与维护者
    ├── code-of-conduct.md            [新建] 行为准则
    └── adopters.md                   [TODO] 使用者与案例
```

## 5. 关键页面大纲

### 5.1 首页

1. 标题区：愿景"让自主 Agent 的执行可以规模化"、监督带宽论证一句、"今天能做什么"一句，主按钮"第一次运行"。
2. **证明区（新增）**：终端录屏，加三个首屏数字（启动延迟、文件系统开销、端到端任务开销），数字来自 `benchmarks/index`。没有数据前显示"基准建设中"，并链接到 TODO 页。
3. 有界、可逆、可查：三张卡片。
4. 价值随规模展开：四张卡片，标注当前级别。
5. 与现成方案的差别：三行摘要，链接到 `why/comparisons`。
6. 文档入口：按第 2 节的读者分路。

### 5.2 `why/index.md`

1. 问题：Agent 自主性被人的监督带宽封顶。
2. 论证：监督成本随执行量线性增长；脱钩的条件是有界、可逆、可查。
3. pVisor 的定位：每次执行的语义层，不是调度器，也不是另一个沙箱。
4. 今天与方向：信任阶梯摘要，链接到 `trust-ladder`。
5. 指标：每单位 Agent 工作所需的人工监督成本，链接到 `benchmarks/supervision-cost`。

### 5.3 `why/use-cases.md`

每个用例统一使用"场景 → 今天的做法与痛点 → 用 pVisor 之后 → 当前级别 → 相关指南"的结构：

- 个人：让 Claude Code 或 Codex 全自动完成重构，事后选择性合入。
- 个人：同时跑多个 Agent 方案并比较（L2，TODO）。
- 团队：在 CI 中让 Agent 修复失败的测试（TODO）。
- 平台：多租户 Agent 执行与集中审计（L3，方向）。
- 研究：Agentic RL rollout、评测和轨迹采集，可分叉、可回放（方向）。

### 5.4 `concepts/staging.md`

把分散在 CLI 参考和 review-apply 里的语义集中起来。每条语义标注对应的 semspec 用例编号（S-STAGE-xxx）：

- apply 前工作区不变；
- 外部修改、新建、删除冲突时拒绝 apply，并保留外部内容；
- `--all` 遇到冲突时整体拒绝；
- drop 之后不能再 apply；重复 apply 报告"已应用"；
- 重命名显示为删除加新增；
- 不可逆的部分：外部 API、数据库、已发出的消息。

### 5.5 `security/threat-model.md`

1. 资产：工作区、HOME、凭据、宿主机其他路径、网络、模型额度。
2. 攻击者：误操作的 Agent、被提示注入的 Agent、恶意依赖或工具、恶意模型响应。
3. 信任边界：按执行器（host、container、VM）分列。
4. 每个执行器保护什么、不保护什么（表格，链接到 `executor-boundaries`）。
5. 不在范围内：内核漏洞、侧信道、已授予凭据的滥用等。

### 5.6 `reference/platforms.md`

平台 × 执行器 × 能力维度的矩阵，每格写成熟度等级（稳定、Beta、实验、不支持）和证据链接。这一页兼作 README 的成熟度标识来源。

## 6. 基准需求清单（`benchmarks/` 下的 TODO 页）

所有基准共同要求：

- 写明硬件、操作系统、内核或 macOS 版本、FUSE 实现、pVisor 版本和提交号；
- 给出 p50、p95、p99 和样本数；
- 有可以一条命令复现的脚本，放在 `benchmark/` 下，报告使用 `pvisor-benchmark/v1` schema；
- 每个对照组写明配置，不和"未调优的对手"比较；
- 结果按日期保留，不覆盖旧数据。

| 编号 | 页面 | 指标 | 对照组 | 工作负载 | 验收 |
| --- | --- | --- | --- | --- | --- |
| B1 | `startup.md` | 冷启动、热启动延迟；常驻内存 | 裸进程；`docker run`；Firecracker；各 Agent 自带沙箱 | `pvisor run -- true`，覆盖 host、host+stage、`--safe`、container、VM 五种配置 | Linux 和 macOS 各一组；已有 guest init 数据迁入 |
| B2 | `filesystem.md` | 元数据操作延迟、读写吞吐、典型任务耗时比 | 原生文件系统；Docker bind mount；overlay2 | `git status` 大仓库、`npm install`、`cargo build`、ripgrep 全仓搜索 | 分 Linux FUSE、macFUSE、FSKit 三组；给出相对开销百分比 |
| B3 | `network.md` | 请求延迟、吞吐、连接建立耗时 | 直连；Docker 网络 | 小请求大量并发；大文件下载；LLM 流式响应 | 覆盖 host 代理、deny-all、VM smoltcp |
| B4 | `apply.md` | apply、drop 耗时与改动规模的关系；冲突检测成本；崩溃一致性 | `cp -a`；`git apply` | 10、1k、100k 个文件的改动集；在 Prepared、TargetApplied、Committed 各状态 kill -9 | 崩溃注入后工作区一致率必须是 100%，并说明恢复路径 |
| B5 | `agent-tasks.md` | 墙钟时间、token 用量、任务成功率 | 同一 Agent 不加 pVisor | SWE-bench Lite 子集（或自建任务集），Claude Code、Codex 各一组 | 成功率差异在统计误差内；开销百分比 |
| B6 | `supervision-cost.md` | 每个任务的人工介入次数和监督时间 | 逐条批准模式；全自动加事后 `git diff` | 固定任务集的用户研究，至少 10 名参与者 | 这是愿景指标的第一组实测数据；协议要预先注册 |
| B7 | `density.md` | 单机并发 Job 数、每个 Job 的 CPU 与内存开销、尾延迟 | Docker 同等密度 | 1、8、32、128 个并发 Job | 给出不同执行器下的单机上限，作为 L2/L3 规划依据 |
| B8 | `isolation-tests.md` | 逃逸用例通过率 | 各执行器之间 | semspec S-STAGE 用例，加公开的沙箱逃逸语料（符号链接替换、路径穿越、Unix socket、`/proc` 等） | 每个用例按执行器给出 PASS/FAIL/XFAIL，XFAIL 链接到已知限制 |
| B9 | `replay-fidelity.md` | 回放一致率、续跑成功率 | — | 已有 `benchmark/replay/qwen3.6-results.md` | 迁入并补充方法说明 |

## 7. 对比需求清单（`benchmarks/compare-*.md`）

所有对比页面的共同规则：

- 页首写明比较日期和各产品的版本；
- 每项结论附出处：官方文档链接，或可以复现的测试；
- 有"更正"入口：链接到 issue 模板；
- 同时写对方擅长的地方，以及什么情况下应该选对方；
- 能用基准数据的地方，引用第 6 节的结果。

| 编号 | 页面 | 对比对象 | 维度 |
| --- | --- | --- | --- |
| C1 | `compare-agent-sandboxes.md` | Claude Code 沙箱、Codex 沙箱与审批模式、Gemini CLI 沙箱 | 拦截方式、事后选择性合入、冲突保护、证据记录、跨 Agent 一致性、网络控制、性能开销 |
| C2 | `compare-containers.md` | Docker、devcontainer，以及 Docker 加 `git diff` | 隔离强度、改动审查、冲突保护、选择性合入、证据、启动与文件系统开销、本地工具链可用性 |
| C3 | `compare-cloud-sandboxes.md` | E2B、Daytona、Modal | 执行位置、本地工作区集成、审查链路、数据出境、成本模型、规模化能力 |
| C4 | `compare-runtimes.md` | gVisor、Firecracker、Kata | 定位说明：它们是隔离基座，pVisor 是执行语义层，可以作为执行器后端；列出接入现状与计划 |
| C5 | `compare-rl-infra.md` | OpenHands runtime、SWE-Gym 类环境、RL 框架的沙箱组件 | 隔离、轨迹记录、分叉与回放、并发密度、与训练框架的集成方式 |

`why/comparisons.md` 只放一张总表（每个对象一行，三列：能做到、缺什么、何时选它），细节链接到上面各页。

## 8. TODO 页面约定

每个 TODO 页面使用统一的模板，作为给研发的需求单：

```markdown
---
status: todo
search:
  exclude: true
---

# 文件系统开销

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题
用户最关心的一句话问题，例如"在 pVisor 里跑 cargo build 会慢多少"。

## 需求
- 指标：
- 对照组：
- 工作负载：
- 环境：

## 验收标准
- ……

## 关联
- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：……
```

配套机制：

1. `scripts/check-docs.py` 统计 `status: todo` 的页面，构建时输出一张 TODO 清单。
2. 首页和栏目首页不直接链接 TODO 页，只在对应位置写"建设中"并链接。
3. 每个 TODO 页对应一个 GitHub issue，打上 `docs-todo` 标签。页面写完后去掉 `status: todo`，同时关闭 issue。
4. 导航中的 TODO 页标题后加"（规划中）"后缀。

## 9. 现有页面迁移

| 现有路径 | 新路径 | 处理 |
| --- | --- | --- |
| `concepts/run-model.md` | `concepts/jobs.md`、`design/execution-model.md` | 拆分：用户部分留在概念，Operation、Attempt、Session 移入设计 |
| `guides/execution.md` | `guides/executors/index.md` | 改写为选择指南，细节拆到三个执行器页面 |
| `guides/network.md` | `guides/policies/network.md` | 执行器边界矩阵移到 `security/executor-boundaries.md`，本页链接过去 |
| `guides/sandbox-replay.md` | `guides/replay.md` | 改名 |
| `guides/workflows.md` | 拆入 `why/use-cases.md` 和各指南 | 删除 |
| `development/*` | `community/*` | 整体迁移 |
| `docs/shared-image-cache.md` | `reference/shared-image-cache.md` | 修复：导航里引用了，但站点源码里没有这个文件 |
| `benchmark/pvisor/README.md` 中的数据 | `benchmarks/startup.md` | 数据迁入站点，脚本和原始报告仍放在 `benchmark/` |
| `benchmark/replay/qwen3.6-results.md` | `benchmarks/replay-fidelity.md` | 同上 |

所有迁移都在 `docs/redirects.json` 中登记旧路径到新路径的映射。

## 10. 双语策略

中文仍是权威版本。为了国际影响力，英文必须覆盖主路径，分三期：

| 优先级 | 英文必须覆盖的页面 |
| --- | --- |
| P0 | 首页、`why/index`、`why/comparisons`、`start/*`、`concepts/capabilities-and-evidence`、`security/index`、`security/disclosure`、`reference/cli` |
| P1 | `guides/agents/*`、`guides/review-apply`、`concepts/staging`、`benchmarks/index`、`design/architecture`、`community/contributing` |
| P2 | 其余页面 |

英文页面缺失时，链接标注"（中文）"，不静默跳转。

## 11. 分期落地

| 阶段 | 内容 | 产出 |
| --- | --- | --- |
| 第一期：骨架 | 新目录与导航；全部 `[TODO]` 占位页；页面迁移与重定向；修复 `shared-image-cache` 导航；`why/` 四页；`security/index`、`disclosure`，加根目录 `SECURITY.md`、`CONTRIBUTING.md` | 结构完整，需求可见 |
| 第二期：可信 | `concepts/staging`、`security/threat-model`、`reference/platforms`、`run-bundle`、`config`；B1、B2、B4、B8 四项基准；C1、C2 两项对比；英文 P0 | 首屏有数字，有威胁模型 |
| 第三期：影响力 | B5、B6、B7；C3、C4、C5；`guides/ci`、`rl-rollouts`；`design/research/*`；`adopters`；英文 P1 | 研究和平台读者有完整入口 |

## 12. 待决问题

1. `why/` 和 `start/what-is-pvisor.md` 是否合并成一页？本提案倾向保留两页：前者讲愿景和论证，后者只做一页产品说明。
2. 基准与对比是否拆成两个栏目？本提案合并，理由是对比需要引用基准数据。
3. B6（监督成本用户研究）的成本较高，是否在第三期之前先做小规模的内部试点？
4. TODO 页是否进入英文导航？本提案的建议是：只有 P0 栏目的 TODO 页进入英文导航。
