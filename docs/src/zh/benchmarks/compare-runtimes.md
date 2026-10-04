# 定位：gVisor / Firecracker / Kata

这些运行时提供执行边界；pVisor 在边界之上管理 Job、暂存、审查和证据。目前的 VM 后端是 libkrun，不能把 Firecracker、gVisor 或 Kata 的性能数字当成 pVisor 实测结果。

## 现状（2026-10-04）

| 基座 | 官方定位 | pVisor 接入状态 | 选型意义 |
|---|---|---|---|
| gVisor / runsc | 用每个 sandbox 的应用内核处理系统接口；提供 OCI runtime，与容器工具集成 | 没有专用、通过验收的 runsc 后端。OCI 接口提供接入可能，但本版只验证 crun | 需要容器接口并缩小对宿主内核的直接暴露时评估其兼容性 |
| Firecracker | 使用 KVM 的 microVM VMM，以较少设备减少攻击面 | 未接入；已安装 CLI 不等于 pVisor 已有 Firecracker executor | 已有 Linux microVM 供应系统时可作为基座候选 |
| Kata Containers | 用轻量 VM 提供容器工作流和独立 guest kernel | 未接入和验收；不能仅替换可执行文件就声称受支持 | 既有 Kubernetes/OCI 集群希望采用 VM 边界时评估 |
| libkrun | 本项目使用的 Linux guest VM，Linux/KVM、macOS/HVF | 当前 pVisor VM 后端；启动、生命周期与快照有本地证据 | 需要本机 VM、文件共享与 pVisor 集成语义 |

定位依据 [gVisor](https://gvisor.dev/docs/)、[Firecracker](https://firecracker-microvm.github.io/) 和 [Kata](https://katacontainers.io/) 官方说明。接入状态依据本项目[执行器接口与支持矩阵](../guides/executors/index.md)。

## 已测性能：pVisor VM / Firecracker / QEMU {#reference-comparison}

新增 Linux 同机、完整工具环境的独立参考 CLI 对照；“未接入 pVisor”与“未测性能”分开陈述。VM 都用 2 vCPU、相同两核预算，shell 128 MiB，工具任务 16 GiB。Firecracker/QEMU 共用内核与 ext4，pVisor 使用自己的内置 firmware/staged virtio-fs。QEMU microvm 与 q35 同时测，配置与所有失败公开。

| Runtime | First output P50 ms | Repair/tests P50 s | Codex loop P50 s |
|---|---|---|---|
| pVisor VM | 86.29 | 3.97 | 10.93 |
| Firecracker PCI | 73.74 | 2.25 | 7.83 |
| QEMU q35 | 218.12 | 1.98 | 7.69 |
| QEMU microvm | 88.10 | 1.85 | 7.67 |


pVisor 启动处于 microVM 的百毫秒量级；完整修复任务约为参考 microvm 的 2.1 倍，Codex 闭环也慢约 3.3 秒。因此当前优势是快速获得集成 pVisor 语义的 VM 环境，不能宣称工具性能领先。文件系统与 guest 配置不同，不把所有差值都归因于 libkrun。Claude 在三个参考 VM 各 30/30 通过，在 pVisor VM 初始化超时，兼容性尚有缺口。

gVisor/Kata 没有同机性能样本，不能借官方数字补进排名；Firecracker 无 jailer，部署安全范围也不同。[完整任务与图表](agent-tasks.md#reference-env) · [启动](startup.md#reference-startup) · [协议与资源](methodology.md#reference-env)


## 如何判断接入完成

可启动 workload 只是第一步。后端还需如实报告实际隔离、落实文件/网络/资源策略、终止完整进程树、保留 Run Bundle，并通过 stage/apply/replay 相关验收。快照是否包含 CPU、设备、磁盘和连接状态也应独立核对。

本版没有承诺上述三个后端的交付时间。后续接入可复用 pVisor 的执行协议，但要补齐运行时特有的策略映射及回归。已测性能见 [VM 启动](startup.md)、[内存与快照](vm-memory/index.md)和[隔离](isolation-tests.md)；它们不能证明隔离基座间的性能优劣。

## 更正

在 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 附后端版本、接入提交和测试证据；接入合并与验证后更新矩阵。
