# 实现设计

| 领域 | 文档 |
| --- | --- |
| 核心职责与执行路径 | [核心架构](architecture.md) |
| 多 Worker 执行、最终一致性、调度、生命周期与存储 | [Cluster 完整设计](cluster/index.md) |
| 共享镜像缓存、S3/文件系统目录树与文件索引 | [v1：独立元数据与分页索引](shared-image-cache-storage.md) |
| VM RAM offload、文件布局与生命周期 | [offload 完整设计](offload/index.md) · [磁盘格式与 schema](offload/disk-layout-and-schema.md) |
| 完整 VM 环境保存、独立文件副本与跨 runner 恢复 | [完整环境快照 CLI](environment-snapshot.md) |
| 跨 VM 内容去重、共享池与冷页恢复 | [内存去重与冷页压缩](memory-sharing/index.md) |
| 操作、改写、放置与事实 | [Operation 与 Event](operations-events.md) |
| 文件合成、首次触达与 apply 恢复 | [OverlayCore 设计](overlayfs.md) |
| 事件追加、回执与尾部恢复 | [Journal 设计](journal.md) |
| 宿主机、容器、VM 与文件系统边界 | [隔离](isolation.md) |
| 网络策略与拦截 | [OverlayNet](overlaynet.md) |
| 模型路由与捕获 | [Gateway](gateway.md) |
| CLI 与配置 | [命令模型](cli.md) · [Job 检查点与分叉 CLI 设计稿](job-checkpoint-cli.md) |
| 工程取舍 | [设计原则](principles.md) |

实现声明应能对应到代码和测试。核心设计描述当前组件和运行路径；驱动文档中的目标方案须与已实现机制分开。实际强制控制以 Run Bundle 的观察证据为准。
