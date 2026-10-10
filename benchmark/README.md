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
7. **不在文档中讲过程。** 不写"先做了 v1，又改成 v2"、"我们发现"、"本轮"之类的叙述，不使用内部代号。过程和历史留在证据目录与 相关目录的 `.data/` 下。

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
  - 末尾提供同目录整理后的 CSV 下载和来源摘要，复现命令链接到 `benchmark/` 的 README。原始样本与制品清单留在本地 `.data/`，不生成失效的站点下载链接。

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
- Markdown 保存读者需要的加工表格与结论。需要支撑性下载时，同目录放整理后的 CSV，并在 Markdown 链接；CSV 必须保留批次、配置、样本数和统计口径。
- 不再把 typed TSV 完整报告、原始 samples CSV、压缩证据包或源码快照发布到 `docs/src/assets/benchmarks/`。保留本地数据与加工结果之间的摘要关联。
- 发布脚本的输入和原始归档放 `.data/`，只有加工 CSV 与图表进入公开目录。站点构建和搜索也排除 `.data/`。
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
- **入口脚本：** `reference_baselines.py --modes ready`（`firecracker_kernels.py` 为独立制品准备 helper）、`startup.py`、`linux_vm_ready.py`、`vm_ready.py`、`run_all.py`、`ubuntu_baselines.py`；启动工程实验与诊断见下列独立条目。

### B-LAZY-STARTUP：远程镜像按需读取能缩短多少启动等待 {#b-lazy-startup}

- **文档：** `lazy-image-startup.md`（中英文同步）。
- **角色：** user-facing。
- **Motivation：** 频繁创建短任务环境时，用户需要判断客户端按需读取是否比完整拉取、解包更划算，以及镜像服务端准备成本如何摊销。
- **想要的结论：** 同一固定 linux/amd64 digest、同一负载下（Ubuntu shell、Python/NumPy 脚本与可选 PyTorch CPU 导入分别成批，不合并），Docker 与 pVisor cache-service lazy VM 的客户端冷/热缓存 ready、completion 和传输量；明确服务端预准备成本，不解释为纯 VMM 差异。
- **实验设计：** 开源 Distribution registry 提供固定 OCI 镜像，pvisor-cache serve 提供同镜像文件索引和内容。两服务均为本机 loopback，无人工延迟时只能解释为本机模拟远程协议，不是 WAN。按独立网络条件分批；配置两核/2 GiB（容器硬限制与 VM guest RAM 不等价，Docker daemon/containerd 未作整机两核约束，必须披露），随机交替每组至少 30 次，冷客户端缓存后紧接热缓存，新 guest/容器与工作区。Ubuntu 负载校验发行版身份和 shell 输出；Python 负载禁用 bytecode 写入，固定单线程，校验 Python/库版本和确定性 NumPy 数组与矩阵运算（可选 PyTorch 还须校验 CPU-only build 和张量运算），ready 为全部校验后的唯一输出。两组均要求成功退出；失败单列，不剔除慢有效样本。预热/预检不纳入正式样本；非分离分布 P50、分离分布各簇比例和中位数、参考 P95、配对 bootstrap 95% CI。镜像服务端拉取/解包/索引时间单独保留，原始日志、源码和制品摘要放 `.data/`。不回答完整 OS 引导、真实 WAN、并发或大型任务性能。
- **当前实现复测：** 新静态 release 的 Docker/lazy 对照按负载独立成批，显式启用客户端索引页、上游连接池及持久私有桥；私有 namespace 隔离已有 Host listener。可使用校验后的原始压缩 OCI blobs 发布新 registry，并复制支持的 cache store，此时仅报告缓存准备成本，不沿用历史首次拉取耗时；Read Data、Metadata Data 和总响应分别计数。旧批次保留且不与新批次合并或计算实现加速比。
- **入口脚本：** `lazy_startup.py`。

### B-LAZY-ENG：小文件 lazy image V2 的工程对照 {#b-lazy-eng}

- **文档：** 工程结果保留 `benchmark/pvisor/LAZY_IMAGE_V2_REPORT.md`、`benchmark/pvisor/LAZY_INDEX_PAGES_REPORT.md`、`benchmark/pvisor/LAZY_BRIDGE_REPORT.md` 和 `.data/`；不混入用户启动页的历史 Docker 对照。
- **角色：** engineering A/B。
- **Motivation：** 判断有界目录元数据预取与持久连接是否减少 Python 导入的小文件请求成本。
- **想要的结论：** 同一冻结制品、同一 NumPy 镜像及脚本下，关闭/开启 V2 的冷/热 ready、completion、请求数、连接数和内容量变化，附配对 bootstrap 95% CI；不宣称 Docker/WAN 或吞吐排名。
- **实验设计：** 私有 user/mount/PID namespace 隔离 Host listener；新 VM/workspace/stage，CPU 0/1、2 vCPU、2 GiB guest RAM，预准备 cache 服务在 CPU 2/3，本机 loopback TCP，无延迟注入。每轮随机交替 V1-compatible（`PVISOR_LAZY_IMAGE_V2=0`）/V2（默认启用），每模式冷客户端后紧接热，至少 30 轮，另有初始轮与 3 warmups。两模式均包含正确分页等共同修复，开关只比较元数据预取与连接复用；当前 harness 在此对照中显式关闭客户端索引页（`PVISOR_LAZY_INDEX_PAGES=0`）。唯一正确 NumPy 输出、退出码、Run Bundle、冷内容非零、热内容为零全部通过才有效；任何失败使批次无效，不剔除慢有效样本。分布规则同 B-LAZY-STARTUP，P95 仅参考。冻结源码、制品、harness、请求和原始报告在 `.data/`，不与历史批次合并。
- **客户端索引页对照：** 独立批次比较 V2 RPC（`PVISOR_LAZY_IMAGE_V2=1, PVISOR_LAZY_INDEX_PAGES=0`）与 V2 客户端页（两项均为 1），隔离索引页带来的增量收益；另保留 V1/旧服务能力缺省回退测试。复用同一 NumPy、预算、正确性和 30 轮协议，文件内容载荷与二进制元数据载荷分别计数，不将索引页计为文件内容；原始证据及工程报告独立于已有 V1/V2 和 Docker 批次。
- **私有网络桥对照：** 独立批次比较桥 V1 单次连接与桥 V2 持久连接，V2 客户端与上游连接池均开启；分别在 RPC 与索引页模式测量，不跨模式合并。桥的连接读帧/idle 等待与四个执行 worker 分离，有界接入和队列。分别记录 runner→桥 Unix 连接/请求以及桥→服务 TCP 连接/请求，校验文件与元数据字节一致；同制品开关、预算、NumPy、30 轮、正确性和 bootstrap 规则同上，不混入历史批次。
- **入口脚本：** `lazy_image_v2.py`。

### B-FS-TOOLS：开发工具在各执行模式下要多花多少时间 {#b-fs-tools}

- **文档：** `filesystem.md`
- **角色：** user-facing
- **Motivation：** Agent 的大部分时间花在遍历、读写、git、搜索、编译和装依赖上。用户需要知道选择 staged 或 VM 后，这些工具会慢多少，以便选择模式和设置超时。
- **想要的结论：** "相对原生和 Docker，pVisor staged 在哪类操作上接近、在哪类操作上慢几倍；VM 相对轻量 VM 慢或快多少；一次七项工具任务的总等待是多少"，以及每种模式最适合的任务类型。
- **实验设计：**
  - 七项负载：2,048 个文件的遍历、64 MiB 读取并校验、256 个文件写入、git status、rg、小型 Cargo 编译、离线 npm 安装。每项代表一类常见的 Agent 工具操作。
  - 对照组为原生、Docker bind mount、Firecracker、QEMU（q35 和 microvm），以及 pVisor host staged 和 VM。
  - 所有组使用相同的工具环境、相同的两核预算和相同的 VM 内存；每次执行使用新的工作区。
  - 工具缓存每任务从空目录开始，默认保留执行器提供的 TMPDIR，在其中创建私有 HOME 与启用的 Node 编译缓存；七项工具中的 Cargo 使用工作区内独立的 `_cargo-home`、`_tmp` 和 `_cargo-target`，固定修复中的 Cargo 使用任务临时目录。记录实际缓存目录与文件系统类型。各执行器的默认临时存储差异是配置成本的一部分，不解释为纯 VMM 成本。工作区存储缓存是显式控制条件，与默认口径分开采样和统计。
  - 分别报告每项工具耗时和完整任务耗时（包括启动与收尾）。
  - 每个样本都要校验工具输出；暂存模式还要校验宿主原文件未被写入、upper 内容完整。
  - 不能回答的问题：大型仓库、冷磁盘、真实 registry 和并发吞吐。
- **入口脚本：** `reference_baselines.py`（跨运行时对照）。

### B-FS-ENG：文件系统改动的工程 A/B {#b-fs-eng}

- **文档：** `docs/src/*/design/filesystem-performance-analysis.md`
- **角色：** engineering A/B
- **Motivation：** 开发者需要判断一次文件系统改动是否让 B-FS-TOOLS 的用户结论变好，并定位时间花在哪一层（传输、OverlayCore、持久化、内容指纹）。
- **想要的结论：** "改动 X 让负载 Y 的中位数变化 Z%（95% 置信区间），其余负载未检出差异"；以及"staged 与 FUSE 直通之间的差距中，持久化占 A ms，内容指纹占 B ms，路径解析占 C ms"。
- **实验设计：**
  - 版本 A/B 使用同一份冻结源码，只应用待测改动后构建两个二进制，同机随机交替运行，每格 30 个样本，并报告置信区间。
  - 分解实验在同一批次内对照原生、FUSE 直通和 staged，同时打开 profile 计数器另跑一批；带计数器的批次不计入计时结论。
  - lazy 镜像、stage 持久化策略、内核缓存探针各自独立成批。
  - 只有当结果改变了 B-FS-TOOLS 的用户结论时，才更新 `filesystem.md`。
- **新增独立 kernel-cache 实验：** `kernel_cache_runner.py` / `kernel_cache_driver.rs`，同一冻结 release 制品 native、legacy-writable（immutable 物理缓存开启，默认 1s）、metadata-writable60s、metadata-readonly60s、metadata-and-data-readonly60s；2048/32 half-deep、全字节验证、3 warmups/30 seed-shuffled samples、CPU 0,1；hot、legacy-TTL-expired/extended-TTL-warm、single-pass readsearch、verified git/rg 与 startup/mount/task/unmount。 全部 native/lower/upper/work 使用相同 private user/mount/PID namespace 的真正 noatime tmpfs backing；保留 mountinfo、past-atime 读验证、监督/终止回执；旧 Btrfs/future-atime kernel-cache 批次未验收，只作历史诊断，raw 保留且不可拼接。writable 必须保留 fusectl abort guard；独占 fresh upper/work、稳定 immutable lower，无 journal/preimage/metrics/custom policy/exclusions；KEEP_CACHE 只在两种 readonly 条件间归因。报告 `benchmark/pvisor/KERNEL_CACHE_REPORT.md` 仅属 Linux HOST API 工程实验，不代表 pvisor run/VM/review guarantees。
- **入口脚本：** `filesystem_ab.py`、`filesystem_stage_durability.py`、`filesystem_lazy_ab.py`、`immutable_lower_cache.py`（同一 release 二进制 native/mutable/immutable-cache-off/on，独占 immutable lower，真实 host FUSE 的 hot/TTL-expiry 重复 metadata/open/read、readsearch 与 upper 正确性；无 journal，排除 review；工程报告 `benchmark/pvisor/IMMUTABLE_LOWER_CACHE_REPORT.md`）。

- **宿主 copy-up 调度对照：** `host_copy_up_ab.py` / `host_copy_up_driver.rs`；从同一工作树冻结两份 release 源码，只撤销/保留宿主调度和 prepared copy-up 改动。macOS FSKit，同一 APFS lower，1 GiB 非稀疏固定数据、64 个独立 metadata 路径，关闭日志与 compact strict 日志分别成条件；3 次预热、30 个随机配对轮次。观察实际临时文件后立即测首次 stat 与 64-path metadata，另测 writable-open 完成和空闲 metadata；完整 upper/lower 内容、基线观察和卸载校验。任何错误、未命中复制窗口或检测到构建/测试干扰使批次失败；保留慢有效样本，不外推 Linux、冷磁盘、并发吞吐或整个 staged 任务。报告和加工 CSV 留在 `benchmark/pvisor/`，原始证据在 `.data/`。

- **读队列与 journal 锁对照：** `host_read_journal_ab.py` / `host_read_journal_driver.rs`；冻结同一源码，仅撤销/保留读调度与指纹锁改动。macOS FSKit 的实际部分 copy-up 窗口中并发测只读 OPEN、已打开描述符 READ、OPENDIR，关闭日志、legacy 与 compact strict 分条件；另以共享 Core 的真实 1 GiB 原生文件指纹计算测 unrelated journal 观测，legacy/compact 独立成批。原生文件访问时间变化证明首次读已发生，开始探测时哈希线程必须仍在运行，不人工延迟。每格 3 次预热、30 个 seeded 随机配对样本，空闲对照、完整内容/观察/卸载和干扰门禁、配对 bootstrap 95% CI；不合并复制与哈希条件，不宣称冷磁盘、Linux、VM、吞吐或端到端工具收益。报告/加工 CSV 在 `benchmark/pvisor/`，原始来源和失败在 `.data/`。

### B-FS-DIAG：文件系统请求成本分解 {#b-fs-diag}

- **角色：** diagnostic
- **Motivation：** 定位 FUSE 传输、OverlayCore、持久化、内容指纹和缓存路径的成本。
- **想要的结论：** 请求与 inclusive span 的成本分解，不能相加为精确归因，不作为用户性能数据。
- **Kernel-cache 诊断入口：** `kernel_cache_runner.py --profiles`；每个 count case 使用独立 fresh mount，记录 warm priming 与操作的完整请求总数，另测 prime-only 以分离 warm 增量。源码推导 expected instances（包括 notifier 是否创建 profile），核查全部 PID/component/instance 的唯一 final record，不能固定忽略额外线程实例；任何缺失/额外实例或失败都拒绝。诊断耗时绝不进入正式 distribution。
- **实验设计：** 独立诊断批次，直通 FUSE 仅为不含暂存语义的下限；插桩计时与正式性能采样分开。`immutable_lower_cache.py --profiles` 保留每个 PID/component/instance 的所有累计 records，以各实例 final record 比较 physical parent/leaf stats 和 cache hits/misses/evictions；inclusive spans 不相加。真实 mount 若被拒绝，保留失败并仅以明确标记的 core 机制实验补充，不替代 FUSE 结果。
- **入口脚本：** `filesystem_fuse_ab.py`、`filesystem_stage_ab.py`、`filesystem_kernel_probe.py`、`filesystem_exec_probe.py`、`filesystem_counters.py`、`filesystem_diagnostic.py`、`immutable_lower_cache.py --profiles`。`filesystem_stage_durability.py --profiles` 也服务此诊断条目；不开启 profile 时服务 B-FS-ENG。`filesystem_exec_probe.py` 在新 VM 中比较相同可执行文件、loader 和全部动态库从 virtio-fs 与匿名 RAM 执行；两组都先复制并校验所有输入、传递相同 fd，避免将准备成本混入 exec。准备来源分为原始文件和独立副本：前者预热原 inode，后者从工作区独立 inode 读入相同字节，保留原工具文件首次映射的机会；共同的 Python 准备仍会预热解释器及部分共享库，不能称为完全冷启动。首个 exec 与后续重复 exec 分开，且不由热路径的零差异否定首次映射成本。guest `/dev/shm` 保持 noexec，使用显式 executable memfd；不重挂载或放宽策略。独立 VM 的完整计数器包含相同准备与指定次数 exec，差异用于请求归因，不能当作完整 Agent 启动水位。

### B-STARTUP-ENG：初始化实现的工程 A/B {#b-startup-eng}

- **角色：** engineering A/B
- **Motivation：** 判断 guest init 实现变化是否改变启动等待。
- **想要的结论：** 同输入就绪中位数差异及 95% bootstrap 区间。
- **实验设计：** 相同 runner、rootfs 与 payload，随机交错实现，不作为跨产品用户对照。
- **入口脚本：** `guest_init.py`。

### B-KERNEL-ENG：裁剪固件是否缩短当前产品的启动 {#b-kernel-eng}

- **角色：** engineering A/B；放入 `docs/src/*/design/vm-startup-performance-analysis.md`，不作为跨产品用户排名。
- **Motivation：** 判断裁剪 guest 内核的收益，以及它是否仍支持工作区、开发工具和网络所需能力。
- **想要的结论：** 相同 pVisor 二进制、相同 Linux 源码和补丁、相同打包方式，仅配置不同的两份新固件，其就绪/完整任务中位数差及 95% 配对 bootstrap 区间；列出减少的功能与未验证能力。
- **实验设计：** 冻结固件源码、输入 tarball、补丁、配置与编译记录，分别重建通用配置和裁剪配置；不复用旧固件作为基线。先验证构建和同样负载的输出、暂存隔离与工作区内容；Linux KVM 上以相同两核/内存/工具输入，随机交替 shell-ready、七项文件工具和修复任务，每格 30 次、3 次预热。CPU 缓解、seccomp、namespaces、virtio-fs 等所需配置必须保留；网络/恢复能力需独立正确性检查，不能由 shell 启动通过推导。配置中 guest LSM/设备功能差异必须明确，不称为相同加固程度的排名。
- **入口脚本：** `kernel_comparison.py`；`prepare_firmware_comparison.py` 是离线构建和来源准备 helper。

### B-STARTUP-DIAG：固件启动阶段诊断 {#b-startup-diag}

- **角色：** diagnostic
- **Motivation：** 定位内核和初始化阶段对就绪时间的贡献。
- **想要的结论：** 各阶段计时及计时边界，不作为用户启动水位。
- **实验设计：** 固定 guest payload 与 runner；早期内核时钟和收尾时间分别解释。
- **入口脚本：** `firmware_boot.py`。

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
- **入口脚本：** `v1/replay.py`。

### B-VM-MEMORY：VM 闲置时能省多少内存，恢复要付出什么 {#b-vm-memory}

- **文档：** `vm-memory/index.md`
- **角色：** user-facing
- **Motivation：** 长期挂起的 Agent 环境占用内存。用户需要知道冷页回收、offload 和快照能省多少内存，以及恢复访问时要等多久。
- **想要的结论：** "闲置 VM 的驻留内存可以降低 X%，以整机物理内存衡量；恢复后首次访问会增加 Y ms"；以及与 Docker 或其他 VM 相比是否真的更省。
- **实验设计：**
  - 用整机或 cgroup 的物理内存作为主要指标，进程 footprint 只作参考。
  - 用户选型按默认空闲页回收、运行中压缩、闲置卸载和跨实例共享组织。默认回收测工作集释放/再次分配；压缩测可压缩、随机和读热点混合内容；卸载比较继续运行、仅暂停、raw 和压缩 backing；共享测 1/2/4 实例与写入后的退化。开启命令与内存、CPU、峰值和下一次完整工具任务等待一起报告。
  - 同时测回收量、恢复延迟和 CPU 开销。
  - 当前用户页以 512 MiB 配置的 VM 为基准：主表使用完整产品进程 PSS 之和描述实际常驻物理内存，共享页按比例计入，包含压缩池和辅助进程；明确它不包含未映射文件缓存及内核内存，整组 cgroup 内存与缓存另外报告。不能把 PSS 当作完整宿主成本，也不能把配置容量当作实际占用。
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
- [ ] 中英文两版同步更新；加工表格与整理后的 CSV 已加入版本控制，原始样本与制品清单位于忽略的 `.data/`。

## 目录

- [`pvisor/`](pvisor/README.md)：测量脚本、运行方式与报告格式。
- [`replay/`](replay/qwen3.6-results.md)：Qwen3.6 SandboxReplay 实验记录，保留历史样本，不作为产品保证。
- 用户文档：[`docs/src/zh/benchmarks/`](../docs/src/zh/benchmarks/index.md)。
- 技术分析：`docs/src/zh/design/*-performance-analysis.md`。
