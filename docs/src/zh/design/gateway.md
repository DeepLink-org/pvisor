# Gateway 实现

Gateway 是嵌入 pVisor 的运行时驱动，负责模型调用路由和已观察流量的记录。它随 Run 启停，公共 CLI 不提供独立 Gateway daemon。

## 数据路径

```text
Agent → 注入的代理或 base URL → OverlayNet HTTP 路径
      → 协议适配器 → capture engine → 按 story 串行处理的 actor → event sink
```

协议适配器把支持的请求和响应转换为 `pvisor-core` 共享事件词汇。引擎携带 Run、Attempt、agent、session 和 story 身份。每个 story 的 actor 串行写入 sink，并在追加成功后更新内存中的轮次索引。

公开捕获输出使用 `pvisor_core::event::Event` 与共享 Journal。可变 capture 输入只用于对话投影，
不是第二套正式事件信封。草稿不进入事实日志，Markdown 参数仍仅作兼容。

## 顺序与持久化

Journal 位置表示提交顺序；稳定事件 ID 支持幂等重试，因果引用表示已知依赖。
Run 与内嵌 Gateway 共用 Journal。story actor 先提交事实，再更新 Story、SessionIndex
和通知观察者；观察者失败不撤销已提交事实。

命令 WAL 已删除。启动时从已提交事实重建投影，不重放 HTTP 请求、不重复通知观察者，
也不重写日志。有界输入队列仍是尽力而为的，只有 Journal 回执证明持久化。
flush 报告拒绝或失败的工作，shutdown 等待消费者释放 Journal。非空历史 WAL 会阻止
启动，要求先用旧版本排空，避免迁移时静默丢失数据。

## 观察边界

捕获只覆盖使用注入代理或 base URL 的客户端。executor 没有强制网络边界时，直接 socket 可以绕过显式代理。捕获响应证明在该路径上观察到了它，不能证明不存在其他流量。

捕获级别决定保留多少载荷。完整载荷可能包含用户提示、模型输出和请求细节。Run Bundle 证据与捕获事件是不同记录：事件并不包含 Bundle 的所有文件改动、输出字段或控制观察。

## 代码职责

| 组件 | 源码区域 | 职责 |
| --- | --- | --- |
| 协议解析与转发 | `pvisor-gateway` | 模型协议转换与调用观察 |
| 引擎与 story actor | `pvisor-gateway/src/engine` | Journal 提交、因果身份和轮次投影 |
| 事件词汇 | `pvisor-core` | 共享序列化记录；sink 实现与接入归运行组件 |
| 运行时集成 | `pvisor` | Run 生命周期、路由配置、事件 sink 和关闭 |
| 网络路径 | `pvisor-overlaynet` | 代理传输和策略接入 |

使用方式见[捕获指南](../guides/capture.md)。网络强制控制见 [OverlayNet](overlaynet.md)，事件契约见 [Operation 与 Event](operations-events.md)，组件归属见[核心架构](architecture.md)。
