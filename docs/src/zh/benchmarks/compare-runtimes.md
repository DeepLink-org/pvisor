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

## 如何判断接入完成

可启动 workload 只是第一步。后端还需如实报告实际隔离、落实文件/网络/资源策略、终止完整进程树、保留 Run Bundle，并通过 stage/apply/replay 相关验收。快照是否包含 CPU、设备、磁盘和连接状态也应独立核对。

本版没有承诺上述三个后端的交付时间。后续接入可复用 pVisor 的执行协议，但要补齐运行时特有的策略映射及回归。已测性能见 [VM 启动](startup.md)、[内存与快照](vm-memory/index.md)和[隔离](isolation-tests.md)；它们不能证明隔离基座间的性能优劣。

## 更正

在 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 附后端版本、接入提交和测试证据；接入合并与验证后更新矩阵。
