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

实现声明应能对应到代码和测试。当前设计只描述已存在的组件和运行路径；实际强制控制以 Run Bundle 的观察证据为准。
