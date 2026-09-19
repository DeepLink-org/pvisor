# Gateway 实现

Gateway 是嵌入 pVisor 的运行时驱动，负责模型调用路由和已观察流量的记录。它随 Run 启停，公共 CLI 不提供独立 Gateway daemon。

## 数据路径

```text
Agent → 注入的代理或 base URL → OverlayNet HTTP 路径
      → 协议适配器 → capture engine → 按 story 串行处理的 actor → event sink
```

协议适配器把支持的请求和响应转换为 `persisting-control` 共享事件词汇。引擎携带 Run、Attempt、agent、session 和 story 身份。每个 story 的 actor 串行写入 sink，并在追加成功后更新内存中的轮次索引。

公开的捕获输出是 EventRecord JSONL。story actor 当前忽略草稿命令。`--gateway-stream-markdown` 为兼容旧调用保留，不会生成实时 Markdown 投影；需要展示层时，应从持久事件派生。

## 顺序与持久化

序号属于对应生产者或 session 的顺序范围。合并事件流时应保留身份字段；时间戳和孤立的 `seq` 都不能定义全局顺序。`timestamp` 与 `timestamp_unix_ms` 是同一观察时间的两种表达。

capture engine 使用有界异步 WAL 提交队列，由后台成组提交。进入队列不等于已同步持久化。恢复可重放已落盘但尚未确认的工作；进程在排队记录落盘前突然退出，仍可能丢失该记录。正常关闭会刷新待处理工作。判断记录是否完整时，需要检查捕获错误和死信。

sink 追加可能在写入部分字节后失败。除非 sink 能证明完全拒绝，该错误的结果应视为未知；把所有 I/O 错误都视为干净拒绝会导致不安全的重试。运行时 JSONL 使用仅所有者可读写的权限，重新打开已有文件时也会收紧权限。

## 观察边界

捕获只覆盖使用注入代理或 base URL 的客户端。executor 没有强制网络边界时，直接 socket 可以绕过显式代理。捕获响应证明在该路径上观察到了它，不能证明不存在其他流量。

捕获级别决定保留多少载荷。完整载荷可能包含用户提示、模型输出和请求细节。Run Bundle 证据与捕获事件是不同记录：事件并不包含 Bundle 的所有文件改动、输出字段或控制观察。

## 代码职责

| 组件 | 源码区域 | 职责 |
| --- | --- | --- |
| 协议解析与转发 | `persisting-gateway` | 模型协议转换与调用观察 |
| 引擎与 story actor | `persisting-gateway/src/engine` | 身份、顺序、WAL、追加和轮次状态 |
| 事件词汇 | `persisting-control` | 共享序列化记录和 sink 契约 |
| 运行时集成 | `persisting-pvisor` | Run 生命周期、路由配置、事件 sink 和关闭 |
| 网络路径 | `persisting-overlaynet` | 代理传输和策略接入 |

使用方式见[捕获指南](../guides/capture.md)。网络强制控制见 [OverlayNet](overlaynet.md)，执行记录见[系统架构](architecture.md)。
