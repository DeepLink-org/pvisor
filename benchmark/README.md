# PolicyVisor benchmarks

**benchmark 的目的是给用户一个可以据此做决定的结论，再用实验支撑它。** 不回答用户问题的测量不写进用户文档；没有实验支撑的结论不写进任何文档。

本文件是所有 benchmark 工作的最高约束，优先于子目录 README、脚本注释和既有文档的写法。

## 给 Agent 的强制规则 {#agent-rules}

在 `benchmark/`、`docs/src/*/benchmarks/` 或 `docs/src/assets/benchmarks/` 下做任何事（新增、修改、复测、改写文档）之前：

1. **逐级阅读 README。** 先读本文件，再读从仓库根到目标文件路径上每一级目录的 `README.md`（例如 `benchmark/pvisor/README.md`）。不能只读与当前文件同级的 README，也不能因为"只是改一个数字"就跳过。
2. **先找到注册表条目。** 在[注册表](#registry)中找到对应的 benchmark ID，读完它的 motivation、想要的结论和实验设计，再读对应脚本开头的 `Benchmark:` 注释块。两者不一致时以本文件为准，并修正脚本注释。
3. **先写问题，再动手。** 开始测量或写作前，用一句话写出"这次要回答用户的哪个问题、期望得到什么形式的结论"。答不出来就停下来问用户，不要先测再找结论。
4. **新 benchmark 先登记。** 没有注册表条目的测量不得进入用户文档。新增 benchmark 时，先在注册表中加条目，再在入口脚本中加注释块，最后才写文档。
5. **区分三种角色。** 每个 benchmark 只有一种角色（见[角色](#roles)）。工程 A/B 和诊断实验的结果不进入用户文档正文。
6. **实验不能支撑想要的结论时，如实写出来。** 把结论改成实验真正支撑的范围，或者标为未测；不要扩大措辞，也不要用别的批次补齐。
7. **不在文档中讲过程。** 不写"先做了 v1，又改成 v2"、"我们发现"、"本轮"之类的叙述，不使用内部代号。过程和历史留在证据目录与 `target/` 下。

完成后自查[检查清单](#checklist)，每一项都满足才算完成。

## 每篇 benchmark 的结构 {#structure}

用户文档中的每篇 benchmark 固定为以下四节，顺序不变：

```markdown
# 标题：用户关心的问题，不是被测组件名

## 主要结论 {#conclusions}
一段加粗的结论句回答标题中的问题，再用一张小表给出选型含义。

## Motivation {#motivation}
用户为什么需要知道这件事：哪种决策依赖它，不知道会付出什么代价。

## 实验设计 {#interpretation}
为了回答上面的问题，测什么、对照什么、怎样控制变量、什么算有效样本。

## 实验数据和分析 {#results}
支撑结论的数据表或图，以及对数据的解读和适用范围。
```

各节的要求：

- **主要结论**
  - 第一句直接回答标题的问题，读者只看这一句也能做决定。
  - 结论中的每个数字都必须出现在本页"实验数据和分析"的表或图中。
  - 写清楚适用条件（平台、配置、负载类型），但不在结论里堆批次、日期、制品哈希。
  - 有短板就写短板。"pVisor 在 X 上更慢"也是结论。
- **Motivation**
  - 写用户的决策，不写我们的工程动机。"选 staged 还是 VM 前需要知道工具任务要等多久"可以；"验证路径索引优化"不可以。
  - 两到四句话。
- **实验设计**
  - 说明负载为什么能代表用户场景，对照组为什么是这几个。
  - 列出控制变量：硬件预算、预热和采样数、缓存状态、隔离方式、正确性校验。
  - 说明什么样本被拒绝（失败、校验不通过、受干扰），失败不能当作零耗时。
  - 说明这个设计**不能**回答什么。
- **实验数据和分析**
  - 先放回答结论的那张表，再放补充数据。
  - 每张表写明单位、统计量（P50/P95）、样本数。
  - 分析要解释数据对用户意味着什么，不复述表格。
  - 末尾链接原始样本、制品清单与复现命令（复现命令放在 `benchmark/` 的 README，文档只链接）。

篇幅：正文控制在一屏到三屏。超过时，把技术分析移到 `docs/src/*/design/*-performance-analysis.md`，用户页只保留结论和一句链接。

## 角色 {#roles}

| 角色 | 回答谁的问题 | 结果放在哪里 | 能否进入用户文档正文 |
|---|---|---|---|
| **user-facing** | 用户：该不该用、选哪个模式、要预留多少资源 | `docs/src/*/benchmarks/` | 能，且必须遵守[结构](#structure) |
| **engineering A/B** | 开发者：这个改动有没有让产品变好或变差 | `docs/src/*/design/*-performance-analysis.md`、PR 描述 | 不能；只有改变了用户结论时，才更新 user-facing 页的数字 |
| **diagnostic** | 开发者：时间花在哪里 | `target/`、设计文档的分析节 | 不能 |

一个脚本可以同时被多个 benchmark 使用，但每次运行只服务一个 benchmark ID，报告中要记录该 ID。

## 数据与统计规则 {#data-rules}

- **不合并批次。** 不同日期、不同二进制、不同配置的样本分别统计。跨批次放在同一张表时，必须在表头或表注中写明。
- **同批对照才能说"更快"。** 只有同机、同预算、同负载、随机交替执行的对照，才能写百分比变化。跨批次只能并列展示各自的水平。
- **给出不确定性。** A/B 结论附上中位数差异的 95% 置信区间（bootstrap 即可）。区间包含 0 时写"未检出差异"，不写"略快"或"略慢"。
- **先看分布形状。** 分布是双峰时，报告每簇的比例和中位数，不报告单个 P50。
- **处理干扰。** 事先写好剔除规则（例如同一任务中所有负载同时超过中位数 1.5 倍即判为宿主干扰），并报告剔除了几个。不能事后挑选样本。
- **尾延迟要有足够样本。** 30 个样本不报告 P99，P95 也只作参考。需要尾延迟结论时，增加样本量。
- **正确性先于计时。** 输出校验、隔离校验、宿主未被修改的检查都通过的样本才计入。快但不正确的结果判为失败。
- **记录来源。** 二进制和源码的摘要、harness 版本、宿主内核、负载、CPU 亲和性写进报告；文档只链接到这些信息。

## 注册表 {#registry}

以下条目定义每个 benchmark **要回答的问题和期望得到的结论形式**。它们是需求，不是现有数据的摘要：如果现有实验不能支撑某条结论，应补实验或在文档中写明未测，而不是改写需求去迁就数据。

每个条目的入口脚本开头都有同名的 `Benchmark:` 注释块，格式如下：

```text
Benchmark: B-XXX (benchmark/README.md#b-xxx), role user-facing.
Motivation: 用户的哪个决策依赖这个测量。
Conclusion sought: 期望得到的结论形式。
Design: 负载、对照、控制变量与有效样本判据。
```

只准备环境、渲染报告、绘图或作为 worker 被调用的辅助脚本（如 `prepare_*`、`render_*`、`plot_*`、`*_worker.py`、`evidence_tsv.py`）不需要注释块，它们服务于调用它们的入口脚本。

### B-STARTUP：启动一个可用环境要等多久 {#b-startup}

- **文档：** `startup.md`
- **角色：** user-facing
- **Motivation：** Agent 频繁创建一次性环境时，启动等待直接决定交互延迟和短任务的总成本。用户需要知道 pVisor 各模式的启动处于什么量级，以及与 Docker 和轻量 VM 相比如何。
- **想要的结论：** "已准备好环境时，pVisor host/staged 的首条命令可在 X ms 内执行，VM 在 Y ms 内执行，与 Docker、Firecracker 处于同一量级或差多少"；以及"完整发行版的启动成本为 Z 秒级，pVisor 无镜像 VM 能省掉多少"。
- **实验设计：**
  - 指标分为首条有效输出（ready）和启动到进程退出（completion），两者分开报告。
  - 对照组为原生进程、Docker、Firecracker、QEMU microvm，以及 pVisor 的 host、staged、VM。
  - 环境和镜像预先准备好，准备时间单独记录；热缓存与冷镜像分成两组。
  - 统一 CPU 和内存预算，随机交替执行，每格至少 30 个样本。
  - 完整 Ubuntu 只作为"完整 OS 启动成本"的对照，不与最小 VM 做 VMM 排名。
- **入口脚本：** `startup.py`、`linux_vm_ready.py`、`vm_ready.py`、`run_all.py`、`ubuntu_baselines.py`；诊断用 `firmware_boot.py`、`guest_init.py`。

### B-FS-TOOLS：开发工具在各执行模式下要多花多少时间 {#b-fs-tools}

- **文档：** `filesystem.md`
- **角色：** user-facing
- **Motivation：** Agent 的大部分时间花在遍历、读写、git、搜索、编译和装依赖上。用户需要知道选择 staged 或 VM 后，这些工具会慢多少，以便选择模式和设置超时。
- **想要的结论：** "相对原生和 Docker，pVisor staged 在哪类操作上接近、在哪类操作上慢几倍；VM 相对轻量 VM 慢或快多少；一次七项工具任务的总等待是多少"，以及每种模式最适合的任务类型。
- **实验设计：**
  - 七项负载：2,048 个文件的遍历、64 MiB 读取并校验、256 个文件写入、git status、rg、小型 Cargo 编译、离线 npm 安装。每项代表一类常见的 Agent 工具操作。
  - 对照组为原生、Docker bind mount、Firecracker、QEMU（q35 和 microvm），以及 pVisor host staged 和 VM。
  - 所有组使用相同的工具环境、相同的两核预算和相同的 VM 内存；每次执行使用新的工作区。
  - 分别报告每项工具耗时和完整任务耗时（包括启动与收尾）。
  - 每个样本都要校验工具输出；暂存模式还要校验宿主原文件未被写入、upper 内容完整。
  - 不能回答的问题：大型仓库、冷磁盘、真实 registry 和并发吞吐。
- **入口脚本：** `reference_baselines.py`（跨运行时对照）。

### B-FS-ENG：文件系统改动的工程 A/B 与成本分解 {#b-fs-eng}

- **文档：** `docs/src/*/design/filesystem-performance-analysis.md`
- **角色：** engineering A/B 与 diagnostic
- **Motivation：** 开发者需要判断一次文件系统改动是否让 B-FS-TOOLS 的用户结论变好，并定位时间花在哪一层（传输、OverlayCore、持久化、内容指纹）。
- **想要的结论：** "改动 X 让负载 Y 的中位数变化 Z%（95% 置信区间），其余负载未检出差异"；以及"staged 与 FUSE 直通之间的差距中，持久化占 A ms，内容指纹占 B ms，路径解析占 C ms"。
- **实验设计：**
  - 版本 A/B 使用同一份冻结源码，只应用待测改动后构建两个二进制，同机随机交替运行，每格 30 个样本，并报告置信区间。
  - 分解实验在同一批次内对照原生、FUSE 直通和 staged，同时打开 profile 计数器另跑一批；带计数器的批次不计入计时结论。
  - lazy 镜像、stage 持久化策略、内核缓存探针各自独立成批。
  - 只有当结果改变了 B-FS-TOOLS 的用户结论时，才更新 `filesystem.md`。
- **入口脚本：** `filesystem_ab.py`、`filesystem_fuse_ab.py`、`filesystem_stage_ab.py`、`filesystem_stage_durability.py`、`filesystem_lazy_ab.py`、`filesystem_kernel_probe.py`、`filesystem_diagnostic.py`。

### B-AGENT-TASK：一次完整的 Agent 修复任务要多久，主流 CLI 能否正常运行 {#b-agent-task}

- **文档：** `agent-tasks.md`
- **角色：** user-facing
- **Motivation：** 启动时间和单项工具时间都不能代表"改代码、跑测试"的总等待。用户还需要知道自己使用的 Agent CLI 能否在 pVisor 中正常工作。
- **想要的结论：** "固定修复任务在 staged 和 VM 中的端到端耗时分别是多少，与原生和 Docker 相比差多少"；以及"Claude Code、Codex 等 CLI 在各模式下能否完成受控工具闭环"，不能完成的要写明失败点。
- **实验设计：**
  - 使用固定的修复任务和测试，模型响应在本地固定回放，排除推理时间和公网波动。
  - 对照组与 B-FS-TOOLS 相同。
  - 以任务通过测试作为成功判据，失败单独计数，不计入耗时分布。
- **入口脚本：** `reference_baselines.py`（tools 模式）、`v1/agent.py`。

### B-APPLY：审查后合入改动要多久，并行修改是否安全 {#b-apply}

- **文档：** `apply.md`
- **角色：** user-facing
- **Motivation：** 暂存改动的价值要到合入时才兑现。用户需要知道合入的耗时如何随文件数增长，以及宿主上的并行修改会不会被覆盖。
- **想要的结论：** "合入 N 个文件需要多少时间，在多大规模以内适合交互式使用"；"宿主并行修改一定会被检测为冲突，不会被静默覆盖"；"合入中途被中断后可以恢复，或者状态明确"。
- **实验设计：**
  - 文件数按 10 到 10 万的数量级扫描，并与同批的 Git patch 合入做对照。
  - 冲突注入：在合入前或合入过程中修改宿主文件，检查是否全部被检测到。
  - 中断注入：在合入的各阶段 SIGKILL，检查重新执行后的最终状态。
- **入口脚本：** `v1/apply.py`。

### B-NETWORK：网络代理和 VM 网络有多大开销 {#b-network}

- **文档：** `network.md`
- **角色：** user-facing
- **Motivation：** Agent 会频繁发小请求、下载依赖、拉取模型流式响应。用户需要知道开启网络策略和使用 VM 后，延迟和吞吐会损失多少。
- **想要的结论：** "小请求经过 pVisor 代理增加多少毫秒；VM 中的批量传输吞吐是原生的几分之一"，并说明这些开销与模型响应时间相比是否重要。
- **实验设计：**
  - 使用本地 HTTP 服务，分别测小请求延迟和 32 MiB 量级的传输吞吐。
  - 对照组为原生、host 代理和 VM。
  - 不访问公网，不代表真实模型 API 的延迟。
- **入口脚本：** `v1/network.py`、`ubuntu_vm_network.py`。

### B-DENSITY：一台机器能同时跑多少个环境 {#b-density}

- **文档：** `density.md`
- **角色：** user-facing
- **Motivation：** 多 Agent 并行时，内存和启动成本会累积。用户需要知道给定的机器能稳定支撑多少个 staged 或 VM 环境。
- **想要的结论：** "在 X GiB 内存下，staged 可稳定并发 N 个、VM 可稳定并发 M 个，每个环境平均占用多少内存"；超过多少开始失败，失败的形式是什么。
- **实验设计：**
  - 按 1 到 128 的并发度扫描，同时记录成功率、启动等待、RSS 与 cgroup 内存。
  - 对照组为 Podman 或 Docker。
  - 先测空闲探针，再测带真实工具负载的情况；两者分别报告。
  - 成功率与资源一起报告，不能只报告成功的样本。
- **入口脚本：** `v1/density.py`。

### B-ISOLATION：隔离是否真的生效 {#b-isolation}

- **文档：** `isolation-tests.md`
- **角色：** user-facing（正确性，不是性能）
- **Motivation：** 性能数字只有在隔离真正生效时才有意义。用户需要知道每种模式实际阻止了什么、放行了什么。
- **想要的结论：** "每种模式下，视图外的读、写、网络访问是否被阻止"，用一张模式乘以行为的表回答，并对比 OCI 可写挂载等常见做法。
- **实验设计：**
  - 每种行为都有负对照（必须被阻止）和正对照（必须被允许）。
  - 以宿主上的最终状态作为判据，而不是只看请求返回了什么。
- **入口脚本：** `v1/isolation.py`。

### B-SUPERVISION：审查改动的机器成本有多大 {#b-supervision}

- **文档：** `supervision-cost.md`
- **角色：** user-facing
- **Motivation：** 用户在 Agent 运行之外，还要审查 diff、选择性合入或丢弃。用户需要知道这一流程中工具本身占用的时间是否可以忽略。
- **想要的结论：** "审查 N 个文件、合入其中一部分的机器流程耗时为多少，与 Git diff 流程相当或差多少"；人的阅读时间不在测量范围内，需要明确写出。
- **实验设计：** 固定数量的改动文件，测 status、review、选择性 apply 和 drop 的完整流程，并与 Git 工作流做同批对照。
- **入口脚本：** `v1/supervision.py`。

### B-REPLAY：轨迹回放能否忠实准备环境 {#b-replay}

- **文档：** `replay-fidelity.md`
- **角色：** user-facing（正确性与成本）
- **Motivation：** 训练和复现需要把历史轨迹准确还原到同一个起点。用户需要知道支持哪些轨迹格式、准备是否有副作用、成本是多少。
- **想要的结论：** "支持的格式全部通过一致性校验，准备阶段不执行工具也不修改工作区，每条轨迹的准备耗时为 X ms"。
- **实验设计：** 对每个适配器使用固定的原生格式样本，校验前缀结构、工具参数和观测内容，同时校验工作区没有被修改。
- **入口脚本：** `v1/replay.py`。

### B-VM-MEMORY：VM 闲置时能省多少内存，恢复要付出什么 {#b-vm-memory}

- **文档：** `vm-memory/index.md`
- **角色：** user-facing
- **Motivation：** 长期挂起的 Agent 环境占用内存。用户需要知道冷页回收、offload 和快照能省多少内存，以及恢复访问时要等多久。
- **想要的结论：** "闲置 VM 的驻留内存可以降低 X%，以整机物理内存衡量；恢复后首次访问会增加 Y ms"；以及与 Docker 或其他 VM 相比是否真的更省。
- **实验设计：**
  - 用整机或 cgroup 的物理内存作为主要指标，进程 footprint 只作参考。
  - 同时测回收量、恢复延迟和 CPU 开销。
  - 区分重复数据和随机数据负载。
  - 每次试验使用新的 VM，并校验数据完整性。
- **入口脚本：** `macos_cold_ram.py`；`vm_snapshot*.py`、`vm_stress.py` 为已退役功能的历史脚本，不再产生新结论。

### B-CLUSTER：增加机器和资源后，能否得到更多有效结果 {#b-cluster}

- **文档：** `cluster-scalability.md`
- **角色：** user-facing
- **Motivation：** 规模化运行 Agent 时，用户关心的是增加 Worker 和资源能不能线性地增加有效产出，以及控制面在长期运行后是否会成为瓶颈。
- **想要的结论：** "在固定的总资源预算下，每秒完成的有效任务数随 Worker 数增长的曲线"；"控制面在保留 N 条历史记录时的内存和重启耗时"。
- **实验设计：**
  - 固定每个 Worker 的资源上限，扫描 Worker 数量。
  - 以完成且通过校验的任务数作为产出指标。
  - 控制面的历史规模单独扫描。
  - 不能只报告就绪时间来代替吞吐。
- **入口脚本：** `cluster_scalability.py`、`plot_cluster_scalability.py`。

### B-MACOS：macOS 上的工具和迁移成本 {#b-macos}

- **文档：** 并入 `filesystem.md` 和 `startup.md` 的 macOS 小节
- **角色：** user-facing
- **Motivation：** macOS 用户的对照对象通常是 Docker Desktop。他们需要知道在 Apple Silicon 上，pVisor VM 的工具执行和启动相对 Docker 如何。
- **想要的结论：** "在 Apple Silicon 上，pVisor VM 与 Docker Desktop 在相同工具负载下的耗时对比"。
- **实验设计：** 同机随机交替执行，使用相同的 Alpine 环境和工具负载，分别计时 worker 耗时和完整任务耗时。
- **入口脚本：** `macos_docker_tools.py`、`macos_migration.py`。

### B-COMPARE：与其他工具的对比页 {#b-compare}

- **文档：** `compare-*.md`
- **角色：** user-facing（综合页）
- **Motivation：** 用户通常带着"我已经用 X，要不要换"的问题来。对比页把各主题的结论按对方工具重新组织。
- **想要的结论：** "相对 X，pVisor 在哪些场景下更合适、在哪些场景下 X 更合适"，每条都要链接到某个 user-facing benchmark 的数据。
- **实验设计：** 对比页本身不做新测量。没有同条件数据的工具，只比较能力差异，不给数值排名，也不引用厂商宣传的数字。

### B-PROCESS：Run 本身的进程级开销（CI 回归门禁） {#b-process}

- **文档：** 无用户文档；结果用于 CI 和 PR
- **角色：** engineering A/B
- **Motivation：** 防止 Run 的启动和 Bundle 读取在日常开发中出现回归。
- **想要的结论：** "候选版本相对主干，最小 Run 的耗时和 status 读取耗时没有超过阈值的回归"。
- **实验设计：** 同机同套件，smoke 使用 2 次预热、10 个样本，nightly 使用 10 次预热、50 个样本；默认 15% 阈值。
- **入口脚本：** `bench.py`（`just benchmark`）。

## 检查清单 {#checklist}

提交 benchmark 相关改动前逐项确认：

- [ ] 已阅读本文件和路径上每一级目录的 README。
- [ ] 改动对应的注册表条目存在，脚本注释块与条目一致。
- [ ] 用户文档页严格按"主要结论 → Motivation → 实验设计 → 实验数据和分析"四节组织。
- [ ] 主要结论的第一句直接回答标题的问题，所有数字都能在本页的表或图中找到。
- [ ] 没有过程叙述、版本迭代史和内部代号；工程 A/B 和诊断结果没有进入用户页正文。
- [ ] 百分比变化来自同批对照，并附有置信区间；双峰分布与受干扰样本已按[规则](#data-rules)处理。
- [ ] 所有计入样本都通过了正确性和隔离校验，失败已单独计数。
- [ ] 中英文两版同步更新，原始样本与制品清单已加入版本控制。

## 目录

- [`pvisor/`](pvisor/README.md)：测量脚本、运行方式与报告格式。
- [`replay/`](replay/qwen3.6-results.md)：Qwen3.6 SandboxReplay 实验记录，保留历史样本，不作为产品保证。
- 用户文档：[`docs/src/zh/benchmarks/`](../docs/src/zh/benchmarks/index.md)。
- 技术分析：`docs/src/zh/design/*-performance-analysis.md`。
