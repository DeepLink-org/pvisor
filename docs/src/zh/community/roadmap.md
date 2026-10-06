# 路线图

主线的分级与规模轴见[信任阶梯](../why/trust-ladder.md)。下表列出 L1（本机逐个 Job）的持续工作与验收标准。

| 工作 | 验收标准 |
| --- | --- |
| 本地生命周期与暂存可靠性 | macOS/Linux 的完成、取消、超时、fork 和分批 apply 路径有回归；失败后可检查记录及剩余改动 |
| 边界与证据一致 | 按执行器验证文件和网络控制，Bundle 区分计划、安装回执、未观测与零计数；缺失控制不被标签掩盖 |
| Gateway 捕获可靠性 | 有界队列、提交失败、关闭和恢复有回归；明确入队与持久提交的差别 |
| Replay 兼容性 | 每个适配器固定受支持版本，验证完整工具批次和第一次续跑请求边界；实验报告记录样本及局限 |
| 文档与分发一致 | 入口示例可运行，默认行为只有一处权威定义 |

新增公开功能前，应具备实现入口、验证场景、限制说明和发布记录。改变数据契约、执行边界或公开命令时，先明确兼容策略和验收方式。

## 惰性镜像的小文件供给优化 {#lazy-image-small-files}

计划中：在镜像构建／发布时自动合并小文件，降低 S3-backed 惰性镜像的远端请求数和冷读取等待；尚无实现或性能验收结果。现有格式与限制见[共享镜像缓存 V1](../design/shared-image-cache-storage.md)，目录打包、确定性分桶与两级索引的详细提案见 [Lazy Image V2](../design/lazy-image-v2.md)。

设计方向：

- 文件路径、权限、hardlink、symlink 和 xattr 保持独立语义；物理内容按 package／目录等访问局部性组织到不可变 pack，以索引定位 offset 和 length。
- 区分对象大小、读取／压缩块大小与缓存粒度；支持范围读取和独立块解压，不要求整体下载或解压 pack。
- 配合本地元数据缓存、相邻读取合并、有界预取及并发 miss 合并；仅合并对象而逐文件请求，不算完成优化。
- 保留跨镜像内容共享，避免每发布一个镜像就重打公共依赖；大文件继续按需分块读取。先评估 Nydus／EROFS 的可复用能力，再决定是否扩展自有格式。

验收标准：

- 对比独立小文件对象、pack 内逐文件范围读取、pack 加读取合并／缓存三种策略；分别覆盖冷元数据、冷内容、暖缓存和多任务并发。
- 使用 Python import、Node.js 依赖加载、目录／属性扫描及真实编译／测试；记录首次有效工具调用、完整任务耗时及 P95/P99、请求数、下载字节、读放大、缓存占用、打包耗时与总成本。首次发布成本须入账，低复用环境单独评估；按 benchmark registry 与发布规则建立实验，不预设提升百分比。
- 验证文件语义、copy-up、变更交付、内容完整性与远端故障行为；格式变更前明确旧镜像 handle、checkpoint 依赖的兼容／迁移策略，以及 pack 引用保留与安全回收边界。

## 单机 daemon {#daemon}

[Daemon](../guides/daemon/index.md) 的部分 OpenSandbox 1.1.0 profile 已有 VM-only `NativeRuntime`，在独立 supervisor 中嵌入 pVisor；daemon 可执行入口与必需原生参数已接入。Stage/apply 与 checkpoint/fork API 未实现，也不自动获取 node/cache/pool 共享资源。Bootstrap／镜像未提供或端到端验证，没有 SDK 兼容或密度证据。Controller/Worker 与 Cluster 任务 SDK 已退役。

## L2 与 L3 里程碑

!!! note "建设中"
    L2/L3 的进入条件与缺口清单尚未定稿；下面汇总已写下的要求与验收标准，不构成排期或完成度承诺。级别定义见[信任阶梯](../why/trust-ladder.md)，容量依据见[并发密度](../benchmarks/density.md)。

### L2：本机多个 Job／一条流水线

进入条件（待定稿）：本机并发上限与资源模型有数据，作为容量规划依据，见[并发密度](../benchmarks/density.md)。

验收标准：

- 批量审查流程可复现：按 workspace 聚合、批量 apply，见[单机多 Agent 并行与批量审查](../guides/parallel-agents.md)。
- CI 接入给出可复制的 workflow 示例，明确 `apply` 的语义（谁审、何时合），失败与超时路径有回归，见[在 CI 中运行 Agent](../guides/ci.md)。

### L3：集群化执行、集中证据

进入条件（待定稿）：明确与调度器的边界——pVisor 提供执行语义与证据，跨节点编排交给 Kubernetes、Ray，见[集群化执行](../design/research/cluster-execution.md)。

验收标准：

- 给出外部跨节点调度器集成及证据集中后审计的缺口清单和阶段划分。pVisor 不提供集群总控；单机 daemon 没有全局 DAG 或分布式 lease。
- 容量依据与 L2 共用[并发密度](../benchmarks/density.md)。

