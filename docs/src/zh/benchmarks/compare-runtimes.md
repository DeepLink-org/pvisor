# 如何在 pVisor、Firecracker、QEMU 与隔离运行时之间选择？

## 主要结论 {#conclusions}

**pVisor VM 的轻量启动接近 Firecracker/QEMU microvm，工具任务目前更慢。** 同工具修复/测试 P50 为 pVisor **3.97 s**、Firecracker **2.25 s**、QEMU microvm **1.85 s**。无镜像 pVisor 比启动完整 Ubuntu 更早返回短任务结果，优势来自减少完整系统启动等待。

gVisor/Kata 没有同机性能数据。pVisor 当前 VM 基座为 libkrun；独立测过 Firecracker/QEMU 不表示它们已接入为 pVisor executor。

| 需求 | 选型含义 |
|---|---|
| 只需轻量 VM 执行 | 同时比较 Firecracker 和 QEMU microvm |
| 需要统一 stage/apply | 评估 pVisor 的执行与合入总成本 |
| 需要 gVisor 或 Kata | 没有同条件本机性能排名 |

## Motivation {#motivation}

需要独立 guest kernel、OCI 工作流或系统调用隔离时，执行边界是选型的一部分。还需要区分 VMM 启动、操作系统启动、工具执行和 pVisor 的暂存/记录成本。

## 实验设计 {#interpretation}

Linux 同机、相同两核预算、2 vCPU；轻量启动 128 MiB，工具任务 16 GiB，N=30、3 次预热。Firecracker 1.13.1 PCI 无 jailer，QEMU 10.2.2 分别用 q35/microvm，裁剪内核与静态 init。完整 Ubuntu 独立配置采用发行版内核、initrd 和 systemd，启动 2 GiB，Firecracker N=30、QEMU N=10。结果不混成相同安全或 OS 配置下的排名。

| 基座 | 执行方式 | pVisor 状态 |
|---|---|---|
| libkrun | 本机 Linux guest VM | 当前集成 VM 后端 |
| Firecracker / QEMU | 独立 VMM | 已测参考 CLI，未作为集成 executor 验收 |
| gVisor | 应用内核处理系统接口，提供 runsc | 未建立本机性能对照或专用后端验收 |
| Kata | VM 支持容器工作流 | 未接入验收或同机测量 |

官方定位见 [gVisor](https://gvisor.dev/docs/)、[Firecracker](https://firecracker-microvm.github.io/) 和 [Kata](https://katacontainers.io/)；pVisor 支持范围见[执行器](../guides/executors/index.md)。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

### 最小参考环境 {#reference-comparison}

| Runtime | First output P50 ms | Repair/tests P50 s | Codex loop P50 s | Seven-tool filesystem task P50 s |
|---|---:|---:|---:|---:|
| pVisor VM | 86.29 | 3.97 | 10.93 | 4.08 |
| Firecracker PCI | 73.74 | 2.25 | 7.83 | 2.37 |
| QEMU q35 | 218.12 | 1.98 | 7.69 | 1.82 |
| QEMU microvm | 88.10 | 1.85 | 7.67 | 1.77 |

七项文件系统任务列包含启动、工具运行和退出；pVisor 使用 2026-10-05 的最新 release 实测（2 vCPU / 4 GiB），Firecracker/QEMU 使用 2026-10-04 的参考实测（2 vCPU / 16 GiB），各 30 次采样。该列与修复/测试、Codex 闭环是不同负载；其余三列保留各自实测，不用文件系统结果替代。逐项工具耗时和 P95 见[文件系统对比](filesystem.md)。

启动处于同一量级，pVisor VM 的工具和 Codex 完整闭环更慢。Claude 在参考 VM 通过，在 pVisor VM 初始化超时。内核、virtio-fs/ext4、guest 与网络配置都不同，表格不能证明性能差的唯一原因是 libkrun。

### 完整 Ubuntu 部署 {#full-ubuntu}

pVisor 无镜像 Ready 约 **110 ms**，完整 Ubuntu 的 Firecracker **5.64 s**、QEMU q35 **5.43 s**、microvm **7.67 s**。修复/测试任务分别 **4.61、8.51、8.12、10.33 s**；pVisor 的工具内部为 **4.02 s**，参考 Ubuntu VM **2.30–2.81 s**。一次性短任务可减少开机等待；常驻环境不能仅凭启动数据选型。

数据使用各报告固定制品，没有随当前文件系统集成制品全面重测。[启动](startup.md) · [完整任务与兼容性](agent-tasks.md) · [方法](methodology.md)
