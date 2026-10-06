# 已有 Docker 或 devcontainer，还需要 pVisor 吗？

## 主要结论 {#conclusions}

**pVisor staged 在所测修复任务中短于 Docker，在七项文件系统任务中长于 Docker。需要保留改动、选择性合入和冲突拒绝时评估 stage；工具速度本身不支持全面替代容器。**

| 需求 | 选型含义 |
| --- | --- |
| 本机执行并保留改动 | 评估 host/staged 与完整审查流程 |
| 独立 guest 内核 | 同时预算启动和 VM 工具等待 |
| 并发或闲置环境 | 需要固定资源下的吞吐与物理内存实测 |

## Motivation {#motivation}

可写 bind mount 会立即改宿主，而独立工作区还需要创建、审查和合入。工具等待和完整改动流程共同决定使用成本。

## 实验设计 {#interpretation}

Linux x86_64，AMD Ryzen 7 9700X，Fedora 7.2.8-200.fc44.x86_64。执行进程树与专用 Docker daemon 固定到 CPU 0,1；VM 为 2 vCPU，shell 探针配置 128 MiB，工具任务配置 16 GiB。原生/Docker 未限制内存，因此是 CPU 控制的任务对照，不能推导相同内存预算下的容量。host/staged 使用 rootless_process。

同一套离线工具与固定输入，每次新建工作区；热缓存、3 次预热、每格 60 次正式采样，固定种子随机交替执行。环境准备、构建、镜像导入和输入重置不计时；启动和退出计入完整任务。Docker Engine 29.7.2 使用专用 rootless **overlay2** daemon、经典镜像存储和可写 bind mount；Firecracker 1.13.1 PCI 不使用 jailer，QEMU 10.2.2 分别使用 q35/microvm 与私有 ext4。pVisor VM 使用 virtio-fs 和自己的固件。内核、存储和暂存语义不同，结果是这些配置下的任务成本，不是纯 VMM 或安全排名。

对照 Docker 的可写 bind mount 与 pVisor 的暂存任务，复用三个独立负载。Docker 加 Git 的完整审查流程、devcontainer 插件和 Docker Desktop 未测；本机 Git worktree/reflink 对照见[完整审查流程](supervision-cost.md)。

## 实验数据和分析 {#results}

测于 2026-10-06，每个后端/负载 60/60 有效，正式失败 0。输出、退出和执行器记录必须通过校验；暂存模式还验证宿主原文件不变和完整改动保留。保留所有有效慢样本，没有按耗时剔除。表格通常为 P50；分离分布展示各簇中位数和数量，P95 仅作观察参考。原始报告、二进制、输入与源码摘要保存在忽略的 `.data/`，公开 CSV 保留负载、批次和来源关联。

### 完整任务对照 {#reference-comparison}

| Runtime | Ready P50 ms | Repair completion P50 s | Seven-tool completion P50 s |
| --- | --- | --- | --- |
| Native | 1.28 | 0.44 | 0.45 |
| pVisor host | 12.72 | 0.46 | 0.47 |
| pVisor staged | 25.13 | 0.64 | 1.09 |
| Docker rootless / overlay2 | 74.48 | 0.81 | 0.82 |
| pVisor VM | 100.73 | 3.25 | 4.27 |

分项、计时边界与差异置信区间见[启动](startup.md)、[文件系统](filesystem.md)和[修复任务](agent-tasks.md)。

Docker 可写挂载直接改宿主，pVisor staged 到 apply 才合入选中路径。性能比较还需结合[隔离范围](isolation-tests.md)和[完整审查流程](supervision-cost.md)。

### 数据下载与复现 {#run}

[整理后的统计 CSV](compare-containers.csv) · [全部运行时统计](runtime-summary.csv) · [差异与 95% 置信区间](runtime-comparisons.csv) · [源码与制品来源](runtime-provenance.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
