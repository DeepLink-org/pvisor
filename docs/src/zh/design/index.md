# 实现设计

| 领域 | 文档 |
| --- | --- |
| 核心职责与执行路径 | [核心架构](architecture.md) |
| 操作、改写、放置与事实 | [Operation 与 Event](operations-events.md) |
| 宿主机、容器、VM 与文件系统边界 | [隔离](isolation.md) |
| 网络策略与拦截 | [OverlayNet](overlaynet.md) |
| 模型路由与捕获 | [Gateway](gateway.md) |
| CLI 与配置 | [命令模型](cli.md) |
| 工程取舍 | [设计原则](principles.md) |

实现声明应能对应到代码和测试。核心设计描述当前组件和运行路径；驱动文档中的目标方案须与已实现机制分开。实际强制控制以 Run Bundle 的观察证据为准。
