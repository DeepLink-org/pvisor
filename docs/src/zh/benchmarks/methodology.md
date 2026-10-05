# 哪些 benchmark 数字可以用于选型？

## 主要结论 {#conclusions}

**用同负载、同预算、同计时口径的实测选择执行模式；跨配置数据只用于了解各自性能水位。** 启动、工具执行、合入和资源占用回答不同问题，不能相互替代。

| 决策 | 需要的数据 |
|---|---|
| 选本地工具执行方式 | 原生、Docker、pVisor host/staged/VM 的任务对照 |
| 选独立 guest kernel | Firecracker、QEMU 与 pVisor 的启动和工具数据 |
| 设置容量或超时 | 成功率、完整任务等待、相同口径的资源占用 |

## Motivation {#motivation}

启动更快并不保证编译更快，可写挂载与暂存审查也提供不同工作流。选型需要同时知道速度、执行边界和改动最终如何进入原目录。

## 实验设计 {#interpretation}

### 同机对照

B-STARTUP、B-FS-TOOLS、B-AGENT-TASK 使用相同离线工具和固定输入，对照原生、pVisor host/staged/VM、私有 rootless Docker、Firecracker PCI、QEMU q35 和 microvm。镜像、工具与 daemon 预先准备，所有组使用同一两核亲和性，工具 VM 的内存预算相同；每个任务使用新的工作区，按固定种子随机交替执行。完整 Ubuntu、macOS 和不同制品单独成批。

下载、编译 benchmark 制品、镜像导入和输入复制不计入任务等待。计时区分首条有效输出、内部工具与校验、结果返回，以及启动到进程退出。成功样本必须通过输出、执行器和暂存完整性校验；暂存模式还检查宿主原目录未改动。

### 分布与失败

不合并批次，不事后剔除慢样本。失败与校验不通过单列，不能计作零耗时。未执行的容量 guard 不能当成完成任务。P95 只是所测样本的观察参考，少于 30 次不展示，公开统计不展示 P99；小样本不给稳定尾延迟结论。

存在分离簇时分别展示比例和各簇中位数。复测的描述性分簇规则在[运行手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)中给出，不据此推断原因。工程 A/B 的百分比变化需配中位数差异的 95% bootstrap 区间；用户页不展示优化过程。

### 隔离与资源 {#reference-resources}

Docker writable bind mount 直接写宿主；pVisor staged 保留改动到 apply。Firecracker/QEMU 使用私有 ext4，pVisor VM 使用 virtio-fs，内核与设备不同；对照不是纯 VMM 或生产安全排名。独立 stage 的隔离能力以实际记录和[隔离校验](isolation-tests.md)为准。

RSS 是定期采样的进程范围求和，可能重复共享页、漏掉短峰；Docker 必须包含实际容器进程与专属 daemon 并标明范围。Cluster 用互不重叠 cgroup 的内存总和。配置 RAM、RSS、macOS RAM proxy 和净物理内存不可混算。

## 实验数据和分析 {#results}

### 已准备环境 {#reference-env}

[启动](startup.md) · [文件与工具](filesystem.md) · [修复任务和 CLI](agent-tasks.md)。各表保留独立批次与样本数，使用固定制品，不代表第三方当前最新版。

### 完整发行版 {#full-ubuntu}

Ubuntu 使用发行版内核、initrd、systemd 和私有磁盘；pVisor 复用已准备工具目录。该对照回答部署等待，不隔离纯 VMM 成本。

### QEMU 完整发行版 {#full-ubuntu-qemu}

q35 和 microvm 使用同一 Ubuntu 模板；样本和百分位数与其他批次分开，不合并。

### 任务与资源 {#product-v1}

[合入](apply.md) · [网络](network.md) · [并发](density.md) · [隔离](isolation-tests.md) · [回放](replay-fidelity.md) · [审查](supervision-cost.md) · [Cluster](cluster-scalability.md)。未测的业界方案明确标记，不填入厂商宣传数字。

### 文件系统工程实验 {#filesystem-service}

工程 A/B、带计数器的 profile 和诊断探针保存在[技术分析](../design/filesystem-performance-analysis.md)，不作为跨产品主表。

### 数据位置与下载 {#evidence-format}

Markdown 保存面向用户的加工表格，每篇附可下载的同目录 CSV。原始报告、逐次样本、日志、制品清单和冻结 harness 放在相关目录的 `.data/`，被 Git 忽略，也不发布到站点。CSV 只保留整理后的统计和来源摘要，不能伪装成原始样本。

复现与来源保留规则见[运行手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)。

### 数据下载与复现 {#run}

[整理后的表格 CSV](methodology.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
