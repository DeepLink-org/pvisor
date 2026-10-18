# PolicyVisor benchmarks

新的 hyperfine JSON 模板与 semspec 入口和逐批次 CSV / Markdown 更新方式见
[`benchmark.new/README.md`](../benchmark.new/README.md)。本文件继续维护 benchmark ID 的需求与角色；
下述旧脚本及历史报告保留原始口径，不能作为 hyperfine 测量结果发布。
`just benchmark` 的 B-PROCESS 已使用新入口；其余 ID 的新入口映射见
[`benchmark.new/registry.csv`](../benchmark.new/registry.csv)，仍需显式提供准备好的单次实验条件命令。

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
7. **不在文档中讲过程。** 不写"先做了 v1，又改成 v2"、"我们发现"、"本轮"之类的叙述，不使用内部代号。过程和历史留在证据目录与 相关目录的 `.data/` 下。

完成后自查[检查清单](#checklist)，每一项都满足才算完成。

## 每篇 benchmark 的结构 {#structure}

用户文档中的每篇 benchmark 固定为以下四节，顺序不变：

```markdown
# 标题：用户关心的问题，不是被测组件名

## 主要结论 {#conclusions}
一句话说明 pVisor 的收益，再用两到三条简短结论支撑。

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
  - 按“总判断 → 支撑判断 → 定量证据”分层。总判断说明 pVisor 的技术选择及用户价值；两到三条支撑判断各表达一个完整观点；数字紧随对应观点或放在数据节。不要把指标或对照项目平铺成结论列表。
  - 结论中的每个数字都必须出现在本页"实验数据和分析"的表或图中。
  - 写清楚适用条件（平台、配置、负载类型），但不在结论里堆批次、日期、制品哈希。
  - 有短板就写短板。"pVisor 在 X 上更慢"也是结论。
- **Motivation**
  - 写用户的决策，不写我们的工程动机。"选 staged 还是 VM 前需要知道工具任务要等多久"可以；"验证路径索引优化"不可以。
  - 两到四句话。
- **实验设计**
  - 说明负载为什么能代表用户场景，对照组为什么是这几个。
  - 使用读者能理解的任务和操作描述；benchmark 注册编号、内部 API、辅助程序与开关细节留在复现手册和证据中。
  - 列出控制变量：硬件预算、预热和采样数、缓存状态、隔离方式、正确性校验。
  - 说明什么样本被拒绝（失败、校验不通过、受干扰），失败不能当作零耗时。
  - 说明这个设计**不能**回答什么。
- **实验数据和分析**
  - 先放回答结论的那张表，再放补充数据。
  - 每张表写明单位、统计量（P50/P95）、样本数。
  - 分析要解释数据对用户意味着什么，不复述表格。
  - 末尾按需提供白名单内的必要统计 CSV 和来源摘要，复现命令链接到 `benchmark/` 的 README。正文已有的静态表不默认另附下载；完整数据、原始样本与制品清单留在本地 `.data/`，不生成失效的站点下载链接。

先写 pVisor 做了什么、带来什么收益，用具体动词和简短判断表达。对照项目的名称、版本和数据放在实验与数据节；适用条件集中说明。实测收益与预期分开，保留不利结果；多个条件同时变化时，只报告整体效果。

篇幅：正文控制在一屏到三屏。超过时，把技术分析移到 `docs/src/*/design/*-performance-analysis.md`，用户页只保留结论和一句链接。

## 角色 {#roles}

| 角色 | 回答谁的问题 | 结果放在哪里 | 能否进入用户文档正文 |
|---|---|---|---|
| **user-facing** | 用户：该不该用、选哪个模式、要预留多少资源 | `docs/src/*/benchmarks/` | 能，且必须遵守[结构](#structure) |
| **engineering A/B** | 开发者：这个改动有没有让产品变好或变差 | `docs/src/*/design/*-performance-analysis.md`、PR 描述 | 不能；只有改变了用户结论时，才更新 user-facing 页的数字 |
| **diagnostic** | 开发者：时间花在哪里 | 相关目录的 `.data/`、设计文档的分析节 | 不能 |

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

### 原始数据与加工数据的存储

- 原始报告、逐次样本、运行日志、profile、输入与二进制清单、冻结源码和 harness 保存到生成目录的 `.data/`。全仓库 `.data/` 被 Git 忽略；忽略规则不会自动移除已跟踪文件，迁移时保留原字节并核对摘要。
- Markdown 保存读者需要的加工表格与结论。正文已有的静态表、逐轮逐环境记录、历史表和工程中间表不默认导出到公开目录。必要的用户统计 CSV 由 [`benchmark.new/public_assets.py`](../benchmark.new/public_assets.py) 白名单控制，经 `csv_path(repo, filename, locale)` 定位到 `docs/overrides/assets/benchmarks/data/`，中英文共用一份；保留批次、配置、样本数和统计口径。动态 `.render.json` 只保留构建所需的展示表，本地化投影留在文章旁。完整 CSV/JSON 保存在原批次的本地 publication 目录或 `.data/` 归档。
- 不再把 typed TSV 完整报告、原始 samples CSV、压缩证据包或源码快照发布到 `docs/src/assets/benchmarks/`。保留本地数据与加工结果之间的摘要关联。
- 发布脚本的输入和原始归档放 `.data/`。公开数据限于白名单必要 CSV、SVG 图表和必要用户实验的精简来源摘要，不发布 PDF；工程实验只公开 SVG，完整统计和来源留在本地。公开摘要保留批次、口径、配置和制品/证据哈希；完整 manifest、逐文件 harness 清单与软件包收据留在本地。双语来源摘要共用 `docs/overrides/assets/benchmarks/`。历史与工程 CSV/JSON、旧 provenance 等归档于 `benchmark.new/.data/docs-assets-prune-20261018/`，迁移清单保留原路径、字节摘要与新位置；不得改写测量数据或把历史结果冒充新实验。站点构建和搜索也排除 `.data/`。
- 退役脚本与模块没有新测量入口；活动 benchmark 的注册 ID、入口和复现手册必须同步。

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

### B-OVERVIEW / B-METHOD：选型总览与比较方法 {#b-overview}

- **文档：** `index.md`、`methodology.md`
- **角色：** user-facing（综合页）
- **Motivation：** 用户需要选择主题并判断哪些数字可比较。
- **想要的结论：** 各主题的选型含义、证据范围与未测项目。
- **实验设计：** 复用已登记 user-facing 主题，不引入新测量，也不拼接不同口径为排名。
- **入口脚本：** 无独立入口；每个数据点链接到所属主题。

### B-STARTUP：启动一个可用环境要等多久 {#b-startup}

- **文档：** `startup.md`
- **角色：** user-facing
- **Motivation：** Agent 频繁创建一次性环境时，启动等待直接决定交互延迟和短任务的总成本。用户需要知道 pVisor 各模式的启动处于什么量级，以及与 Docker 和轻量 VM 相比如何。
- **想要的结论：** "已准备好环境时，pVisor host/staged 的首条命令可在 X ms 内执行，VM 在 Y ms 内执行，与 Docker、Firecracker 处于同一量级或差多少"；以及"完整发行版的启动成本为 Z 秒级，pVisor 无镜像 VM 能省掉多少"。
- **实验设计：**
  - 指标分为首条有效输出（ready）和启动到进程退出（completion），两者分开报告。
  - 对照组为原生进程、Docker、Firecracker、QEMU microvm，以及 pVisor 的 host、staged、VM。Firecracker 首选显式 `fc-system` 官方发行版 stock 内核；独立定制 `fc-reference` 为补充。旧 `firecracker` 只能标为 legacy reference/unknown，不能称为系统内核。
  - 内核来源问题：相同已准备 userspace 与预算下，内核来源如何影响首条正确输出的等待？两个 FC variant 共用参数、rootfs/磁盘、CPU/RAM、logger 与 teardown；冻结内核、配置、来源元数据、stock vmlinuz/extractor 和可选 initrd 的精确字节并在前后校验，不使用 pVisor firmware 准备 FC 内核。来源/配置/initrd 的差异需记录，不能解释为纯 VMM 成本。
  - QEMU stock 对照通过 `--qemu-system-receipt` 使用同一收据保留的原版 `/boot/vmlinuz` 和可选 initrd；FC 使用从该 vmlinuz 提取的 ELF。microvm 的模块驱动可使用 `prepare_stock_initrd.py` 准备仅加载原版模块并进入共同 rootfs 的 initrd，不能将最小 userspace 称为完整发行版启动。
  - 默认 normal 必须成功退出；可选 `--fc-ready-policy ready-only` 仅用于 ready，唯一且有序 Ready/Result/Exit0 与无 panic 后才受控 SIGTERM，再校验全部输出。ready-only 不报告 Completion，也不代表完整任务或正常关机；失败不能作为有效样本。新正式 cohort 不与历史用户数字合并。
  - 环境和镜像预先准备好，准备时间单独记录；热缓存与冷镜像分成两组。
  - 统一 CPU 和内存预算，随机交替执行，每格至少 30 个样本。
  - 完整 Ubuntu 只作为"完整 OS 启动成本"的对照，不与最小 VM 做 VMM 排名。
- **入口脚本：** `benchmark.new/startup/timing.json` 是八个后端的 Ready / Exit 对比表，每行调用固定的 Python timing 脚本（3 次预热、60 次样本，同一次 Popen 记录精确 Ready 行与正常退出）。启动命令由 `subjects.py` 固定；解释器启动和输入准备不计时。`just benchmark startup/timing` 输出整表并生成 `timing.render.json`；`--row` 可选择单行。单后端 `*_timing.json` 可独立执行。旧 hyperfine completion 模板已删除，历史完成时间数据保留原口径。显式保留的成功批次可通过 `update.py --startup-docs` 发布，历史报告归档于 `benchmark.new/startup/.data/history/`。Firecracker 使用 stock 内核/MMIO，预检失败不报告有效 Exit。复现见 [`benchmark.new/startup/README.md`](../benchmark.new/startup/README.md)。历史结果仍由 `reference_baselines.py`、`startup.py`、`linux_vm_ready.py`、`vm_ready.py`、`run_all.py`、`ubuntu_baselines.py`；工程实验与诊断见下列独立条目。

### B-LAZY-STARTUP：远程镜像按需读取能缩短多少启动等待 {#b-lazy-startup}

- **文档：** `lazy-image-startup.md`（中英文同步）。
- **角色：** user-facing。
- **Motivation：** 频繁创建短任务环境时，用户需要判断客户端按需读取是否比完整拉取、解包更划算，以及镜像服务端准备成本如何摊销。
- **想要的结论：** 同一固定 linux/amd64 digest、同一负载下（Ubuntu shell、Python/NumPy 脚本与可选 PyTorch CPU 导入分别成批，不合并），Docker 与 pVisor cache-service lazy VM 的客户端冷/热缓存 ready、completion 和传输量；明确服务端预准备成本，不解释为纯 VMM 差异。
- **实验设计：** 开源 Distribution registry 提供固定 OCI 镜像，pvisor-cache serve 提供同镜像文件索引和内容。两服务均为本机 loopback，无人工延迟时只能解释为本机模拟远程协议，不是 WAN。按独立网络条件分批；配置两核/2 GiB（容器硬限制与 VM guest RAM 不等价，Docker daemon/containerd 未作整机两核约束，必须披露），随机交替每组至少 30 次，冷客户端缓存后紧接热缓存，新 guest/容器与工作区。Ubuntu 负载校验发行版身份和 shell 输出；Python 负载禁用 bytecode 写入，固定单线程，校验 Python/库版本和确定性 NumPy 数组与矩阵运算（可选 PyTorch 还须校验 CPU-only build 和张量运算），ready 为全部校验后的唯一输出。两组均要求成功退出；失败单列，不剔除慢有效样本。预热/预检不纳入正式样本；非分离分布 P50、分离分布各簇比例和中位数、参考 P95、配对 bootstrap 95% CI。镜像服务端拉取/解包/索引时间单独保留，原始日志、源码和制品摘要放 `.data/`。不回答完整 OS 引导、真实 WAN、并发或大型任务性能。
- **当前实现复测：** 新静态 release 的 Docker/lazy 对照按负载独立成批，显式启用客户端索引页、上游连接池及持久私有桥；私有 namespace 隔离已有 Host listener。可使用校验后的原始压缩 OCI blobs 发布新 registry，并复制支持的 cache store，此时仅报告缓存准备成本，不沿用历史首次拉取耗时；Read Data、Metadata Data 和总响应分别计数。旧批次保留且不与新批次合并或计算实现加速比。
- **TurboOCI 对照协议：** [具体协议](../benchmark.new/startup/TURBO_OCI.md)回答“相同 OCI 来源与应用下，TurboOCI 和 pVisor 的按需路径分别缩短多少客户端等待，准备和传输成本是多少”。四组为相同 containerd/runc 的 eager OCI 与 TurboOCI，以及相同 pVisor VM 的 eager OCI 与文件索引 lazy；内部比较加载路径，跨运行时仅比较完整产品路径。原四项公开表缺少镜像身份和负载脚本，不称精确复现；先固定 shell 与 NumPy 同类负载。独立服务、namespace 和缓存，禁止调用上游全局清理脚本；Range 206 与 200 的实际字节、元数据与内容分别计数。先预检，再按轮随机交替冷客户端及紧随其后的热客户端，3 次预热、30 个正式样本，正确性与证据门禁同上。独立 OverlayBD/TCMU 后端已部署并通过 TurboOCI shell 功能验证，操作见[部署记录](../benchmark.new/startup/OVERLAYBD_LAB.md)；正式四组采样/审计尚未接通，不产生或发布新性能结果。
- **入口脚本：** `lazy_startup.py`。

- **已准备镜像的启动对照：** `benchmark.new/startup/image_compare.py` 是同源 Ubuntu shell 的独立补充批次，经单个 Python 配对行走 prepare → semspec bench → 专用 audit/render。普通 OCI/overlayfs 容器、TurboOCI/OverlayBD 容器与 pVisor 本地预准备 rootfs VM，每组 3 次预热、30 次随机交替，新执行实例和可写状态；镜像输入与宿主页缓存保留，不包含 pull、格式转换或解包。CPU 12,13，容器 2 CPU 配额/2 GiB，guest 2 vCPU/2 GiB；服务开销及 Docker exec 调用差异明确披露，不视为等整机预算或纯镜像格式比较。独立样本保存 Popen→唯一正确输出的 Ready 和同次正常退出的 Completion，校验输出、VM Bundle、实际后端与清理，配对 bootstrap 95% CI。记录其他 CPU 上的后台负载，不清理或停止无关任务；采样 CPU 上出现并发构建/其他已识别 benchmark 时整批标记受干扰，不事后剔除慢样本。不与 cold-client/warm-client 样本混合，不冒充完整四组冷镜像对照。

- **冷/热客户端镜像路径：** `benchmark.new/startup/image_lazy_compare.py` 单独比较 OCI/overlayfs 容器、TurboOCI/OverlayBD 容器和 pVisor lazy VM。固定上述同源 shell、CPU 和预算，每组 3 次预热、30 轮预定随机顺序，每轮冷客户端后紧接热客户端，均创建新执行实例。OCI/TurboOCI 冷条件在释放本实验任务/挂载/TCMU设备后，重启同一私有lab并将仅本lab的 containerd root、cache、run 目录归档后建立空目录；保留 registry 源内容和宿主页缓存，重启后恢复服务 CPU 14,15，故不能称冷磁盘或暖进程缓存。pVisor 使用新的空客户端缓存键及同一键的热复用，独立镜像服务预准备。客户端时钟包含 pull+run 或 pVisor CLI 的实际取数至唯一 marker/正常退出，环境重置与服务准备不计时。逐请求保留 registry 200/206 实际响应 body 和 pVisor framed 响应，content/metadata/total 分列；流量截止为完成后的明确观测点，不提供未经真实截点测量的 Ready 流量。冷内容必须非零、热内容必须为零；命令/输出/Bundle/制品/清理或流量校验失败使整批无效。两种缓存状态分别统计，不与 prepared 批次混合；缺少 pVisor eager OCI 冷准备条件，不称完整四组实验或纯格式贡献。

- **相同 stage 执行器的文件加载对照：** `benchmark.new/startup/stage_lazy_compare.py` 回答“去掉 VM/容器执行边界差异后，同一 pVisor host staged 任务从两种按需镜像文件系统读取同源文件，需要多久、传输多少”。两组分别为公开 cache API 持有的真实 host FUSE lower 与 TurboOCI/OverlayBD ext4 lower；同一宿主工具读取并校验固定 Ubuntu 镜像文件，在独立 stage 中只读校验，不能称镜像内程序启动。完整计时包含客户端打开/拉取和挂载、同一 staged CLI 到正确输出与正常退出；另报已挂载后的 stage 子阶段。固定 CPU 12,13、服务 14,15，保留宿主页缓存，不将其称冷磁盘；每轮新挂载与可写 stage、冷客户端后紧接热客户端，3 次预热与30轮随机配对。先功能预检；冷内容非零、热内容零、来源文件摘要、实际 FUSE/ext4 mount、host Bundle、空 changeset、正常退出与资源清理都必须成立，失败使整批无效。保存原始请求/字节、时钟、制品、后台负载、配对95% CI，分布分离时分簇报告；不合并原 VM/容器成绩，不发布为 OCI rootfs 执行支持。

### B-LAZY-ENG：小文件 lazy image V2 的工程对照 {#b-lazy-eng}

- **文档：** 工程结果保留 `benchmark/pvisor/LAZY_IMAGE_V2_REPORT.md`、`benchmark/pvisor/LAZY_INDEX_PAGES_REPORT.md`、`benchmark/pvisor/LAZY_BRIDGE_REPORT.md` 和 `.data/`；不混入用户启动页的历史 Docker 对照。
- **角色：** engineering A/B。
- **Motivation：** 判断有界目录元数据预取与持久连接是否减少 Python 导入的小文件请求成本。
- **想要的结论：** 同一冻结制品、同一 NumPy 镜像及脚本下，关闭/开启 V2 的冷/热 ready、completion、请求数、连接数和内容量变化，附配对 bootstrap 95% CI；不宣称 Docker/WAN 或吞吐排名。
- **实验设计：** 私有 user/mount/PID namespace 隔离 Host listener；新 VM/workspace/stage，CPU 0/1、2 vCPU、2 GiB guest RAM，预准备 cache 服务在 CPU 2/3，本机 loopback TCP，无延迟注入。每轮随机交替 V1-compatible（`PVISOR_LAZY_IMAGE_V2=0`）/V2（默认启用），每模式冷客户端后紧接热，至少 30 轮，另有初始轮与 3 warmups。两模式均包含正确分页等共同修复，开关只比较元数据预取与连接复用；当前 harness 在此对照中显式关闭客户端索引页（`PVISOR_LAZY_INDEX_PAGES=0`）。唯一正确 NumPy 输出、退出码、Run Bundle、冷内容非零、热内容为零全部通过才有效；任何失败使批次无效，不剔除慢有效样本。分布规则同 B-LAZY-STARTUP，P95 仅参考。冻结源码、制品、harness、请求和原始报告在 `.data/`，不与历史批次合并。
- **客户端索引页对照：** 独立批次比较 V2 RPC（`PVISOR_LAZY_IMAGE_V2=1, PVISOR_LAZY_INDEX_PAGES=0`）与 V2 客户端页（两项均为 1），隔离索引页带来的增量收益；另保留 V1/旧服务能力缺省回退测试。复用同一 NumPy、预算、正确性和 30 轮协议，文件内容载荷与二进制元数据载荷分别计数，不将索引页计为文件内容；原始证据及工程报告独立于已有 V1/V2 和 Docker 批次。
- **私有网络桥对照：** 独立批次比较桥 V1 单次连接与桥 V2 持久连接，V2 客户端与上游连接池均开启；分别在 RPC 与索引页模式测量，不跨模式合并。桥的连接读帧/idle 等待与四个执行 worker 分离，有界接入和队列。分别记录 runner→桥 Unix 连接/请求以及桥→服务 TCP 连接/请求，校验文件与元数据字节一致；同制品开关、预算、NumPy、30 轮、正确性和 bootstrap 规则同上，不混入历史批次。
- **入口脚本：** `lazy_image_v2.py`。

### B-FS-TOOLS：各执行模式在哪些文件系统任务上更合适 {#b-fs-tools}

- **文档：** `filesystem.md`
- **角色：** user-facing
- **Motivation：** Agent 的大部分时间花在遍历、读写、git、搜索、编译和装依赖上。用户需要知道选择 staged 或 VM 后，这些工具会慢多少，以便选择模式和设置超时。
- **想要的结论：** 分别给出仓库检查、真实增量编译、离线解包发布的等待时间，再用调用形状及工作集/并发对照说明各执行模式的适用条件；不再合成七工具或19项总分。
- **当前分层用户协议：** `benchmark.new/filesystem/user_workloads.py`，复用
  `WORKLOADS.md` 的12个机制、4个资源边界、3个完整工作流，但重新创建
  B-FS-TOOLS 用户配置对照批次，不复用 B-FS-ENG 的写回开关样本。
  快速覆盖 Native、普通 pVisor host、staged、CLI VM 默认、CLI VM 显式 guest
  writeback，以及 Docker、Firecracker stock kernel、QEMU q35 和 QEMU microvm。
  九种配置必须在同一新批次随机交替采样，不能拼接旧五模式的耗时。
  所有模式共用冻结 payload、fixture、Python/git/C 工具链；VM 为
  2 vCPU/512 MiB，整个串行 campaign 两核/2 GiB/零 swap。Native/host 的内存
  上限属于整个父组，不能声称与512 MiB guest等价；资源工作集统一按固定
  512 MiB参考尺度取128/640 MiB，每轮逻辑写入1 GiB，四文件不变。
  每个case/配置使用3个新环境，每环境3个相关轮次，case之间不互暖；同case
  按样本随机交替配置，无预热、无安静/干扰/全量清单门禁，保留全部慢值和失败。
  快速协议每环境最长90秒；某配置/case失败后，不再重复启动该单元剩余样本，
  明确记为未执行，其他单元继续。完整单元仍须N3，不用失败或缺失补零。
  Ready从观察者启动执行器至同一静态payload输出，排除退出；准备、工作、最终
  文件及目录同步、验证、退出分列。主要任务完成时间为work+final_sync，不含
  Ready/退出。guest逐轮验证，退出后host独立核对最终内容；失败配置/case单列。
  宿主页缓存不清理；后续轮次继承验证所造成的缓存，不等于冷磁盘。公开CSV保存
  逐环境三轮与中位数/范围，N3不提供可靠尾延迟和普遍排名。大规模单轴曲线、
  网络IOCP未测；Docker必须记录工作区存储方式与daemon预算，参考VM必须记录
  stock内核来源、virtio-blk与ext4磁盘路径，不能解释为纯VMM开销。
  不沿用历史七工具数字补格。
  工程开关归因和请求计数保留在B-FS-ENG/B-FS-DIAG分析页。
- **历史实验设计（原始样本保持原口径）：**
- **实验设计：**
  - 七项负载：2,048 个文件的遍历、64 MiB 读取并校验、256 个文件写入、git status、rg、小型 Cargo 编译、离线 npm 安装。每项代表一类常见的 Agent 工具操作。
  - 对照组为原生、Docker bind mount、Firecracker、QEMU（q35 和 microvm），以及 pVisor host staged 和 VM。
  - 所有组使用相同的工具环境、相同的两核预算和相同的 VM 内存；每次执行使用新的工作区。
  - 工具缓存每任务从空目录开始，默认保留执行器提供的 TMPDIR，在其中创建私有 HOME 与启用的 Node 编译缓存；七项工具中的 Cargo 使用工作区内独立的 `_cargo-home`、`_tmp` 和 `_cargo-target`，固定修复中的 Cargo 使用任务临时目录。记录实际缓存目录与文件系统类型。各执行器的默认临时存储差异是配置成本的一部分，不解释为纯 VMM 成本。工作区存储缓存是显式控制条件，与默认口径分开采样和统计。
  - 分别报告每项工具耗时和完整任务耗时（包括启动与收尾）。
  - 每个样本都要校验工具输出；暂存模式还要校验宿主原文件未被写入、upper 内容完整。
  - 不能回答的问题：大型仓库、冷磁盘、真实 registry 和并发吞吐。
- **历史及独立入口：** `reference_baselines.py`（历史跨运行时对照）。 新框架的 `benchmark.new/filesystem/tools.json` 使用两个 Python 行命令，测量原生 metadata/read 的完整 Python 命令（32 目录 / 2,048 文件 / 64 MiB、3 预热 / 30 样本）。`filesystem/prepare.py` 创建一份共享输入并预检，`fs_timing.py` 用 perf_counter_ns 采样，`audit.py` 复算逐次证据；`just benchmark filesystem/tools` 输出表格并生成 `tools.render.json`，支持 `--row`。解释器、管道和负载校验包含在完整命令时间内；输入准备与观察者启动不计时。它不能替代七工具内部阶段、跨后端任务或配对区间；新 Python 时钟数据独立于历史 `filesystem-semspec.csv`，旧结果归档于 `.data/history/`，复现见 [`filesystem/README.md`](../benchmark.new/filesystem/README.md)。

**历史 Ready 与驻留环境快速对照：** `filesystem_rounds.py` / `filesystem_rounds_workload.py`
回答“启动到首条负载 Ready 要多久，同一环境连续执行三轮工具后要等多久”。
每条件五个新环境，同轮随机交替；只将启动至唯一 `REFERENCE_READY` 计作启动时间，
任务内部计时与启动、退出分开，不以 completion 排名。七工具在同一进程树、VM、工作区
和任务缓存内执行三轮；第一轮 Cargo/npm 从空输出开始，后两轮保留构建与安装结果。
256 文件写入后两轮覆盖相同文件，读/遍历/git/rg 保留相同输入。
另预先固定两个适合缓存/写回的场景：同一文件 10,000 次 open/fstat/close，
以及同一 fd 连续 8,192 次 1 KiB 写入。后者分列 write 接受耗时与包含显式 fsync/close 的总耗时，
完整读回校验在任务计时外；所有条件执行相同调用与字节量。
对照沿用八个后端，另单列 `PVISOR_VM_FS_WRITEBACK=1` 的普通 CLI VM；
默认 VM 的写回关闭，两者都不声称普通 Mutable 工作区启用了 Immutable 目录镜像。
按用户快速测试要求不设安静期、外部干扰或全量清单扫描门禁，保留失败、所有有效慢样本、
正常退出、缓存继承、暂存隔离和完整字节校验。每一轮按五个环境独立统计，不能将三轮
当作十五个独立样本；中位数与范围独立于历史批次，不据跨批次差异推算优化收益。

### B-FS-ENG：文件系统改动的工程 A/B {#b-fs-eng}

- **文档：** `docs/src/*/design/filesystem-performance-analysis.md`、
  `docs/src/*/design/filesystem-io-performance-analysis.md`
- **角色：** engineering A/B
- **Motivation：** 开发者需要判断一次文件系统改动是否让 B-FS-TOOLS 的用户结论变好，并定位时间花在哪一层（传输、OverlayCore、持久化、内容指纹）。
- **想要的结论：** "改动 X 让负载 Y 的中位数变化 Z%（95% 置信区间），其余负载未检出差异"；以及"staged 与 FUSE 直通之间的差距中，持久化占 A ms，内容指纹占 B ms，路径解析占 C ms"。
- **实验设计：**
  - 版本 A/B 使用同一份冻结源码，只应用待测改动后构建两个二进制，同机随机交替运行，每格 30 个样本，并报告置信区间。
  - 分解实验在同一批次内对照原生、FUSE 直通和 staged，同时打开 profile 计数器另跑一批；带计数器的批次不计入计时结论。
  - lazy 镜像、stage 持久化策略、内核缓存探针各自独立成批。
  - 只有当结果改变了 B-FS-TOOLS 的用户结论时，才更新 `filesystem.md`。

- **guest 生命周期修复快速对照：** 固定同一个 GNU release/gateway host 制品，
  对比最终 0044 固件和叠加 0045 修复的固件；写合并关闭/开启分别成条件，
  五个随机配对环境，每环境沿用九操作三轮负载，启动只报告 Ready、内部操作排除退出。
  另以公开 VM API 的 `guest_metadata_case` / guest `bench` 模式，在显式 Immutable
  share 上比较三轮 256 文件 stat、目录扫描和 10,000 次热 open/fstat；SDK guest
  为 512 MiB、普通 CLI 为 16 GiB，分开统计。两组均 2 vCPU、相同两核宿主预算。
  按用户快速验证要求关闭外部干扰与清单门禁，保留全部有效慢样本、失败及字节校验。
  时间戳/xattr/hard-link 的原 fd 内容、并发扫描、writeback 正常及失败退出用独立
  正确性探针验证；profile 计数另跑，必须覆盖全部启动时注册的实例及预期 component。
  原始来源与失败保存在 `.data/fs-review-*`，只更新工程分析，不混入跨运行时用户表。
  2026-10-16 首批写合并开启时两版均在 unlink 后的最终同步返回 ENOENT，整批保留
  为无效尝试；后续独立 `flife16b` 仅比较写合并关闭，每版 5 个 CLI 与 5 个 SDK
  环境，三轮计时均通过。49 个指标逐项保留，单项区间未作多重比较校正。
- **分层文件系统负载 V2（协议、离线计划与guest写回浅测）：**
  `benchmark.new/filesystem/WORKLOADS.md` 与 `workloads_v2.py` 以机制、资源边界、
  完整任务三层替代七工具系统总分。quick目录19点（12+4+3），处理按适用case
  生成计划；`case×condition×pair`各自fresh环境、内部三轮，避免跨case互暖。
  三个完整工作流独立为主要应用结果，不合成任意总分；旧七工具及历史CSV保持原口径。
  guest WB、host WB与lower DATA分别进行隔离A/B，native仅参考；先逐family的
  N5探索及独立N1诊断，再按问题扩展单轴曲线。保留慢值和失败，无安静/干扰/清单门禁。
  离线计划不构建、不启动VM、不产生计时；独立`workloads_v2_run.py`接入19点的
  静态Rust payload、私有输入及真实git/C增量构建/Python离线解包发布，使用semspec
  执行。按用户快速采样要求可先N3配对，内部三轮，明确低样本探索范围；仅guest
  WB 0/1已接入执行。正式计时关闭profile，独立N1诊断只解释整case生命周期请求。
  工作与同步完成分列，准备/全量验证/退出另记；host验证最终保留字节与逻辑命名空间。
  实际CPU/内存预算须记录，guest队列/脏页峰值仍未知；不能用计划JSON更新用户表。
- **host-owned 写缓存曲线协议：** 问题是“相同显式持久化语义下，异步接管与合批
  随请求粒度、并发、屏障频率和容量如何影响完成吞吐、延迟与内存？”入口为
  `benchmark.new/filesystem/curves.py`，静态 Rust payload 在 native 与 VM 中使用相同字节。
  WB=0、DAX=0；同步、异步不合并、异步合并及资格检查锁消融使用同一冻结 host 制品，
  私有有界参数逐项改变，不做未经声明的全因子搜索。每个点分列 WRITE 返回、
  checked dup/close 的 FLUSH 排空、逐文件 fsync、最终 close 和总时间；off/on 都显式
  fsync，不能用 off 的 ENOSYS SYNCFS 对照 on 的真实 SYNCFS。准备、全字节读回和
  退出分开记录。先五个 seeded 随机配对新环境、内部三轮探索，独立单位为环境；
  选定主点另做 N30 确认，不合并两个批次。保留全部有效慢值及失败，无安静期、
  外部干扰或逐轮全量清单门禁。新 VM 仅称 fresh guest，不称冷磁盘；宿主缓存状态、
  backing 文件系统和整组资源预算明确记录。正式计时关闭 profile，B-FS-DIAG 独立核对
  WRITE/实际 backing 调用、字节与队列高水位。SDK 机制曲线与普通 CLI 七工具落地
  分开，不将前者绝对时间当 CLI Ready。参数选择后的验证使用独立样本；N5 结果只作
  探索，不能宣称完成论文级跨机器、开放负载尾延迟或掉电耐久验证。
- **guest writeback 合并后续协议：** 问题是“在相同文件与目录 fsync 完成边界下，
  guest 页缓存合并能否减少跨 VM 的 WRITE，而不只减少宿主 backing 调用？”新入口
  `benchmark.new/filesystem/guest_writeback_curves.py` 比较新固件 `off`、`guest-wb`、
  旧固件 `old-wb` 及同静态 payload 的 `native` 参照；host writeback=0、DAX=0，
  其他实验性缓存关闭。同一 host、payload 和 guest 配置下，`old-wb` 与 `guest-wb`
  均为 WB=1，单独隔离 0048 的增量；`off` 与 `guest-wb` 比较开启 writeback 的
  联合变化。独立 driver 仅通过冻结的 `CURVES_FIRMWARE` 选择固件并记录实际路径。
  旧 0047 固件 SHA256 为 `e032682ce7be163cc37db18e5e3b1d731795e24df6c20e75adac067fdf358b74`，
  新 0048 为 `f96af96339d40698faaa88d78992d550f8403137cdb58fcce3a4f9350a3d5dbb`。
  两者 kernel-source/config 摘要相同，旧补丁均保持，只新增 0048；SOURCE 和补丁随批次冻结。
  单独的 `guest_writeback_guest.rs` 在所有条件下省掉辅助 dup/close 阶段，
  `applied_ns=0` 明确为未测的保留字段；最终文件 fsync 承担 dirty-page 排空，
  total 保留写循环、文件 fsync、最终 close 与目录 fsync。write-close 小文件保留
  原本关闭成本，不用弱化同步语义制造收益。请求粒度覆盖 1/4/64/1024 KiB、
  1/2/4 个 worker、每 1/16/256 次或末尾 fsync、128 个 1 KiB 小文件及
  8/64/128 MiB 工作量。五个 seeded 随机配对新环境、内部三轮；独立单位为环境，
  不当作 N15。独立 N1 诊断核对全部注册/final/phase 库存及 WRITE 数、payload 字节、
  尺寸桶与实际协商状态；累计 max 不能相减成逐阶段 max。准备、完整 guest/host
  字节验证与退出在完成时钟外；保留有效慢值和失败，无安静期、干扰或逐轮清单门禁。
  这是 guest 内合并机制实验，不是普通 CLI、冷磁盘、RAM 可卸载或持久化崩溃验收。
- **guest 写回热路与更大合批窗口：** `guest_writeback_curves.py --baseline-patch 48`
  以同一冻结 host/payload 对比 0048 与只新增 0049 的固件，两臂 WB=1；另保留
  新固件 WB=0 与 native 参照，原 0047/0048 样本独立保留。0049 的预定变化是
  提交阈值改为 dirty 目标的 1/4（1–16 MiB），只对真正 pending batch 调度，
  热路提交提示使用近似 BDI 计数，去掉 write_end 后重复的 pressure 检查；
  大 WRITE 在原子提交中临时分配 scatterlist 的失败已由内核栈定位，候选固件改为
  每个 request virtqueue 启动时预分配最多516项 SG 和指针暂存区，提交时在原队列锁
  内复用；它只缓存传输元数据，逐请求 argbuf 与数据页的所有权不变。
  逐 folio 的真实压力、近阈值 full sum、10ms timer、256 leases、双屏障及同步语义不变。
  仍为四臂 N5、内部三轮、15 cells/900 行和独立 N1 计数，使用真实 semspec bench；
  同时报告 completed 和 accepted，不把1KiB已有 WB 收益算作0049增量。全部有效
  慢值保留，无干扰/清单门禁；新结果使用独立 tuning namespace，不覆盖0048来源。
- **普通 CLI guest writeback 落地对照：** 问题是“当前 host 与最终固件下，开启
  guest writeback 能否改善真实普通 CLI 七工具合计，而不仅是 SDK 小块写曲线？”
  独立入口 `benchmark.new/filesystem/guest_writeback_cli.py`，同一新构建的普通
  `pvisor` / `pvisor-cache`、同一最终固件和两份已公开准备的 owned rootfs/workspace
  generation，仅切换 `PVISOR_VM_FS_WRITEBACK=0/1`；host writeback、DAX、DATA、
  readonly FLUSH、NE、SQ 与 Core 两种缺项缓存均固定关闭，guest cache=1、目录容量256。
  每样本 fresh 私有 Host+VM，五个 seeded 配对环境、内部三轮原九操作；三轮七工具
  worker 时间之和是唯一主要指标，Ready 排除退出，原工作负载的同步/关闭与完整字节
  检查不变。小文件 write 为 write/close/stat、无显式 fsync；stream_write 含文件 fsync
  和 close，因此七工具 worker 合计不当作全部写入的持久化完成时间。独立 N1 预检和
  N1 profile，核对13个注册/final实例、每 mount 54个
  token、每 VM 216个 component interval、两设备实际 WB/批量 WB 协商及 WRITE 尺寸桶。
  32指标的5000次配对 bootstrap 不作多重比较校正，不将三轮当作N15；保留全部有效
  慢值和失败，无安静期、外部干扰或逐轮全量清单门禁。通过实际 `semspec bench`
  执行新批次；旧制品、旧时序和旧 SDK 数据不混入。准备与编译在测量外，只复用输入
  generation，不伪称重新封存，也不把该对照当作某个内核补丁的独立增量。
- **不可变 lower 读缓存曲线：** 问题是“同一物理不可变输入上，保留 guest 数据页
  随工作集与 reopen 复用程度减少多少 READ 和完成时间，超过 guest RAM 时在哪里
  失效？”`benchmark.new/filesystem/read_cache_curves.py` 使用同一冻结 public-API
  subject、相同物理 fixture、WB=0/DAX=0，仅切换 root-data/off。工作集覆盖
  1/16/64/256/384/448/512/768 MiB，在 RAM 边界加密取点；每 cell 连续五遍
  reopen/read/close，分列首遍与后四遍；
  每个新 guest 内部三轮，五个 seeded 随机配对环境，native 同静态 payload 作参照。
  读取与全字节核对同计入 cell 时钟，输入准备、独立 host 全字节验证和退出另列；
  该 read/check 口径独立于写曲线，不能混算绝对吞吐。2 vCPU/512 MiB guest 与
  两核/2 GiB enclosing 预算固定；预准备且在宿主热缓存中的共享输入页可能预先
  记账于父组之外，需明确披露，不能据此宣称整机物理内存节约。fixture 在准备时
  完整核对并保存摘要，采集后复核；诊断另跑 B-FS-DIAG，要求所有注册实例都有
  最终记录并核对首遍/复用遍 READ。N5 只作探索，不报告操作 P99 或冷磁盘结论。
- **封存输入与缓存资格的四组快速对照：** 同一当前 host/0045 固件，WB=0、DAX=0，
  比较当前 Mutable 声明、只为封存 rootfs 接通 Immutable、只为工作区快照接通
  Immutable、两者同时接通。rootfs 四组使用同一个真实封存的物理目录，只切换
  Mutable / Immutable 声明。工作区基线与 rootfs-only 条件使用独立新拷贝作为
  live Mutable lower；workspace-only 与 both 使用初始字节相同的封存快照 lower、
  frozen baseline 和 Immutable 声明，apply target 仍是独立的 trial 工作区。
  工作区对照同时改变缓存资格和 frozen-baseline 的读观察/journal 工作，结果属于
  联合处理收益，不能单独归因于 metadata cache；不把活跃目录声明为 Immutable。
  工程入口使用正常 PVisor Job、stage/journal、
  VmExecutor 和 native runner；省略 CLI Host 服务，绝对 Ready 不与 CLI 用户表混用。
  封存 generation 的导入/准备单独记录，所有权 lease 持续到 native teardown。
  每条件五个新环境，同轮 seeded 随机交替，每环境沿用九操作三轮，先逐环境合计
  七工具再统计；Ready 排除工具和退出，保留字节/Bundle/lower 隔离及正常退出校验，
  无干扰、安静期或每轮全量清单门禁。另跑四组各少量独立计数 VM，通过同步阶段
  checkpoint 核对每轮每工具的 rootfs/workspace OPEN、READ、LOOKUP、READINDEX
  和物理查询；计数含该工具进程树启动/校验，与 worker 时钟范围分别说明。
  入口 `filesystem_cache_matrix.py`；原始入口/制品/准备/失败留在 `.data/`，
  工程结论进入 `design/filesystem-performance-analysis.md`，不更新跨运行时用户表。
- **普通 CLI 封存 rootfs 入口快速对照：** 同一冻结真实 CLI 与 0047 固件，
  比较同一封存物理 rootfs 的普通 `--rootfs PATH` 与公开认证的
  `--rootfs-base-store STORE --rootfs-base ID`；通过公开 SnapshotStore API
  保留持久 catalog pin，准备在时钟外。两组工作区均为新复制的 live Mutable
  lower，不接入工作区快照或 frozen baseline。处理同时改变 rootfs 所有权/缓存
  资格和 execution checkpoint 能力/准入；记录 Bundle 中的声明能力及实际 control 配置，
  私有临时 native binding 仅能从冻结源码推断时明确标为未知，不将 Ready 差异
  单独归因于 cache，不为对照绕过能力或修改产品。
  每组五个 fresh private Host/VM 环境，seeded 随机配对，每环境九操作三轮。
  每个样本独立 user/mount/PID namespace、owned pivot_root、真实复制 CLI 与
  私有 Host authority，不接触既有用户 listener。Ready 包含对称 fresh Host+VM
  启动并排除退出，worker 保持原任务时钟。2 vCPU/16 GiB、父预算 CPU0/1
  两核及 swap0，WB=0、DAX=0、容量256、DATA/FLUSH/标准负项/两项 Core
  缺失缓存全0。保留原退出、唯一 Ready/Result、工具缓存、真实 Bundle、live
  lower 隔离及全部写文件/stream 字节检查，无安静/干扰/全量清单门禁。
  主要指标为逐环境三轮七工具累计，另保留其余31指标和全部五对原值，5000次
  环境块配对 bootstrap，区间未校正多重比较，不将三轮当独立样本或报告尾延迟。
  两组各另跑一个独立完整计数环境，复用带同步 marker 的封存 rootfs，但工作区
  仍 live；核对全部注册/final/phase、各挂载容量和实验开关实际值，计数与计时
  分开，inclusive spans 不相加。入口与冻结证据保留 `.data/pc17/`，结果只进
  工程分析，不更新跨运行时用户表或合并旧批次。
- **普通 CLI 工作区快照入口快速对照：** 两组固定同一个已认证 prepared-rootfs
  selector 与物理 generation，仅比较 live Mutable 工作区和公开 CLI 认证的
  `--workspace-base-store STORE --workspace-base ID`。工作区快照由
  `pvisor-cache prepare-workspace --workspace SOURCE --base-store STORE` 在计时外
  准备并保留独立费用，rootfs/workspace 使用独立 store。快照组以所选固定
  generation 为唯一 Immutable lower 与 frozen baseline，apply target 保持
  独立 trial 工作区，fresh stage 在正常 RunLease 内创建，不冒充 execution restore。
  这是所有权/缓存资格与读观察、journal 语义的联合处理，不能单独归因 metadata cache。
  每组五个 fresh private Host/VM 环境，seeded 随机配对，每环境九操作三轮，
  主要指标为逐环境三轮七工具累计；Ready 包含 fresh Host+VM 启动并排除退出，
  worker 保持原任务时钟，stream_write 保留自身 fsync/close。2 vCPU/16 GiB、
  父预算 CPU0/1 两核及 swap0，WB=0、DAX=0、容量256、guest cache=1；
  新 DATA、只读 FLUSH 消除、标准负项 entry、两项 Core 缺失缓存及同次 OPEN
  资格复用全部关闭。保留唯一 Ready/Result、正常退出、terminal Job/Bundle、
  实际 selector/lower/baseline/live target、工具缓存、lower 隔离以及全部写文件和
  stream 字节检查，无安静期、干扰或全量清单门禁，保留全部有效慢样本。
  三轮不作为独立样本；5000 次环境块配对 bootstrap，32 个区间不校正多重比较，
  不报告尾延迟。两组各另跑一个独立逐工具计数环境，按冻结构造核对全部
  注册/final/phase、挂载实际开关、OPEN/READ/LOOKUP/GETATTR 与 Core/journal
  请求，inclusive spans 不相加。新制品、准备、harness 和失败留在 `.data/pw17-cli/`，
  不复用历史计时；结果仅进入工程分析，不更新跨运行时用户表。
- **目录 FD 缓存容量快速 A/B：** 复用封存输入对照的正常 Job/Overlay/VM 工程入口，
  固定 rootfs 与 workspace 均为真实 owned Immutable、workspace 使用 frozen baseline，
  同一 host/0045 固件，仅将有界 native directory-FD cache 容量设为 256 / 1024。
  每组五个 fresh 环境、每环境原九操作三轮，同轮随机配对；WB=0、DAX=0，
  Ready 排除工具与退出，逐环境三轮七工具累计另列，不将三轮作为独立样本。
  无干扰/安静期/全量清单门禁，保留慢有效样本及原有完整字节、Bundle、lower 隔离校验。
  profiling 关闭的计时与每组一个环境的完整逐工具计数分批，核对实际容量、cache miss、
  eviction、OPEN/READ/LOOKUP/GETATTR 与 Core 查询，不从减少计数直接推算耗时。
  入口 `filesystem_cache_capacity.py`；ATIME 的 read→statx 请求掩码因果探针属于独立
  B-FS-DIAG，只说明属性失效机制；工程结果进入设计分析，不改变跨运行时用户表。
- **Immutable 物理 lower 负项快速 A/B：** 同一新 host/0045 固件、相同封存输入与
  256 容量 directory-FD cache，显式关闭/开启 `PVISOR_IMMUTABLE_LOWER_NEGATIVE_CACHE`。
  仅保存 eligible Immutable 物理层的 MissingLeaf 及其父身份；仍逐次检查 upper、
  whiteout、opaque、策略、读观察与后续 lower，不缓存合并视图不存在、错误或
  UnavailableParent。每组五个 fresh VM、九操作三轮，WB=0/DAX=0、Ready 排除退出，
  沿用上一快速 A/B 的隔离/字节校验、随机配对、无干扰/清单门禁与全部有效慢样本。
  同一入口的显式 variants 定义只改变该缓存开关，独立 N1 计数核对实际开关、负命中、
  物理查询、普通 OPEN/READ/LOOKUP，五环境配对区间和新增回归只进入设计分析。
- **Immutable regular lower 属性期限快速 A/B：** 固定上述 owned inputs、容量 256、
  负缓存关闭、WB=0/DAX=0，显式关闭/开启 `PVISOR_VM_FS_IMMUTABLE_ATTR_CACHE`。
  只对全部 lower Immutable、LinuxComplete、实际命中单硬链接 regular lower 的普通
  LOOKUP/无 FH GETATTR 响应延长 attr TTL；entry TTL、upper、Mutable、目录、符号链接、
  多硬链接和 FH 属性保持原超时。沿用已有 upper 独占变更契约及 READ 后 ATIME、guest
  mutation 失效。先完成真实 guest copy-up/硬链接/替换/属性变更一致性探针，再运行每组
  五环境、九操作三轮的随机配对；Ready 和 worker 排除退出，无干扰或清单门禁。
  正式计时与独立 N1 全阶段计数分开，检查实际开关、GETATTR/LOOKUP/OPEN/READ 与
  Core 查询。只进入工程设计分析，不改变默认开关或用户跨运行时表。
  该候选已因 O_PATH hardlink source 的 nlink 陈旧被拒绝并撤除；只保留独立诊断及
  `.data/` 冻结制品，没有正式工具时间，不应以当前产品制品调用此旧开关。
- **独占 upper 物理不存在缓存快速 A/B：** 固定 owned Immutable rootfs/workspace、
  容量 256、lower 负缓存关闭、WB=0/DAX=0，仅切换
  `PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE=0|1`。借助 VM 已有独占 upper
  namespace 及稳定物理祖先契约，在连续只读 epoch 内缓存物理 upper 的 MissingLeaf /
  UnavailableParent 与完整父证明；不缓存 Entry、错误、merged winner、whiteout、
  opaque、权限或 journal 决策。每个 write_operation 获得独占锁后禁用并清空，Drop
  再清空后恢复，覆盖失败及部分 mutation；journal 与 upper 重叠不能启用。
  默认关闭，Core/host 通用路径不推断此所有权。先运行定向语义测试与新环境预检，再
  五环境每臂随机配对、九操作三轮，Ready/worker 排除退出，无干扰/安静期/全量清单
  门禁，保留全部有效慢样本和输出、Bundle、lower、完整字节校验。独立 N1 计数核对
  实际开关、命中/清空/失效、OPEN/READ/LOOKUP/GETATTR 与 Core 查询；不改变 guest
  TTL、数据页、WB 或 DAX，工程结果不更新用户跨运行时表。
- **Immutable local 只读 FLUSH 消除快速 A/B：** 固定 owned Immutable rootfs/workspace、
  容量 256、lower/upper 不存在缓存关闭、WB=0/DAX=0，同一新 host 与新固件仅切换
  `PVISOR_VM_FS_READONLY_FLUSH_ELISION=0|1`。连接 bit 43 协商后仅对合格的本地
  Immutable 只读句柄设置 FOPEN bit 29；guest 保留写同步与错误检查，再省略 backend
  FLUSH。可写、upper、Mutable、原生 FD 位于远程/未知文件系统、未标记句柄仍走原路径；不改变页缓存、TTL、
  OPEN 或 RELEASE 语义。先做 guest 只读/写入/错误与兼容性预检，再每臂五个 fresh VM、
  九操作三轮随机配对，Ready/worker 排除退出，无安静/干扰/全量清单门禁，保留全部
  有效慢样本及完整原有校验。独立 N1 诊断逐挂载核对实际开关，检查消除的 FLUSH 与
  OPEN/READ/RELEASE、Core 查询，完整注册/final/phase 覆盖；结果仅进入工程分析。
- **Immutable lower 独立数据映射快速 A/B：** 回答独立 lower 数据 inode、固定 EOF 与
  跨 OPEN 页复用能否减少驻留工具 READ 并改善耗时。同一新 host/0047 固件仅切换
  `PVISOR_VM_FS_LOWER_DATA_CACHE=0|1`；两组均为 owned Immutable rootfs/workspace、
  frozen baseline、容量 256、WB=0/DAX=0，并关闭两项缺失缓存及只读 FLUSH 消除。
  普通 OPEN 的 Core/策略/journal/native 绑定保留；新 bit 44/OPEN bit 28 给出稳定
  connection-local lower 对象 ID 和固定 size，guest 为合格只读对象分离 data mapping，
  upper/Mutable/多硬链接/不合格句柄保留原路径。对象 ID 表有界且会话内不复用含义；
  实际协商会话暂拒绝快照，不从 Immutable 元数据资格推导数据页安全。
  原 review-pages 四个 oracle 不变，候选须消除其失败；新增真实 guest cross-fd、
  mmap/COW、非页齐 EOF、truncate、rename/unlink、关闭与回退用例，记录既有
  fd 属性/copy_file_range 边界。通过候选正确性后每臂五个 fresh VM、九操作三轮，
  seeded 随机配对，Ready/worker 排除退出，无安静/干扰/全量清单门禁，保留全部
  有效慢样本与原有完整校验。另以 N1 独立逐工具计数核对实际协商、对象 ID 注册/
  复用、OPEN/READ/GETATTR/FLUSH/RELEASE 与全部注册/final/phase 完整性。
- **数据页缓存与只读 FLUSH 的四臂组合：** 同一冻结 host/0047 固件比较
  两项全关、仅 data、仅 FLUSH、两者开启的完整 2×2 组合；显式
  `treatment_design=lower-data-local-flush-factorial`，只有这两项开关变化，
  容量 256、两项缺失缓存关闭、WB=0、DAX=0、owned Immutable 输入与
  frozen baseline 相同。预先指定全关→两者、FLUSH→两者、data→两者三个
  主要对照，补充全关→各单项，共五个对照各 32 个指标，不从旧批次相加收益。
  先用组合配置运行现有数据/整页/关闭/ATIME oracle，再每臂五个新环境，
  每环境九操作三轮，固定种子块内随机交替；Ready 和 worker 排除退出，
  无干扰、安静期和全量清单门禁，保留全部有效慢样本与原有完整校验。
  以五个环境块配对 bootstrap，三轮不当独立样本，主要三轮七工具累计
  和逐轮单项分别报告区间，未校正多重比较。另每臂一个独立计数环境，
  逐设备严格核对两项实际协商 0/1、两项 Core 缓存为 0、全部 final/phase，
  检查 READ、FLUSH 与剩余 OPEN/RELEASE；结果仅进入工程分析。
- **普通 CLI 数据页与只读 FLUSH 四臂对照：** 在真实 `pvisor run`、fresh Host、
  native runner 路径中复用同一已验证 CLI/0047 制品与已认证的 rootfs/workspace
  固定 generation，比较 DATA/只读 FLUSH 的 00、10、01、11。所有条件都使用
  workspace frozen baseline 和独立 live apply target，不改变来源或重新封存；
  已有准备成本仅引用原始收据，旧批次耗时不导入。SQ、标准负项 entry、两项
  Core 缺失缓存、WB、DAX 全0，guest cache=1、目录 FD 容量256。guest 2 vCPU/
  16 GiB、父预算 CPU0/1 两核 quota、16 GiB/swap0，独立私有 namespace/Host。
  每组五个 fresh 环境，每环境九操作三轮，固定种子按环境块随机交替。预定三个
  主要三轮七工具累计对照为 00→11、01→11、10→11，补充 00→10、00→01；
  每个对照保留32指标、五对原值与5000次环境块配对 bootstrap，区间不作多重
  比较校正，三轮不作独立样本。Ready 包含 fresh Host+VM 并排除退出，worker
  保持原时钟，stream_write 保留自身 fsync/close。先每组一个真实 CLI 正确性
  预检，保留正常退出、唯一 Ready/Result、terminal Job/Bundle、来源 receipt、
  lower/cache 隔离及全部写/stream 字节检查；不放宽已有 fd/mmap/copy-up oracle。
  无安静期、干扰或每轮全量清单门禁，保留全部有效慢样本。另每组一个独立计数
  环境，按源码构造库存核对全部注册/final/phase 与每挂载 DATA/FLUSH 实际协商，
  核对减少的 READ/FLUSH 与剩余 OPEN/RELEASE、GETATTR、Core 查询；计数不作
  耗时或磁盘 I/O 归因。原始制品关联与新测量保留 `.data/cdf17-cli/`，仅发布工程
  分析，不改用户跨运行时表，不据此默认开启候选或宣称未覆盖的对象语义已修复。
- **同次 OPEN 资格查询复用快速对照：** 回答 DATA 与只读 FLUSH 同时启用后，
  复用同一实际 native FD 的资格证据能否改善普通工具耗时。新冻结 host 与既有
  0047 固件仅切换 `PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION=0|1`；DATA=1、
  只读 FLUSH=1、标准负项及两项 Core 缺失缓存=0、容量256、WB=0、DAX=0，
  两组均为 owned Immutable rootfs/workspace 与 frozen workspace baseline。
  原 Core/策略/journal/native OPEN、FD lease、第二次 backend 检查及对象 ID/EOF
  约束保留；证据只在本次调用栈传递，不跨 OPEN 缓存授权。先保留原整页、数据、
  close、ATIME、负项及 exclusion 真实 guest oracle，并检查单项能力和旧 peer 回退。
  正式每臂五个 fresh 环境、九操作三轮，seeded 随机配对；Ready/worker 排除退出，
  无安静/干扰/全量清单门禁，保留全部有效慢样本及原字节/Bundle/lower 校验。
  主要指标是逐环境三轮七工具累计，5000次环境块配对 bootstrap；三轮不当独立样本，
  单项区间未校正多重比较。另每臂 N1 独立计数，按真实构造核对完整实例库存、
  注册/final/phase、挂载实际开关及四类 native 资格查询尝试（含显式零）。
  查询调用点减少不是整条 OPEN 的 syscall 数；两臂内部 `open` span 边界不同，
  不直接比较或相加，使用完整 RPC/worker 时钟。入口 `filesystem_cache_capacity.py`，
  冻结制品和原始证据保留 `.data/sq17*/`，结果只进入工程分析，不改用户性能表。
- **标准负项 entry 缓存快速对照：** 同一冻结 host 与既有 0047 固件，仅切换
  `PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE=0|1`；DATA=1、只读 FLUSH=1、容量 256、
  两项 Core 缺失缓存关闭、WB=0、DAX=0、owned Immutable rootfs/workspace 与
  frozen baseline 相同。候选只把成功受检且完成缺失观察的 merged absence
  返回为标准 `nodeid=0`、entry TTL 1 秒，不延长正项或属性 TTL；未知错误、
  Mutable、远端投影、非默认策略及未满足独占命名空间契约的路径不启用。
  固定 exclusion 与 synthetic 路径按祖先、路径自身及子树保留原查询；只有无关
  路径可缓存受检缺失，不能因普通 rootfs 的 Host 控制目录隐藏规则关闭整个 owner。
  先以真实 guest 重复同名缺失、TTL 到期、create/mkdir/symlink/link/rename、
  父目录移动、失败修改、权限、并发及 Mutable 外部创建验证可见性与请求减少，
  另验证 exclusion 已有文件不可见、创建仍拒绝、缺失祖先保留原查询及允许的兄弟路径。
  不改原有数据/关闭/ATIME oracle。正确性通过后每臂五个 fresh 环境、九操作
  三轮，seeded 随机配对；Ready/worker 排除退出，无安静/干扰/清单门禁，
  保留全部有效慢样本与原有字节/Bundle/lower 校验。以五个环境块做配对
  bootstrap，主要指标为三轮七工具累计，单项区间未作多重比较校正。
  另每臂 N1 独立计数，核对 mounted owner 的 actual 资格/负项回复、两项已协商
  能力与全部注册/final/phase；不把 debug/profile 时间混入性能，不合并旧批次。
- **普通 CLI 标准负项 entry 快速对照：** 固定只读 FLUSH=1、DATA=0，
  仅切换 `PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE=0|1`，验证真实 CLI 的 npm 缺项
  查询减少是否改善三轮七工具累计。两组使用相同已验证 release/0047 固件、
  已认证 prepared rootfs/workspace generation、frozen baseline 与独立 live apply
  target；引用原构建、准备及源摘要，不重新导入或混入旧批次计时。
  Core 两项缺失缓存、SQ、WB、DAX 全0，guest cache=1、目录 FD 容量256。
  负项资格、权限/别名、exclusion/synthetic 路径与修改失效契约保持，不延长属性
  或正项 TTL，不把任意缺失或其他 errno 当作可缓存 ENOENT。
  每组五个 fresh private Host/VM 环境、九操作三轮，按环境块 seeded 随机配对；
  唯一主要指标为逐环境三轮七工具累计，保留全部32指标与五对原值，5000次
  环境块配对 bootstrap、区间不校正多重比较，三轮不作独立样本。
  Ready 包含 fresh Host+VM 并排除退出，worker 保持原时钟，stream_write
  保留自身 fsync/close。每组另跑一个正确性预检和一个独立计数环境，保留正常
  退出、唯一 Ready/Result、terminal Job/Bundle、两来源 selector/receipt、
  lower/cache 隔离及全部写/stream 字节检查；核对每挂载 NE requested/enabled、
  DATA0/FLUSH1、完整注册/final/phase、LOOKUP/GETATTR/OPEN/READ/FLUSH/
  RELEASE 与 Core 缺项/父叶查询。LOOKUP 不等于 ENOENT，miss 不等于唯一名字，
  计数不解释为磁盘 I/O 或耗时份额。无安静期、干扰或每轮全量清单门禁，保留
  全部有效慢样本；原始证据在 `.data/cne17-cli/`，仅更新工程分析，不改用户表。
- **普通 CLI 封存输入的自动只读 FLUSH 快速对照：** 同一新 CLI release、0047
  固件及已认证 prepared rootfs/workspace generation，比较显式
  `PVISOR_VM_FS_READONLY_FLUSH_ELISION=0` 与该变量真正缺省。候选仅在已持有
  匹配 opaque prepared-rootfs owner、fresh 且不支持执行捕获/恢复的 runtime
  native child 中补默认；普通路径、仅 SDK `with_rootfs_base`、一般 Immutable
  share 的默认及 checkpoint 合同保持。明确0、非法值及旧 peer 回退分别核对，
  不通过全局环境修改或扩大 Immutable 声明取得资格。
  固定 DATA、NE、SQ、两项 Core 缺项缓存、WB、DAX 全0，guest cache=1、
  目录 FD 容量256；同一 frozen baseline、独立 live apply target。引用既有公开
  输入准备收据，新构建与新时序独立记录，不导入旧批次样本或相加收益。
  每臂五个 fresh private Host/VM、九操作三轮，seeded 环境块随机配对；唯一
  主要指标为三轮七工具累计，32个未校正95%区间、5000次环境块配对 bootstrap。
  Ready 包含 fresh Host+VM 并排除退出，worker 保留原时钟及 stream 自身
  fsync/close。各臂另一个正确性预检和独立计数环境，核对 caller 的变量真缺省、
  owner 的 requested/enabled/marked OPEN、全部注册/final/phase 和协议请求；
  profile 不计入性能。保留正常退出、唯一 Ready/Result、terminal Job/Bundle、
  selector/baseline receipt、lower/cache 隔离及全部写/stream 字节校验，无安静期、
  干扰或每轮全量清单门禁，保留全部有效慢样本。证据在 `.data/af17-cli/`，
  结果仅进入工程分析，不改用户跨运行时表。
- **新增独立 kernel-cache 实验：** `kernel_cache_runner.py` / `kernel_cache_driver.rs`，同一冻结 release 制品 native、legacy-writable（immutable 物理缓存开启，默认 1s）、metadata-writable60s、metadata-readonly60s、metadata-and-data-readonly60s；2048/32 half-deep、全字节验证、3 warmups/30 seed-shuffled samples、CPU 0,1；hot、legacy-TTL-expired/extended-TTL-warm、single-pass readsearch、verified git/rg 与 startup/mount/task/unmount。 全部 native/lower/upper/work 使用相同 private user/mount/PID namespace 的真正 noatime tmpfs backing；保留 mountinfo、past-atime 读验证、监督/终止回执；旧 Btrfs/future-atime kernel-cache 批次未验收，只作历史诊断，raw 保留且不可拼接。writable 必须保留 fusectl abort guard；独占 fresh upper/work、稳定 immutable lower，无 journal/preimage/metrics/custom policy/exclusions；KEEP_CACHE 只在两种 readonly 条件间归因。报告 `benchmark/pvisor/KERNEL_CACHE_REPORT.md` 仅属 Linux HOST API 工程实验，不代表 pvisor run/VM/review guarantees。
- **入口脚本：** `filesystem_ab.py`、`filesystem_stage_durability.py`、`filesystem_lazy_ab.py`、`immutable_lower_cache.py`（同一 release 二进制 native/mutable/immutable-cache-off/on，独占 immutable lower，真实 host FUSE 的 hot/TTL-expiry 重复 metadata/open/read、readsearch 与 upper 正确性；无 journal，排除 review；工程报告 `benchmark/pvisor/IMMUTABLE_LOWER_CACHE_REPORT.md`）。

- **宿主 copy-up 调度对照：** `host_copy_up_ab.py` / `host_copy_up_driver.rs`；从同一工作树冻结两份 release 源码，只撤销/保留宿主调度和 prepared copy-up 改动。macOS FSKit，同一 APFS lower，1 GiB 非稀疏固定数据、64 个独立 metadata 路径，关闭日志与 compact strict 日志分别成条件；3 次预热、30 个随机配对轮次。观察实际临时文件后立即测首次 stat 与 64-path metadata，另测 writable-open 完成和空闲 metadata；完整 upper/lower 内容、基线观察和卸载校验。任何错误、未命中复制窗口或检测到构建/测试干扰使批次失败；保留慢有效样本，不外推 Linux、冷磁盘、并发吞吐或整个 staged 任务。报告和加工 CSV 留在 `benchmark/pvisor/`，原始证据在 `.data/`。

- **读队列与 journal 锁对照：** `host_read_journal_ab.py` / `host_read_journal_driver.rs`；冻结同一源码，仅撤销/保留读调度与指纹锁改动。macOS FSKit 的实际部分 copy-up 窗口中并发测只读 OPEN、已打开描述符 READ、OPENDIR，关闭日志、legacy 与 compact strict 分条件；另以共享 Core 的真实 1 GiB 原生文件指纹计算测 unrelated journal 观测，legacy/compact 独立成批。原生文件访问时间变化证明首次读已发生，开始探测时哈希线程必须仍在运行，不人工延迟。每格 3 次预热、30 个 seeded 随机配对样本，空闲对照、完整内容/观察/卸载和干扰门禁、配对 bootstrap 95% CI；不合并复制与哈希条件，不宣称冷磁盘、Linux、VM、吞吐或端到端工具收益。报告/加工 CSV 在 `benchmark/pvisor/`，原始来源和失败在 `.data/`。

- **Linux staged 空闲调度对照：** 使用 `reference_baselines.run_trial` 的原 task timer，以冻结的 coordinator/namespace helper 随机配对同源码基线与仅修改 host dispatch 的候选，API/README 和私有测试同步更新。CPU 0,1、整父组两核 quota / 16 GiB / swap 0，Btrfs 热宿主缓存、相同空工具缓存与 Python bytecode 策略；每样本独立 user/mount/PID namespace、owned pivot_root 和 Host listener，namespace/输入准备不计时。3 warmups、30 配对轮次、完整七工具输出/Run Bundle/宿主隔离/upper 字节校验、前后输入清单与外部 0.5 s 构建/测试干扰门禁。插桩关闭、保留有效慢样本，5,000 次配对 bootstrap，分离簇不作整体排名。raw helper、冻结源码、制品和失败/中间尝试保留 `.data/`；整理 CSV 与结论仅进入 `design/filesystem-performance-analysis.md#host-idle-dispatch`。不作为 hyperfine 数据，也不与跨运行时用户批次或 macOS/VM 结果合并。

- **Linux VM worker 调度对照：** 同一冻结源码的三版本配对：撤销 `65e65c93`/`09bd1d59` 的优化前版本、保留当前优化与 host 修复的基线、仅修改 VM worker 调度/私有测试及 README 的候选。每版本 3 warmups、30 个随机同轮样本；独立 namespace、owned pivot_root、fresh VM/Host listener，整父组 CPU 0,1 / quota 两核 / 16 GiB / swap 0，同 B-FS-TOOLS 七工具及输入、缓存、逐字节 upper、宿主隔离与干扰门禁。使用原 reference timer，profiling 关闭；与当前基线的差异用于候选归因，优化前版本只用于同批恢复程度对照。5,000 次配对 bootstrap，分离簇不报整体排名；各候选批次独立保留，只发布通过门禁的最终制品，不合并样本。原始 helper、冻结源码、构建和诊断在 `.data/`，加工 CSV 与结果仅进入 `design/filesystem-performance-analysis.md#vm-completion-dispatch`，不重算跨运行时用户表，不外推 macOS、冷磁盘或并发吞吐。

- **不可变 share 的 DAX 开关对照：** `benchmark.new/filesystem/dax_ab.py` 准备同一优化制品、固件和物理不可变输入，生成 `in.json` 后通过 `semspec bench` 执行 Python 配对行；不使用 hyperfine。两组均通过公开 VM API 建立只读 DAX-capable share 和 128 MiB 窗口，仅 guest 挂载选项 `dax=never` / `dax=inode` 不同；statx 必须验证实际 DAX 属性。宿主热缓存、新 guest、独占 rootfs upper/work；完整 VM 组 CPU 0,1、两核 quota、2 GiB、swap 0，guest 2 vCPU/512 MiB。静态 C 探针测独立 64 MiB 文件的顺序读取、双线程读取和 mmap 页遍历，以及 128×64 KiB 小文件 open/read/close、2,048 路径 metadata；首次数据访问与 guest 内重复访问分列，全量内容验证在阶段计时之外。小文件组有超过窗口容量的映射，保留其正常回收成本。各组 3 次预热、30 个预先随机平衡配对，共 60 个正式 VM；5,000 次配对 bootstrap，先检查分布，保留有效慢样本，不报告 P99。校验错误、输入/制品变化、非零退出、预算不符、OOM 或外部构建/测试干扰使批次失败；报告失败和剔除数。冻结 harness、源码、制品、全部日志与样本在 `.data/`，加工表和 CSV 为工程报告；不将只读 share 的结果外推到 CLI 可写 workspace、启动退化或磁盘冷缓存。

- **guest 写合并开关对照：** 同一冻结的直接队列 GNU release/gateway 制品，仅切换宿主 runner 的 `PVISOR_VM_FS_WRITEBACK=1` / `0`，由专用 worker 恢复并校验原环境。Linux VM 的 rootfs/workspace 均无 DAX 窗口；与 B-FS-TOOLS 相同的七项操作、完整任务和校验，整父组 CPU 0,1、两核 quota、16 GiB、swap 0，guest 2 vCPU/16 GiB。热宿主缓存、每次私有 namespace/pivot_root、独立工作区与工具缓存；每臂 3 warmups、30 个 seeded 随机配对，共 60 个正式任务。输入与制品摘要、完整 upper 字节、Run Bundle、正常退出、预算和外部构建/测试门禁通过后才能发布；保留慢有效样本，分离簇不给整体排名，5,000 次配对 bootstrap，不报告 P95/P99。原始及冻结执行入口保留 `.data/writeback-ab-*` 与 `.data/writeback-quick-*`；工程表和 CSV 进入 `design/filesystem-performance-analysis.md#writeback-ab`，生产默认值和跨运行时用户表不由该开关实验改写。

- **guest 目录镜像与惰性 OPEN：** `guest_cache.py`，同一冻结 release CLI/SDK 和固件切换 `PVISOR_VM_GUEST_CACHE=0/1`，writeback 固定关闭。普通 `--safe` CLI 工作区与 SDK 显式 Immutable lower 独立成批；2 vCPU/4096 MiB、热宿主输入、新 VM/upper、3 warmups/30 seeded 随机配对。分列 2048 路径首次/重复 stat、深目录 stat、目录枚举、64 MiB 读取、1000 次热 open/fstat 与 open/read、宽目录单文件访问及 1.3s 空闲后 stat。校验全部数据、退出/Bundle 和输入不变，保留全部有效慢样本，配对 bootstrap 95% CI、先检查分布，不报告 P99。独立 B-FS-DIAG 运行用 prime-only 差分核对 READINDEX/OPEN/GETATTR 等请求，不把 profiling 耗时混入正式样本；声明资格、TTL 和跨文件数据缓存策略差异，不将 SDK 不可变 share 外推到普通 CLI 或整个 Agent。报告、来源、失败和完整数据保留 `.data/guest-cache-*`。修复预取退化时，使用 `--baseline-firmware` 在同一 CLI/SDK/payload 下比较旧/新固件，两臂 guest cache 均为 1，仍采用相同的随机配对、分布与校验规则。自适应预取由 `guest_cache_adaptive.py` 同批比较原始预取、仅枚举加载和候选三种固件（均 cache=1），增加宽目录两文件、32 随机文件、32 跨步文件及全量连续 stat，检验利用率门槛和恢复程度；预算、30 配对轮次、独立计数和校验规则相同。

- **下沉与跨文件写回整体收益：** `guest_lowering_writeback.py`，Apple Silicon/HVF，同一冻结 CLI/SDK、guest 和固件，`PVISOR_VM_GUEST_CACHE=0/1` × `PVISOR_VM_FS_WRITEBACK=0/1` 四臂；普通 `--safe` CLI 与 SDK Immutable share 分列。每 VM 2 vCPU/4096 MiB，热宿主输入、fresh VM/upper，3 warmups/30 seeded 随机配对；沿用目录镜像的 17 个元数据/读阶段，追加 defer、tiny、16 MiB boundary、24 MiB pressure、重复覆盖、单 fd 小块流写及逐文件 fsync。连续测写循环、syncfs 和两者总耗时，另记含启动/验证/收尾的任务完成时间。补充派生统计可先在每个任务内合计七个写阶段，再作配对分布；不得相加中位数或百分比，也不代表真实 Agent 权重。全部字节由 guest 与 host 校验；正常退出和 CLI Bundle 必须通过，输入/制品/harness 前后验摘要；成功 stage 经逐成员校验后压缩保留，失败现场不清理。无 host CPU affinity/cgroup 或无外部干扰保证，不剔除慢有效样本；各 cohort 内配对 bootstrap 5000 次，分离分布不作整体排名，不报告 P99。另跑 B-FS-DIAG 的独立 fresh VM 请求数与 prime-only 差分，不混入计时。技术报告/CSV 留在 `benchmark/pvisor/`，不能据此推断完整 Agent 或跨容器排名。执行时可用 `lowering_writeback_guard.py` 加入 5 GiB 启动余量和 1 GiB 运行保护线；源代码、参数及每 0.2s 的磁盘观测随批次保留，触线整批失败，不补跑或拼接样本。

### B-FS-DIAG：文件系统请求成本分解 {#b-fs-diag}

- **guest writeback 流水诊断：** `benchmark.new/filesystem/guest_writeback_pipeline.py`，
  同一冻结 SDK host/guest 以旧 0047 与新 0048 固件分别开启 WB，关闭 host 写缓存与 DAX。
  只在独立诊断中用 LD_PRELOAD 将文件 A 的首次 backing pwrite 延迟 1.5s；guest 在
  A close 后 200ms 写文件 B，以宿主单调时钟记录 A/B backing 写入开始、完成。
  核查真实拦截、全部字节、正常退出，判断 B 是否在 A 完成前到达 backing；保留未命中
  和失败，不作为性能时钟、吞吐或真实设备延迟结论。通过 semspec bench 执行，
  原始源码、制品摘要和日志在独立 `.data/guest-writeback-pipeline-*`。
  0049 后续用同一 payload/延迟与新冻结 SDK host，再以 0048/0049 各 WB=1
  独立复核，检查放大合批提示后仍能推进后续文件；不复用旧事件或混作性能样本。
- **角色：** diagnostic
- **Motivation：** 定位 FUSE 传输、OverlayCore、持久化、内容指纹和缓存路径的成本。
- **大 WRITE 内存失败定位：** `guest_writeback_memory.py` 对失败批次冻结的
  plan001、同 SDK host、同512 MiB guest/2 GiB父组和全部三轮做独立复现；
  只在128 MiB同步前及操作错误时读取guest meminfo/buddyinfo与klogctl。
  五个fresh复现上限，首次失败后停止，保留所有尝试、源码/制品及实际semspec入口；
  不扩大RAM、不dropcache、不减少校验，也不把插桩时钟当正式性能数据。
- **想要的结论：** 请求与 inclusive span 的成本分解，不能相加为精确归因，不作为用户性能数据。
- **同次 OPEN 资格复用计数：** 在上述 SQ0/1 的独立 N1 owned-input 环境中，
  固定 DATA/FLUSH=1、负项/Core 缺失缓存=0、WB0/DAX0、容量256。按真实 SDK
  构造预先确定实例库存，逐一校验注册与唯一 final、每挂载54 token及全部工具区间。
  分挂载/工具统计 F_GETFL、metadata、statx、fstatfs 的实际查询尝试，包含显式零和
  不合格回退，同时保留普通 OPEN/READ/FLUSH/RELEASE 与 Core 查询。分类入口和
  receipt hit/miss 标签不是额外 syscall；不从合格路径7→3推算整组耗时。共享分支
  将 DATA 资格移入既有 `open` span，该 span 两臂边界不同，只能用完整 RPC/worker
  边界核对性能。原始日志和分析在独立 `.data/sq17*/`，不混入 profile-off 时钟。
- **标准 FUSE 负项入口诊断：** 在独立 fresh 环境复用冻结的 owned-input
  组合配置（data=1、readonly FLUSH=1、WB=0、DAX=0、容量 256，两项 Core
  缺失缓存关闭），保留九操作三轮及原有校验，显式开启现有 server debug 日志。
  按同步阶段统计 LOOKUP 的成功、ENOENT、其他错误及 parent/name 重复；多连接
  unique 碰撞或缺少归属时单列歧义，不把总 LOOKUP 或物理 missing 当作重复负项。
  该 N1 只决定标准 nodeid=0/entry_timeout 候选是否值得实验，日志耗时不进入性能表，
  不改变 TTL 或所有权声明，原始日志与分析保存在独立 `.data/`。
- **真实 guest 一致性补充：** `guest_metadata_case` / `guest_metadata_guest` 的
  `review-pages` 比较旧 lower fd 与 copy-up 后 upper fd 在整页写后的内容；
  `review-attrs` 比较自然 stat、强制同步属性和旧 fd，覆盖属性变更、硬链接、替换及重建。
  `atime` 按请求 mask 区分首次 host READ 和同 fd 缓存读后的 GETATTR。各模式独立 fresh
  VM，保留每个 case 与失败；仅诊断语义和请求机制，不能作为工具耗时或全面正确性证明。
- **只读 FLUSH 诊断：** `filesystem_cache_capacity.py` 的独立 profiles 模式，
  同新 host/固件切换 `PVISOR_VM_FS_READONLY_FLUSH_ELISION=0|1`，固定 WB=0、DAX=0、
  容量 256 与 lower/upper 负项缓存关闭。每臂 N1、九操作三轮，同步 token 包围原有
  `checked()` 及校验；逐设备 overlay 的 `readonly_flush_elision_enabled` 必须显式为
  0/1，两项 Core 开关必须显式为 0，并检查全部实例final及54个token。保留原始
  FLUSH/OPEN/READ/RELEASE，不能把诊断时间当工具性能或跨组件相加精确归因。
- **Kernel-cache 诊断入口：** `kernel_cache_runner.py --profiles`；每个 count case 使用独立 fresh mount，记录 warm priming 与操作的完整请求总数，另测 prime-only 以分离 warm 增量。源码推导 expected instances（包括 notifier 是否创建 profile），核查全部 PID/component/instance 的唯一 final record，不能固定忽略额外线程实例；任何缺失/额外实例或失败都拒绝。诊断耗时绝不进入正式 distribution。
- **实验设计：** 独立诊断批次，直通 FUSE 仅为不含暂存语义的下限；插桩计时与正式性能采样分开。`immutable_lower_cache.py --profiles` 保留每个 PID/component/instance 的所有累计 records，以各实例 final record 比较 physical parent/leaf stats 和 cache hits/misses/evictions；inclusive spans 不相加。真实 mount 若被拒绝，保留失败并仅以明确标记的 core 机制实验补充，不替代 FUSE 结果。
- **入口脚本：** `filesystem_fuse_ab.py`、`filesystem_stage_ab.py`、`filesystem_kernel_probe.py`、`filesystem_exec_probe.py`、`filesystem_counters.py`、`filesystem_diagnostic.py`、`immutable_lower_cache.py --profiles`。`filesystem_stage_durability.py --profiles` 也服务此诊断条目；不开启 profile 时服务 B-FS-ENG。`filesystem_exec_probe.py` 在新 VM 中比较相同可执行文件、loader 和全部动态库从 virtio-fs 与匿名 RAM 执行；两组都先复制并校验所有输入、传递相同 fd，避免将准备成本混入 exec。准备来源分为原始文件和独立副本：前者预热原 inode，后者从工作区独立 inode 读入相同字节，保留原工具文件首次映射的机会；共同的 Python 准备仍会预热解释器及部分共享库，不能称为完全冷启动。首个 exec 与后续重复 exec 分开，且不由热路径的零差异否定首次映射成本。guest `/dev/shm` 保持 noexec，使用显式 executable memfd；不重挂载或放宽策略。独立 VM 的完整计数器包含相同准备与指定次数 exec，差异用于请求归因，不能当作完整 Agent 启动水位。

### B-STARTUP-ENG：初始化实现的工程 A/B {#b-startup-eng}

- **角色：** engineering A/B
- **Motivation：** 判断 guest init 实现变化是否改变启动等待。
- **想要的结论：** 同输入就绪中位数差异及 95% bootstrap 区间。
- **实验设计：** 相同 runner、rootfs 与 payload，随机交错实现，不作为跨产品用户对照。
- **入口脚本：** `guest_init.py`。

### B-STARTUP-REVISION-ENG：提交版本的启动与收尾对照 {#b-startup-revision-eng}

- **角色：** engineering A/B；结果独立保存在 `benchmark.new/startup/.data/`，不更新用户启动水位。
- **Motivation：** 定位指定提交与父提交或已归档历史版本是否改变首条有效输出和正常退出等待。
- **想要的结论：** host、staged、VM 的 Ready / Exit 统计及同轮中位数差的 95% 配对 bootstrap 区间；不能用跨批次数字归因。
- **实验设计：** 精确源码归档、相同 release/musl/gateway 构建参数和冻结内核；相同 rootfs、探针、CPU 0,1、2 vCPU / 128 MiB、热缓存。各条件 3 次预热、60 对正式样本，固定 seed 随机交替两个版本；Ready / Exit 同次采样。版本在独立 user/mount/PID namespace 中运行，私有 Host 状态目录，退出 namespace 时销毁自有服务；不重启宿主已有 Host。复用当前启动 observer 与完整正确性门禁，保留全部有效慢样本，无事后剔除。
- **入口：** 每次生成并冻结的 `.data/revision-<commit>/bench.py`，由同目录 `in.json` 经 `semspec bench` 执行；构建收据、轮次顺序、逐次输出与统计一起归档。
- **历史差距复核：** 先以同一 observer、探针和私有环境随机配对历史原始二进制与指定提交，明确 GNU 动态 / musl 静态等构建差异；此对照只能判断制品差距。确认差距后，以同一固件和构建目标重建源码版本，再判断源码贡献。原始固件提取出的内核摘要必须与冻结内核逐字节核对，不能仅根据链接方式断言固件不同。
- **构建控制：** 历史 GNU、指定提交 GNU、指定提交 musl 可在同轮三条件随机顺序中采样，各 3 次预热 / 60 次正式试次，固定同一历史内核；分别报告 GNU 源码对照及指定提交 GNU/musl 路径对照的配对区间。内核差异另用同源码、同 musl 参数的两条件对照，不能从多个批次差值相减估算贡献。

### B-KERNEL-ENG：裁剪固件是否缩短当前产品的启动 {#b-kernel-eng}

- **角色：** engineering A/B；放入 `docs/src/*/design/vm-startup-performance-analysis.md`，不作为跨产品用户排名。
- **Motivation：** 判断裁剪 guest 内核的收益，以及它是否仍支持工作区、开发工具和网络所需能力。
- **想要的结论：** 相同 pVisor 二进制、相同 Linux 源码和补丁、相同打包方式，仅配置不同的两份新固件，其就绪/完整任务中位数差及 95% 配对 bootstrap 区间；列出减少的功能与未验证能力。
- **实验设计：** 冻结固件源码、输入 tarball、补丁、配置与编译记录，分别重建通用配置和裁剪配置；不复用旧固件作为基线。先验证构建和同样负载的输出、暂存隔离与工作区内容；Linux KVM 上以相同两核/内存/工具输入，随机交替 shell-ready、七项文件工具和修复任务，每格 30 次、3 次预热。CPU 缓解、seccomp、namespaces、virtio-fs 等所需配置必须保留；网络/恢复能力需独立正确性检查，不能由 shell 启动通过推导。配置中 guest LSM/设备功能差异必须明确，不称为相同加固程度的排名。
- **入口脚本：** `kernel_comparison.py`；`prepare_firmware_comparison.py` 是离线构建和来源准备 helper。

### B-STARTUP-DIAG：固件启动阶段诊断 {#b-startup-diag}

- **角色：** diagnostic
- **Motivation：** 定位内核、初始化和宿主启动/收尾阶段对就绪时间及正常退出的贡献。
- **想要的结论：** 各阶段计时及计时边界，不作为用户启动水位。
- **实验设计：** 固定 guest payload 与 runner；早期内核时钟和收尾时间分别解释。
- **入口脚本：** `firmware_boot.py`。
- **版本退化诊断：** 冻结历史与指定版本制品，在启动工程对照的私有环境下启用 `PVISOR_STARTUP_TIMING=1` / `PVISOR_STARTUP_PROFILE=1`，各做少量独立试次，保留 stderr 阶段记录。诊断行命令与输出独立归档；插桩时间不发布为性能水位，不将重叠 span 相加，缺失阶段标记视为未知。

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

- **文档：** `supervision-cost.md#apply-cost`
- **角色：** user-facing
- **Motivation：** 暂存改动的价值要到合入时才兑现。用户需要知道合入的耗时如何随文件数增长，以及宿主上的并行修改会不会被覆盖。
- **想要的结论：** "合入 N 个文件需要多少时间，在多大规模以内适合交互式使用"；"宿主并行修改一定会被检测为冲突，不会被静默覆盖"；"合入中途被中断后可以恢复，或者状态明确"。
- **实验设计：**
  - 文件数按 10 到 10 万的数量级扫描，并与同批的 Git patch 合入做对照。
  - 冲突注入：在合入前或合入过程中修改宿主文件，检查是否全部被检测到。
  - 中断注入：在合入的各阶段 SIGKILL，检查重新执行后的最终状态。
- **入口脚本：** `v1/apply.py`。

### B-APPLY-ENG：合入目录索引的工程 A/B {#b-apply-eng}

- **角色：** engineering A/B，不进入用户页正文。
- **Motivation：** 判断目录依赖闭包索引是否减少大批合入的等待，同时保持冲突检测和完整结果。
- **想要的结论：** 同机配对的合入/冲突拒绝中位差及 95% 区间；只有输出和冲突保护全部通过后才比较。
- **实验设计：** 同一冻结父源码，仅修改 apply 实现；编译器、依赖、release 配置一致。默认 1,000/10,000 文件、合入与合入前冲突、每格 30 次和三次预热，随机交替新旧二进制；每次创建独立 stage/target，准备排除在合入计时之外。诊断计数另跑，不把插桩时间混入性能结论。
- **入口脚本：** `apply_plan_ab.py`。

### B-APPLY-DIAG：合入计划的计数分解 {#b-apply-diag}

- **角色：** diagnostic。
- **Motivation：** 判断时间是否花在收集改动、硬链接分组、目录索引或依赖闭包上。
- **想要的结论：** 完整的 plan inclusive span、改动数、目录数、闭包迭代和祖先查询数；嵌套项不可相加。
- **实验设计：** `apply_plan_ab.py --profile` 独立插桩批次，保存所有 stderr 计数；与正式计时分开。旧实现没有某项计数时明确为缺失，不当作零。可同时指定冻结的 `--trace-syscalls` 和 `--tracer-receipt`，只对合入/冲突命令运行 strace，保留源码、编译器、tracer 二进制和逐进程原始记录；分别统计 target、stage 和其他路径的元数据、复制与同步请求。tracing 耗时不能作为正式性能收益，syscall 用时不能与嵌套 span 相加。
- **入口脚本：** `apply_plan_ab.py --profile`。

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
- **入口脚本：** `density.py`，worker 为 `density_worker.py`。每批使用独立、固定 CPU/内存预算且禁用 swap 的 cgroup；所有任务到达 readiness barrier 后才一起释放。空闲与 Python/Git 修改和校验任务分开报告。OCI 对照必须证明 payload 与 conmon 没有逃离资源预算。记录整个 cgroup、完整 VM backing、RSS、OOM、尝试与完成数量。共享的预先准备工具/镜像缓存可能由父 cgroup 计费，不能把受限 cgroup 的容量换算为整机净内存节省。
- **暂停等待的容量另测：** `parked_density.py` 使用相同静态 worker（`parked_memory_probe.rs`）、64 MiB 完整触碰和校验的数据、64 个文件与四个恢复后改动；重复与确定性随机数据分开。逐个创建并暂停到共同 parked barrier，比较当前 Job raw/压缩执行快照、native SIGSTOP 和 Podman pause；统一包含协调器、辅助进程、backing/页缓存的两核、2 GiB、零 swap 预算。随后以固定一个恢复槽逐个恢复，校验同一执行 token、全部内存和文件结果；分别报告停驻容量、准备成本、恢复至完整结果的成本、失败/未知/OOM，不能称为活跃并发或用快照文件大小推算物理密度。先单独验证四类机制的 stdin barrier 可保存/恢复；不支持的机制明确为未测，不改用计时 sleep。OCI payload/conmon 与 pause 使用的子 cgroup 必须都处于同一总预算。完整容量扫描每格至少五个批次；共享预备工具缓存的计费范围和快照相对容器 pause 的语义差异需明确。SDK offload 与自动冷页压缩不包含在此快照对照中。

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

### B-WORKFLOW：保留少量改动的完整任务流程是否更便宜 {#b-workflow}

- **文档：** `supervision-cost.md`
- **角色：** user-facing
- **Motivation：** Agent 常在大工作区中修改少量文件。用户要付出创建独立工作区、运行、查看 diff、选择性合入和丢弃其余结果的总成本，而不只是工具执行时间。
- **想要的结论：** 同机、同工作区规模下，pVisor stage 与 Git worktree、文件系统 reflink 副本的完整机器流程成本，以及并发宿主修改能否阻止合入。
- **实验设计：** 预先准备相同 Git 仓库；每次创建独立任务视图，修改 20 个文件，生成内容 diff，合入 10 个并丢弃其他改动。分别测小工作区和 10,000 文件工作区，普通合入和宿主冲突独立成组；随机交替执行，至少 30 个样本、3 次预热。验证任务执行不修改原工作区、选择性结果一致、冲突时没有任何文件被覆盖。计时包括任务视图创建到清理，排除相同起始仓库的实验夹具准备与机器校验；不测推理、人类阅读、容器或 VM 隔离。
- **入口脚本：** `review_workflow.py`。

### B-REPLAY：轨迹回放能否忠实准备环境 {#b-replay}

- **文档：** `replay-fidelity.md`
- **角色：** user-facing（正确性与成本）
- **Motivation：** 训练和复现需要把历史轨迹准确还原到同一个起点。用户需要知道支持哪些轨迹格式、准备是否有副作用、成本是多少。
- **想要的结论：** "支持的格式全部通过一致性校验，准备阶段不执行工具也不修改工作区，每条轨迹的准备耗时为 X ms"。
- **实验设计：** 对每个适配器使用固定的原生格式样本，校验前缀结构、工具参数和观测内容，同时校验工作区没有被修改。
- **入口脚本：** `benchmark.new/replay/replay.json`（`prepare.py` 冻结和预检，semspec bench 直接测量，hooks 逐次验证，`docs.py publish` 更新 JSON 表格）。每个适配器一条固定轨迹的 hyperfine 数据为独立批次；历史二十条轨迹、三次重复的完整一致性证据保留 `v1/replay.py` 的原口径。

### B-VM-MEMORY：VM 闲置时能省多少内存，恢复要付出什么 {#b-vm-memory}

- **文档：** `vm-memory/index.md`
- **角色：** user-facing
- **Motivation：** 长期挂起的 Agent 环境占用内存。用户需要知道冷页回收、offload 和快照能省多少内存，以及恢复访问时要等多久。
- **想要的结论：** "闲置 VM 的驻留内存可以降低 X%，以整机物理内存衡量；恢复后首次访问会增加 Y ms"；以及与 Docker 或其他 VM 相比是否真的更省。
- **实验设计：**
  - 用整机或 cgroup 的物理内存作为主要指标，进程 footprint 只作参考。
  - 用户选型按默认空闲页回收、运行中压缩、闲置卸载和跨实例共享组织。默认回收测工作集释放/再次分配；压缩测可压缩、随机和读热点混合内容；卸载比较继续运行、仅暂停、raw 和压缩 backing；共享测 1/2/4 实例与写入后的退化。开启命令与内存、CPU、峰值和下一次完整工具任务等待一起报告。
  - 同时测回收量、恢复延迟和 CPU 开销。
  - 历史用户测量以 512 MiB 配置的 VM 为基准（旧表与文本已归档，当前页面展示单独标注的去重预检曲线）：主表使用完整产品进程 PSS 之和描述实际常驻物理内存，共享页按比例计入，包含压缩池和辅助进程；明确它不包含未映射文件缓存及内核内存，整组 cgroup 内存与缓存另外报告。不能把 PSS 当作完整宿主成本，也不能把配置容量当作实际占用。
  - 先提供短窗口静态读数：单实例每格一个新 VM、零预热、5 秒观察；跨实例先测四实例、每格一个新组、零预热、2 秒扫描窗。启动前一次检查，采样期间持续监测；后台构建如实记录，只允许解释静态内存，不用于速度比较；其他 VM 干扰、预算或校验失败仍拒绝。静态读数只回答指定窗口的占用及观测到的恢复成本，不报告 P50/P95、置信区间、长期稳定性或密度。完整多轮设计是独立后续批次，不能将短窗口读数混入。
  - 跨实例选型另设 `memory_sharing.py --static --strategies`：原始不共享与 KSM 均为四台独立启动的私有匿名 RAM VM，原始组关闭 advice（SDK `with_private_ram()`）、共享快照＋COW 从一份 sealed RAM 恢复、KSM 在相同 RAM 映射上开启 RAM advice；三者同为 512 MiB/2 vCPU、64 MiB 工作集、四实例，ready/25%/100% 写入逐阶段对照。每格 N=1、默认扫描 2 秒，比例以相同阶段原始不共享组为分母；不得将旧独立快照副本或 restored-KSM 的数值充当这些新条件。传入 frozen `--pool-daemon`/`--pool-receipt` 加入 daemon 持有的内存池，四方案共 12 格，包含池进程 PSS、全组计费和内容恢复校验。Linux daemon 池扫描已驻留的 4 KiB 页，优先收纳跨 VM 重复候选，物理页由 daemon memfd 持有；读取保持共享、写入 COW，扫描后释放已改写页的旧引用。独有候选只保留有界摘要，不提前复制页；不需要 userfaultfd，也不压缩独有页。保留稀疏 RAM；为了与既有条件保持一致，四方案统一 `--group-memory-max 4294967296`；上限不作节约分母。`--ksm-scan-seconds 60` 仅延长原始 KSM 组到 60 秒，其余保持静态 2 秒窗，完整 12 格重新测量并保留逐条件窗口；KSM 每台 VM 必须有正的 advice 接受字节，ready smaps 必须有四个 mergeable VM 进程，不能将全部 SharedMapping 跳过当作有效 KSM 结果。单方案 `--preflight --arm/--pattern` 仅诊断，不得发布部分 cohort。完整 smaps 使用流式写入且逐 MiB 同步／缓存丢弃的独立证据文件，测量组内只保留汇总与文件引用；进程 PSS 使用 smaps_rollup，避免逐 VMA 千字节取整；逐映射汇总另行保留。发布时重验字节、SHA-256、进程／逐映射汇总及 KSM flags。避免大量页映射的原始文本驻留在测量进程中造成假占用；保留证据，不将此前内存驻留 smaps 的读数混入本轮。
  - 区分重复数据和随机数据负载。
  - 每次试验使用新的 VM，并校验数据完整性。
  - `memory_savings.py` / `vm_memory_savings` 提供当前 Linux 默认回收、live 压缩、暂停和 raw/compressed offload 用户对照。每格先预检，再 3 次预热、30 次正式样本；完整组四核/2 GiB/零 swap，512 MiB/2 vCPU guest、64 MiB 应用数据、固定 60 秒闲置窗。逐块独立摘要、全量工具写入/读回和原生 reap 为成功门禁，观察者与协调器在组外。新制品与既有快照/offload 批次分开保留。`publish_memory_savings.py` 校验完整条件、预热与正式样本、来源和干扰门禁后导出阶段/峰值/CPU/存储分布及相对默认模式的配对 bootstrap 区间。
  - `memory_sharing.py` 使用同一 SDK worker 的独立用户协议：1/2/4 个 2 vCPU/512 MiB VM，重复、相同随机和独有随机 64 MiB 数据；同一 sealed 快照的共同 inode 与全字节复制的独立 inode 对照，动态私有页 KSM advice off/on。完整组四核/2 GiB/零 swap、扫描器状态只读、每个扫描窗 30 秒；独立 inode 不是独立启动捕获，ready 与动态改写阶段不能混作同一比较。每格 3 次预热、30 个新组，独立恢复/写入/退出校验、全组内存与 CPU 和组外干扰观察；不得将 B-MEMORY-SCALE 工程批次或建议登记字节替代为这些用户数据。
- **入口脚本：** `memory_savings.py`（当前 Linux 单实例回收、压缩、暂停和卸载）、`memory_sharing.py`（共享基线和动态 KSM 的独立用户协议）、`macos_cold_ram.py`（Apple Silicon live cold-page pool）、`vm_memory.py`（当前 Job API 的 Linux raw/compressed execution suspend/resume）、`live_vm_memory.py`（当前 SDK 的 Linux whole-VM offload，使用 `vm_live_memory_bench` example）。Linux 使用独立受限 cgroup，包含 backing/cache、捕获与恢复进程；报告 active、suspended 和恢复阶段、数据完整性与 CPU 成本。SDK offload 每个样本创建新的 VM，重复数据与确定性随机数据、raw 与压缩 backing 随机配对；记录完整 cgroup 的 anon/file/kernel 与 CPU，校验恢复后的全部数据和可变状态。执行快照、whole-VM offload 与运行中自动冷页压缩分别报告；单 VM 回收量不能替代密度实验，不恢复退役的独立 snapshot CLI。

### B-MEMORY-SCALE：最多四个 VM 的共享、写入退化与卸载恢复 {#b-memory-scale}

- **角色：** engineering A/B；实验计划与报告留在 `benchmark/pvisor/`，不填充用户 benchmark 数字。
- **Motivation：** 验证内存去重设计在多个独立 runner 中是否保持同一基线、写入隔离和独立退出；区分基线共享、动态 KSM 合并与单实例 offload 的收益及成本。
- **想要的结论：** 1/2/4 个 VM 的完整 cgroup 内存、每 runner PSS/KSM 和 advice 状态，0/25/100% COW 写入退化曲线、退出后剩余实例的完整性、raw/compressed 卸载后的回收与读回；不将登记字节当作节省，不外推超过四个 VM 的密度。
- **实验设计：** 同源恢复的共同 inode与独立 inode 对照，KSM advice off/on 随机配对；重复、实例独有随机、相同随机内容分别测量。每条件创建新 VM，256 MiB RAM/1 vCPU，完整组统一四核/2 GiB/零 swap 预算，所有 VM barrier-ready 后按阶段采样。基线生产者退出后才启动最多四个恢复实例；每份 64 MiB payload 逐字节/摘要校验，固定写入比例、递增状态和退出顺序。raw/compressed whole-VM offload 使用独立 fresh-live 分组；私有 COW offload 拒绝、KSM 与冷回收互斥作为正确性门禁。读取完整 cgroup anon/file/kernel、CPU/peak/PSI/OOM 与所有 runner smaps，保留来源收据、输入清单、完整失败。预检一轮与正式至少五轮分目录；未运行条件标未测，不报告小样本尾延迟。禁止实例改变全局 KSM；扫描开启的实验仅在管理员预先配置的受控宿主执行，记录预算与扫描时间窗，deadline 到达不是产品失败。
- **Firecracker 对照扩展：** `firecracker_ksm.py` 使用相同页面生成器、64 MiB payload、四 VM 和组预算，测量独立 fresh boot 的 RAM-VMA PSS/KSM、20 秒窗口及 25/100% 写入完整性。仅使用安装版本支持的去重接口；没有接口的 advice-on 标为 unsupported，不注入建议、不把 scanner 开启当作 guest RAM 可合并。跨内核、backing、启动方式的批次只并列展示，不作纯 VMM 排名；最小 PID-1 worker 的组计费不能直接与 SDK checkpoint/runtime 组计费比较。
- **自适应扫描对照扩展：** `benchmark.new/memory/scanner_compare.py` 比较 fresh 私有 RAM 关闭去重、宿主默认参数 KSM、pVisor UKSM-inspired daemon 物理页池；后者不是原版 UKSM 内核补丁。四个新 VM 各 256 MiB/1 vCPU/64 MiB payload，整组四核/2 GiB/零 swap；三种内容、统一 60 秒 ready 后扫描窗、25/100% 写入完整性。预检每格一次，正式至少五轮随机交替，不拼接历史结果。保留整组 PSS、cgroup 内存/CPU、daemon、KSM advice/实际合并、全局 ksmd CPU 独立计数及完整原始证据；全局 CPU 不能严格归因到本组，固定窗口不代表收敛。KSM 必须由管理员预配置，协调器不写 sysfs。所有失败保留并停止批次；已有完整性/资源/外部 VM 与构建干扰门禁适用。报告中位数、范围和配对 bootstrap 95% CI，不作尾延迟或纯扫描算法归因。可选线性扫描制品必须只替换物理页候选调度，并冻结差异后独立登记对照条件。
- **Firecracker 与池化曲线扩展：** `benchmark.new/memory/pool_compare.py` 使用相同静态 guest worker、64 MiB payload 和逐字节/独立 SHA-256 oracle，主基线为原版 Firecracker fresh boot，辅助控制为 pVisor off/KSM/物理池/sealed 压缩池/实例内压缩。两者各四个 256 MiB/1 vCPU VM、整组四核/2 GiB/零 swap，阻塞等待控制输入，不引入 Python payload 或高频文件心跳。60 秒 ready 后窗口，外部每 50 ms 记录完整组计费曲线，每 500 ms 记录进程 PSS；包括 daemon，记录独立全局 ksmd CPU。保存 0/1/2/5/10/20/40/60 秒边界、首次全量读回、25/100% 私有写入、peer/退出完整性、实际 RAM advice/共享映射证据。启动期间可开始扫描，曲线须保留启动到 all-ready 的时间关系，不称同步扫描起点。不同内核及 virtio-blk/virtio-fs、guest supervisor 差异显式披露；原版 FC 未提供合并开关时只运行其真实默认，不注入建议，也不标成 FC+KSM。各机制同时改变块大小/缺页恢复，不能把全部差异称为纯 codec 成本；codec 与哈希/IPC/映射归因须独立诊断。预检 n=1 与正式至少五轮随机交替分目录，失败停止并保留；只读宿主配置，不改变 KSM。源码、输入、制品、命令及完整原始曲线保存在 `.data/`，工程结论不进入用户 benchmark 正文。
- **写后释放窗口：** 曲线协议 v3 在 100% 私有写入及 peer 校验后，保留四 VM 30 秒，另存 1/2/5/10/20/30 秒计费、PSS、内核分类与 CPU；随后完成原生退出及 survivor 校验。区分即时双份驻留、旧槽释放延迟和稳定管理开销。v2 没有此窗口的批次只保留其原口径，不拼入 v3。原 60 秒闲置窗、58 项完整内容与生命周期校验、四核/2 GiB/零 swap 和每条件 180 秒资源期限不变。正式汇总重验完整矩阵、至少五个随机轮次、来源/输入/观察者摘要和完整写后窗口，未达到的回收阈值保持删失，不填作零或 60 秒。
- **Docker 对照扩展：** 独立 rootless Docker daemon 与四容器共用一个受限 systemd slice，将 dockerd、containerd、shim、连接客户端及应用完整纳入四核/2 GiB/零 swap；每容器 256 MiB/一核配额。协议 v4 为所有条件统一设置 256 任务上限，并核对实际上限和任务耗尽事件；旧 v2/v3 保持原 128 任务校验，不混入新正式批次。`prepare_docker.py` 使用冻结的同一静态 worker、Python userspace 和 `.so`，每组独立镜像存储副本。镜像导入在测量前，daemon 启动包含在启动曲线。宿主未委派 cpuset 时由静态 launcher 在 payload 分配前设置亲和性，并核对实际 PID/亲和性、容器与完整组限制、递归 PSS、原生退出及同一完整性 oracle。容器共享宿主内核，rootless 与独立 daemon 的成本显式标注；应用 PSS 和完整组计费同时报告。不得漏掉子 scope、以 `docker stats` 替代全组核算，或将此配置外推为所有 Docker 部署。只操作专属 daemon/socket/镜像，不操作现有宿主容器。Docker 静态与共享库完整预检已通过，每条件 N=1；正式随机轮次未完成，单次预检不能标为正式结果。
- **文件读取驱动共享库：** `benchmark.new/memory/library_compare.py` 独立批次使用同源 Python userspace 和实际 `dlopen` 的 64 MiB 唯一随机页 `.so`。七个条件为原版 Firecracker virtio-blk、独立 rootless Docker、pVisor 普通 rootfs off/KSM、同二进制物理池文件 READ 提示关闭/开启、显式不可变 DAX 参考。后者不是自动 rootfs 共享。沿用四 VM、资源/干扰/58 项完整性门禁与 60 秒闲置、30 秒写后窗口，正式至少五轮预定随机顺序；不与静态 worker 矩阵合并。空闲计时后由组外观察者核对库页的完整字节、backing inode/offset 及前后 present-file PTE，仅描述观察到的跨 VM 同槽共享，不以此代替完整组净内存。记录 userspace 文件清单及 ext4 字节、源库前后摘要、实际 DAX 属性和全局 ksmd；物理池未共享目标页是有效的零收益，不按速度剔除。额外观察阶段单列，不计入首次读回和 60 秒 CPU。普通 mutable lower 的 READ 仅提供候选顺序，不提供不可变性或合并授权。
- **入口：** `memory_scale.py` 调用 SDK `vm_memory_scale` example；实验方案见 `benchmark/pvisor/MEMORY_SCALE_PLAN.md`。Firecracker 工程对照入口 `firecracker_ksm.py`，报告见 `benchmark/pvisor/FIRECRACKER_KSM_REPORT.md`。

### B-MEMORY-DIAG：去重登记、COW 与文件回收的机制验证 {#b-memory-diag}

- **角色：** diagnostic；结果不进入用户 benchmark 正文。
- **Motivation：** 在没有 KVM/FUSE 或没有启用 KSM 扫描的宿主上，区分区域建议登记、不可变基线共享、私有写入隔离和原始 backing 回收，避免把登记字节或进程 RSS 当成实际去重收益。
- **想要的结论：** 给出当前宿主上 advice 安装状态、每映射 smaps 的 PSS/KSM 字节、COW 完整性、原始文件回收后的驻留与读回校验；缺少真实 VM 或扫描器时明确记为未测。
- **实验设计：** 三次独立 64 MiB 映射试验；同 inode 双私有基线、相同内容双匿名映射和单共享磁盘文件分别测量。固定非零重复页、完整字节校验；记录内核、文件系统、KSM 全局状态、来源摘要及命令。禁止改变宿主全局 KSM 设置。smaps PSS 仅描述选定映射，不代表整机净节省；不报告尾延迟或生产密度。
- **入口：** `crates/pvisor-vm/src/ram_dedup.rs` 中 Linux ignored `memory_diagnostic` 测试，使用显式 `cargo test --ignored --nocapture` 特殊诊断 runner。原始数据保留在 `benchmark/pvisor/.data/`；复现命令与机制报告见该目录的 README。

### B-COLD-RUNTIME-ENG：Linux 实例内冷压缩的真实回收与恢复 {#b-cold-runtime-eng}

- **角色：** engineering A/B；报告留在 `benchmark/pvisor/`，不发布生产密度结论。
- **Motivation：** 验证 Linux 内核缺页恢复、自动回收和实例内压缩存储的组合能否安全降低运行 VM 的宿主占用。
- **想要的结论：** 明确实例内 cold off/on 的完整受限 cgroup 内存、RAM PSS、压缩 store/临时峰值、冷页和恢复计数，以及全部 payload 摘要、mutation、设备 I/O、退出；原始随机内容是否拒绝无益回收。
- **实验设计：** 初始 1 VM 新实例 off/on、256 MiB/1vCPU、64 MiB 重复/独有随机数据，每格固定两段冷窗口和两次全量恢复；组四核/2GiB/零 swap，最多四 VM，无全局 KSM/sysctl 修改。kernel-fault userfaultfd 权限由用户授权。每条件独立新 cgroup，固定等待和 deadline，完整保留失败，不以 codec 字节当作净内存节省。初始预检每格 n=1，计时含 debug/校验边界，先正确性后扩展。
- **入口：** `crates/pvisor/examples/vm_cold_runtime.rs` 与 `benchmark/pvisor/linux_cold_runtime.py`；报告 `LINUX_COLD_RUNTIME_REPORT.md`；同源冻结 GNU release 预检与加工 CSV 见 `benchmark/pvisor/COLD_RUNTIME_RELEASE_REPORT.md`，独立成批，不与既有 cohort 合并。

### B-VCPU-IDLE-ENG：真实 guest 的 vCPU 等待窗口与观测开销 {#b-vcpu-idle-eng}

- **角色**：engineering A/B；EXP-001 M0 observe-only，不进入用户 benchmark 正文。
- **入口**：`benchmark/pvisor/vcpu_idle.py`，真实 VM SDK example `vm_vcpu_observe`；协议 `benchmark/pvisor/vcpu_idle_plan.md`。
- **Motivation**：决定当前后端观测是否足以发现真实 guest 等待机会，以及开启采集与有界采样的成本是否值得继续研究。
- **想要的结论**：按后端/负载报告窗口、Unknown 与拒绝原因，observer off/on 的同批配对 wall/CPU 差异及 bootstrap 95% CI；无法证明的能力明确未测。
- **实验设计**：同机同制品，fresh VM，sleep/busy/短 timer，1/2 CPU 与 SMP 单 CPU busy 负对照；预先保存 seeded 随机配对顺序，完整输出校验、有界采样、超时、来源及失败保留。KVM_RUN 是 Unknown；HVF WaitingForEvent 不证明 Linux runqueue 空闲。无 pause/offload、全局 sysctl 修改或自动策略；卸载收益未测，M1/M2 未实现。

### B-COLD-STORAGE-DIAG：冷压缩存储的空间与完整性边界 {#b-cold-storage-diag}

- **角色：** diagnostic；不进入用户 benchmark 正文。
- **Motivation：** 当本机没有 live pager 或实例内后端时，验证可运行的编码存储是否保持全部内容，并区分 encoded payload 缩减与真实 VM 驻留收益。
- **想要的结论：** 64 MiB fill、非填充可压缩和确定性随机数据的编码字节、对象数、两次完整读回及编码/恢复墙钟成本；真实自动回收和实例内 pager 明确标 unsupported/not implemented，不以存储测试替代。
- **实验设计：** 新进程、新的实例独占 CompressedPool，每块 64 KiB，三次独立执行；填充模式允许对象复用，其他模式保持块内容唯一以分开压缩与去重。全字节检查，拒绝校验失败，记录原文/编码 SHA-256 身份、来源/制品摘要和完整失败。编码字节排除索引、allocator、scratch 和原 RAM；不宣称净物理节省或业务延迟。无 VM、无全局 KSM 修改。
- **入口：** `crates/pvisor/examples/cold_storage_probe.rs`；运行命令与报告见 `benchmark/pvisor/README.md`、`COLD_RUNTIME_REPORT.md`。

### B-CLUSTER：增加机器和资源后，能否得到更多有效结果 {#b-cluster}

- **文档：** `cluster-scalability.md`
- **状态：** 已退役；只保留历史证据，无活动测量或发布入口。旧 Controller/Worker 数据不是当前 daemon 的吞吐、密度或历史成本。
- **角色：** user-facing（历史证据）
- **Motivation：** 规模化运行 Agent 时，用户关心的是增加 Worker 和资源能不能线性地增加有效产出，以及控制面在长期运行后是否会成为瓶颈。
- **想要的结论：** "在固定的总资源预算下，每秒完成的有效任务数随 Worker 数增长的曲线"；"控制面在保留 N 条历史记录时的内存和重启耗时"。
- **实验设计：**
  - 固定包含 Controller 和全部 Worker 的总 CPU/内存预算，另保留每个 Worker 的资源上限，扫描 Worker 数量。
  - 以完成且通过校验的任务数作为产出指标。
  - 控制面的历史规模单独扫描。
  - 不能只报告就绪时间来代替吞吐。
- **入口脚本：** 无。旧 `cluster_scalability.py`、`cluster_worker.py`、`controller_history.py`、专属测试及绘图/发布 helper 随 Controller/Worker 退役而删除；不得改名为 daemon 测量。历史数据只回答冻结旧制品的问题。B-DENSITY 与 B-VM-MEMORY 的独立本机探针继续有效，但不能替代 daemon 或跨主机实验。

### B-MACOS：macOS 上的工具和迁移成本 {#b-macos}

- **文档：** 并入 `filesystem.md` 和 `startup.md` 的 macOS 小节
- **角色：** user-facing
- **Motivation：** macOS 用户的对照对象通常是 Docker Desktop。他们需要知道在 Apple Silicon 上，pVisor VM 的工具执行和启动相对 Docker 如何。
- **想要的结论：** "在 Apple Silicon 上，pVisor VM 与 Docker Desktop 在相同工具负载下的耗时对比"。
- **实验设计：** 同机随机交替执行，使用相同的 Alpine 环境和工具负载，分别计时 worker 耗时和完整任务耗时。
- **入口脚本：** `macos_docker_tools.py`。迁移版本 A/B 另属 B-MACOS-ENG。

### B-MACOS-ENG：macOS CLI 迁移的工程对照 {#b-macos-eng}

- **文档：** 技术分析与工程报告，不进入用户页正文。
- **角色：** engineering A/B
- **Motivation：** 开发者需要防止 CLI 迁移导致启动、文件工具或冷页恢复回归。
- **想要的结论：** 同机两个冻结版本的配对中位数差异与 95% 置信区间。
- **实验设计：** 相同 rootfs、firmware 与预算，随机交替、检查输出与隔离；诊断另列。
- **入口脚本：** `macos_migration.py`。

### B-COMPARE：端到端任务与强化学习训练的场景分析 {#b-compare}

- **文档：** `agent-tasks.md`（整合 B-AGENT-TASK 的实测与任务场景分析）、`compare-rl-infra.md`（强化学习训练）；隔离和回放证据分别由 B-ISOLATION、B-REPLAY 维护。
- **角色：** user-facing（综合页）
- **Motivation：** 用户需要按完整任务或 rollout 流程选择执行层，判断现有 Agent 沙箱、容器、VM、Git 工作区和训练系统是否需要改变。
- **想要的结论：** pVisor 在两类场景中的实测优势、额外成本、兼容性失败和选型边界；每条定量结论链接到 owning user-facing benchmark 的数据与条件。
- **实验设计：** 场景分析不做新测量，复用启动、惰性镜像、文件系统、审查合入、容量、隔离、回放等独立实验，不合并批次或相加中位数。原生 Agent 沙箱、Docker/devcontainer、云端和隔离基座的分析整合进端到端任务；OpenHands/SWE-Gym/verl 的职责与限制整合进强化学习训练。没有同条件数据的方案只说明适用需求和缺失验证，不给数值排名、不引用宣传数字。未执行完整 RL 训练时，不推导有效 rollout 吞吐、reward、任务成功率或账单收益。

### B-PROCESS：Run 本身的进程级开销（CI 回归门禁） {#b-process}

- **文档：** 无用户文档；结果用于 CI 和 PR
- **角色：** engineering A/B
- **Motivation：** 防止 Run 的启动和 Bundle 读取在日常开发中出现回归。
- **想要的结论：** "候选版本相对主干，最小 Run 的耗时和 status 读取耗时没有超过阈值的回归"。
- **实验设计：** 同机同套件，smoke 使用 2 次预热、10 个样本，nightly 使用 10 次预热、50 个样本；默认 15% 阈值。
- **入口脚本：** `benchmark.new/process/smoke.json`、`nightly.json`（`prepare.py` 准备，semspec bench 测量，`just benchmark`）；旧 `bench.py` 仅保留历史复现与统计辅助函数。

## 检查清单 {#checklist}

提交 benchmark 相关改动前逐项确认：

- [ ] 已阅读本文件和路径上每一级目录的 README。
- [ ] 改动对应的注册表条目存在，脚本注释块与条目一致。
- [ ] 用户文档页严格按"主要结论 → Motivation → 实验设计 → 实验数据和分析"四节组织。
- [ ] 主要结论的第一句直接回答标题的问题，所有数字都能在本页的表或图中找到。
- [ ] 结论以 pVisor 的技术选择、用户价值与适用条件为主线；对照身份明确，预期与实测区分，不利结果完整保留。
- [ ] 没有过程叙述、版本迭代史和内部代号；工程 A/B 和诊断结果没有进入用户页正文。
- [ ] 百分比变化来自同批对照，并附有置信区间；双峰分布与受干扰样本已按[规则](#data-rules)处理。
- [ ] 所有计入样本都通过了正确性和隔离校验，失败已单独计数。
- [ ] 中英文两版同步更新；加工表格、必要公开 CSV 与构建所需 JSON 已加入版本控制，完整统计、原始样本与制品清单位于忽略的 `.data/`。

## 目录

- [`pvisor/`](pvisor/README.md)：测量脚本、运行方式与报告格式。
- [`replay/`](replay/qwen3.6-results.md)：Qwen3.6 SandboxReplay 实验记录，保留历史样本，不作为产品保证。
- 用户文档：[`docs/src/zh/benchmarks/`](../docs/src/zh/benchmarks/index.md)。
- 技术分析：`docs/src/zh/design/*-performance-analysis.md`。
