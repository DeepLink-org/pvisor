# Gateway 实现

Gateway 是嵌入 pVisor 的运行时驱动，负责模型调用路由和已观察流量的记录。它随 Run 启停，公共 CLI 不提供独立 Gateway daemon。

[Sandbox daemon](daemon/index.md) 未嵌入该 Gateway。它的 execd/egress 端点代理转发预制 OpenSandbox 服务，不提供模型路由／捕获、推理空闲 pause 协调或 CPU 预留释放。即使都转发 HTTP，这仍是不同数据路径。

## 数据路径

```text
Agent → 注入的代理或 base URL → OverlayNet HTTP 路径
      → 协议适配器 → capture engine → 按 story 的 worker/mailbox → Journal → 投影／观察者
```

协议适配器把支持的请求和响应转换为 `pvisor-core` 共享事件词汇。引擎携带 Run、Attempt、agent、session 和 story 身份。每个 story 的准备、Journal 提交及投影 I/O 由一个 worker 和一个有界 FIFO mailbox 统一持有。直接 `apply`、异步捕获与快照／屏障命令共用此所有者。本地 story 命令与 Run enrichment 使用类型化记录／状态。Run registry 锁只覆盖同步 enrichment，不跨 Journal 等待或跨 story 派发。

公开捕获输出使用 `pvisor_core::event::Event` 与共享 Journal。可变 capture 输入只用于对话投影，
不是第二套正式事件信封。流式捕获只发出最终响应或取消事件；公开草稿输入仍作为兼容性 no-op 接受。Markdown 参数仍仅作兼容。

## 委托凭据的动作范围 {#delegated-credential-actions}

Gateway 在选择模型路由、解析凭据之前检查 HTTP method 和端点。POST 允许
Chat Completions、Messages、Responses、Embeddings 和 token counting
的精确路径，支持无版本前缀或 `/v1`。原生 Gemini 支持 `/v1`、`/v1beta` 或
无版本前缀下的 `models/{model}:generateContent`、`:streamGenerateContent`
和 `:countTokens`。GET `/models`、`/v1/models`、`/v1beta/models` 只返回本地
模型配置，不联系上游。允许一个末尾斜杠。管理端点、其他 method、未知路径、
有转义/dot/重复斜杠歧义的路径，以及 method/path override headers 或 query
参数（`_method`、`method`、`path`、`url` 等）会被拒绝。`alt=sse`、`api-version`
等正常 API query 参数继续保留。

协议桥转换出的路径也要重新检查。Gemini 以 URI model 为身份，拒绝冲突的
body model；route forwarding 到其他模型时同步改写 URI。模型路径段只允许
ASCII 字母、数字、连字符、下划线和点，但不能是 `.` 或 `..`。

客户端必须使用这些 Gateway 路径；任意客户端前缀即使匹配协议后缀
也会被拒绝。可信 route 的 `upstream` 仍可包含 `/team/v1` 这样的服务前缀。
配置的上游必须实现所声明的 API 语义。普通网络出口权限与显式模型委托权限
分开：`no-network` 仍可允许这些模型调用；动作检查不代表必达留痕或消费预算。

旧 Detect 分类没有已定义的模型委托动作，不在白名单中。Realtime HTTP session/
凭据管理也不受委托；WebSocket 继续显式返回不支持。

## 顺序与持久化

Journal 位置表示提交顺序；稳定事件 ID 支持幂等重试，因果引用表示已知依赖。
Run 与内嵌 Gateway 共用 Journal。story worker 先提交事实，再更新 Story、SessionIndex
和通知观察者；观察者失败不撤销已提交事实。

启动时从已提交事实重建投影，不重放 HTTP 请求、不重复通知观察者，
也不重写日志。有界输入队列仍是尽力而为的，只有 Journal 回执证明持久化。
`spawn_apply` 采用非阻塞有界准入；直接 `apply` 等待进入同一 FIFO。接受不等于持久化，准入后取消等待不取消已持有的任务。flush 等待已接受任务及其等待的 backfill，报告拒绝或失败的工作，但不阻止生产者继续准入，也不是被拒事件诊断 writer 的屏障。子 Agent 链接 backfill 流向主 story；同 story 的 backfill 在所有者内直接执行，不等待自己的 mailbox。缺失 backfill 回执就是捕获缺口。shutdown 停止准入、排空已接受的尾部任务／backfill；即使另一个 story 报告缺口，也持久化最终投影／索引，随后消费者释放 Journal。

失败的已准备 backfill 保留 `prepared_story`（目标 StoryContext）与 `prepared_record_json`，包含已赋值的 event ID 和 timestamp。恢复将此保留记录提交给目标 story 的同一个有界所有者，绕过准备与 Run re-enrichment，避免已匹配的链接丢失或获得新身份。保留载荷仍受捕获级别过滤和敏感 body 脱敏约束。没有 `prepared_story` 的条目仍通过普通 `apply` 重试旧 source event；诊断是重试输入，不证明 Journal 已提交。

shutdown 显式等待有界被拒事件 writer 中的有序 marker，即使其他 runtime clone 仍持有 writer。Marker 保证尝试追加排在它之前的诊断，并报告 writer I/O 错误；它不对 dead-letter 文件执行 fsync，也不建立持久化证据。被该队列拒绝或在 marker 之后进入的诊断不在保证范围内。普通 `flush` 不等待此 marker。

## 观察边界

捕获只覆盖使用注入代理或 base URL 的客户端。executor 没有强制网络边界时，直接 socket 可以绕过显式代理。捕获响应证明在该路径上观察到了它，不能证明不存在其他流量。

捕获级别决定保留多少载荷。完整载荷可能包含用户提示、模型输出和请求细节。Run Bundle 证据与捕获事件是不同记录：事件并不包含 Bundle 的所有文件改动、输出字段或控制观察。

## 代码职责

| 组件 | 源码区域 | 职责 |
| --- | --- | --- |
| 协议解析与转发 | `pvisor-gateway` | 模型协议转换与调用观察 |
| 引擎与 story 调度所有者 | `pvisor-gateway/src/engine` | Journal 提交、因果身份和轮次投影 |
| 事件词汇 | `pvisor-core` | 共享序列化记录；sink 实现与接入归运行组件 |
| 运行时集成 | `pvisor` | Run 生命周期、路由配置、事件 sink 和关闭 |
| 网络路径 | `pvisor-overlaynet` | 代理传输和策略接入 |

使用方式见[捕获指南](../guides/capture.md)。网络强制控制见 [OverlayNet](overlaynet.md)，事件契约见 [Operation 与 Event](operations-events.md)，组件归属见[核心架构](architecture.md)。
