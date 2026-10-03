# 对比：云端沙箱

本地 pVisor 适合直接使用现有仓库和工具链；E2B、Daytona、Modal 适合把环境供应和容量扩展交给服务方。第一版比较部署与成本边界，不报告未执行的云端启动时间，也不把服务商宣传的启动数字与本地 CLI 数据并排排名。

## 比较范围

2026-10-04 核对官方文档和计费页面。没有调用云端账户、创建远程沙箱或消耗付费额度；SDK 版本、区域和账单配置因此属于未测。下表是文档能力，性能证据来自本站[启动](startup.md)、[文件系统](filesystem.md)与[并发密度](density.md)。

| 方案 | 工作区与工具 | 容量、生命周期和成本 | 适合的任务 |
|---|---|---|---|
| 本地 pVisor | 本机目录，或准备好的 OCI/VM rootfs；按配置共享工具、stage/apply | 本机 CPU/RAM/磁盘与运维成本；本版容量见实测 | 私有仓库、本地依赖、修改需返回同一工作区 |
| E2B | 通过 SDK 在远端 sandbox 执行，模板准备依赖并传入文件 | 托管环境、pause/resume；运行计算资源按秒计费，套餐有并发与资源限制 | 产品中按用户提供独立代码执行环境 |
| Daytona | SDK 创建并操作远端 sandbox，准备镜像及文件同步 | 托管生命周期；按秒计费，计算和存储配置影响成本 | 持续的远程 Agent 开发工作区 |
| Modal | SDK 创建 sandbox，使用镜像、Volume 和上传文件；支持 gVisor/VM 运行时 | 托管扩展，按请求配置的资源与时间计费，最低资源配置也影响账单 | 与现有 Modal 批处理、推理或计算流水线协作 |

出处：[E2B 文档](https://docs.e2b.dev/)与[计费](https://docs.e2b.dev/billing)、[Daytona 文档](https://www.daytona.io/docs/en/)与[价格](https://www.daytona.io/pricing)、[Modal Sandboxes](https://modal.com/docs/guide/sandboxes)与[资源计费](https://modal.com/docs/guide/sandbox-resources)。

## 数据与审查链路

远端执行需要将所需代码、输入和凭据送到执行位置；是否跨境取决于所选区域、账户及服务合同，不能一概称为“数据出境”。选择前确认区域、日志保留与删除设置。本地执行让工作区数据留在宿主，但模型 API、工具网络请求和外部记录目的地仍可能传出数据。

把已有本机 Rust/npm 任务迁到云端时，应分别记录镜像构建、上传、环境创建、依赖缓存、执行、结果下载和本地合入。远端 filesystem snapshot 或 pause/resume 并不自动等价于“把结果冲突安全地合入本地目录”；审查与合入需要调用方设计，也可以采用 pVisor 的 stage 协议。

成本模型建议记录 `运行资源 × 秒数 + 存储/网络/套餐费用 + 模型费用`；本地记录硬件折旧、电力、闲置和运维。这里没有账户账单实验，因此不宣称哪家更便宜。需要突发容量、多租户服务和远程 API 时优先评估云端；需要本地工具链与短反馈回路时先评估本地。

## 更正

在 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 提供日期、区域、SDK 版本、计费参数及完整阶段数据。服务商结果应标为其报告，不能替代本项目的同条件实测。
