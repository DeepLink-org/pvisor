# 实现设计

| 领域 | 文档 |
| --- | --- |
| 核心职责与执行路径 | [核心架构](architecture.md) |
| 单节点 sandbox 准入、生命周期、恢复与存储 | [Daemon 设计](daemon/index.md) |
| 共享镜像缓存、S3/文件系统目录树与文件索引 | [V1：当前实现](shared-image-cache-storage.md) · [Lazy Image V2：目录打包与按需索引提案](lazy-image-v2.md) |
| 内存优化的架构、理念与权衡 | [总体设计](memory-optimization/index.md) · [内存去重](memory-optimization/deduplication.md) · [内存卸载](memory-optimization/offload.md) · [内存压缩](memory-optimization/compression.md) |
| 完整 VM 环境保存、独立文件副本与跨 runner 恢复 | [完整环境快照 CLI](environment-snapshot.md) |
| 内存优化的实验机制与证据边界 | [实验性概念验证](memory-optimization/proof-of-concept.md) |
| 操作、改写、放置与事实 | [Operation 与 Event](operations-events.md) |
| 文件合成、首次触达与 apply 恢复 | [OverlayCore 设计](overlayfs.md) |
| 事件追加、回执与尾部恢复 | [Journal 设计](journal.md) |
| 宿主机、容器、VM 与文件系统边界 | [隔离](isolation.md) |
| 网络策略与拦截 | [OverlayNet](overlaynet.md) |
| 模型路由与捕获 | [Gateway](gateway.md) |
| CLI 与配置 | [命令模型](cli.md) · [Job 检查点与分叉 CLI 设计稿](job-checkpoint-cli.md) |
| 工程取舍 | [设计原则](principles.md) |

实现声明应能对应到代码和测试。核心设计描述当前组件和运行路径；驱动文档中的目标方案须与已实现机制分开。实际强制控制以 Run Bundle 的观察证据为准。
