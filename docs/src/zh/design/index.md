# 实现设计

本节面向需要追踪当前执行链路的贡献者。用户操作见[任务指南](../guides/index.md)，精确参数见[参考](../reference/index.md)。

| 领域 | 文档 |
| --- | --- |
| 职责与数据流 | [架构](architecture.md) |
| 宿主机、容器、VM 与文件系统边界 | [隔离](isolation.md) |
| 网络策略与拦截 | [OverlayNet](overlaynet.md) |
| 模型路由与捕获 | [Gateway](gateway.md) |
| CLI 与配置 | [命令模型](cli.md) |
| 工程取舍 | [设计原则](principles.md) |
| 后续分布式运行方向 | [从本地到集群](local-to-fleet.md) |

实现声明应能对应到代码和测试。未来方向会单独标明；存在某个契约或架构图，并不证明运行时已经强制执行。
