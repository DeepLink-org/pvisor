# Event v3：请求、改写与结果

Event 是不可变的观测事实。它把 [核心表达式](pvisor-algebra.md) 与执行过程连接起来：
保留原请求，记录实际采用的规则和执行表达式，再记录结果。`fs.read(...)` 本身表示请求内容，
事件阶段说明它被请求、改写、派发还是完成。

## 1. 公共信封

`persisting_control::trace::Event` 包含以下字段：

| 字段 | 契约 |
|---|---|
| version | 当前为 3；未知公共版本拒绝 |
| id | 一条事实的稳定身份，重试保持不变 |
| trace_id / producer | 采集关联及生产者身份；可信接入方负责绑定实际来源 |
| observed_at_unix_ms | 观察时间，不用于推导跨进程因果 |
| scope | 归属路径，如 Run / Attempt；不授予权限 |
| context | 指向当时固定的主体、策略和能力绑定 |
| operation | 一次原始请求的身份；外部观察可为空 |
| caused_by | 因果事实引用；允许缺失引用，拒绝自引用和已知因果环 |
| level | Trace / Debug / Info / Warn / Error |
| granularity | Milestone / Operation / Detail，与严重程度独立 |
| data | 类型化的核心事实或带名字/版本的领域观察 |

事件域由载荷决定。当前文件请求、派发与完成属于 filesystem，改写属于 policy，
上下文定义属于 execution；Observation 自带领域，例如 http、network、vm、llm。
相同组件可以产生不同域的事件。Core 的失败完成默认使用 Error，拒绝和不支持使用 Warn；
生产者可按已定义的领域契约设置观察的 level 与 granularity。

## 2. 核心事实

| Fact | 载荷与含义 |
|---|---|
| Context | 固定的执行上下文定义 |
| Requested | 完整、原始的操作项及请求携带的上下文链 |
| Rewritten | 实际规则、pass、before 和 after |
| Dispatched | 后端标识和实际交给它的完整表达式 |
| Completed | 最终采用的表达式、拟交付的 Outcome 及来源 |
| Observation | domain、name、version 与 JSON payload |

一个 operation 的 Requested 保留原请求。Rewritten 产生派生表达式，不回写 Requested。
Append/SetContexts 保持原语和实参，Replace 明确记录操作替换。读取 Rewritten 时，校验器
重新执行纯改写并要求 `rule.apply(before) == after`，因此伪造的后缀改写不能悄悄修改资源。

Completed 的来源是 Backend、Policy、Replay 或 Runtime。运行器的 mock/deny 使用 Policy，
输入/准入等运行器决定使用 Runtime，实际后端返回使用 Backend。Replay 来源留给具有相应
证据的回放生产者；普通执行器不会因为结果相同就宣称发生了回放。

Completed 表示拟返回结果已确定。实际调用者是否收到结果，由接入方记录交付观察。
披露检查拒绝时，先保存 `execution.delivery_denied`，其中保留已观察的结果，再记录拒绝。

## 3. 因果与 scope

运行器依次记录 Context → Requested → Rewritten* → Dispatched? → Completed。
请求及其后续事实共享 context、operation 和 scope，Context 事实定义对应的 context；
caused_by 连接实际前置事实。
外层 mock/deny 不产生 Dispatched，因为其内部上下文和操作未运行。

后端内部步骤通过 `ExecutionContext.dispatch_event` 关联到真实派发。例如确认暂停 VM、
取得远端数据或恢复 VM 可记录为 Observation，不需要加入 Agent 原语集合。
跨操作依赖须由掌握依赖的接入层显式记录，不能从相同 scope 或相邻 journal 位置推断。

事件结构校验不等于整条执行历史完整。单独缺失 Requested、上下文或因果前件时，应报告
覆盖缺口；合并外部记录时还须按 operation 核对请求身份、表达式连续性和完成唯一性。
当前 journal 检查单事件契约、身份、位置与因果环，不宣称完成了跨生产者历史认证。

## 4. 日志展示与数据保存

管道式表达便于先看操作，再看包裹层：

```text
op=17 requested  fs.read("file-17", offset: 0, length: 4096)
op=17 rewritten  rule=route@1 pass=0 [fs.read(...)] => [fs.read(...) |> remote("node-a")]
op=17 dispatched backend="router" fs.read(...) |> remote("node-a")
op=17 completed  fs.read(...) |> remote("node-a") => ok(bytes([...]))
```

以上 ID 与省略号仅缩短示意。实际 `trace show` 输出完整表达式和结果，JSON 保留完整
信封及结构化载荷。匹配、验证与重放使用结构，不解析展示行来猜测权限或执行状态。
当前记录内联数据内容，不实现自动脱敏；读取和导出沿用执行环境的数据权限。

## 5. 取消、失败与提交

取消、连接中断或缺失完成事件，都不能证明操作没有发生。执行结果未知时保留 Unknown
及已知效果；不得把“日志未记录”解释成“没有执行”。确定的结果不因日志提交失败而改写，
调用方通过 `Execution.audit_errors` 获取审计缺口。

`Record` 将 Event 与独立的 `{ journal, offset }` 配对，提交回执包含 event ID、位置和
Volatile/LocalSync。文件头为 `pvisor.trace/3`，旧草稿 journal 明确拒绝，不混写。

- 同 ID、同内容的重试返回原位置；同 ID、不同内容拒绝。
- 一个文件只允许一个写入者；异步等待者取消后，已接受的写入任务继续完成。
- 写入并同步后才返回 LocalSync。写入/同步失败返回 Unknown，当前句柄停止接收写入；
  关闭所有克隆句柄、重开并恢复后再确认或重试。
- 重开校验记录，只截去未完成的末行；完整但损坏的记录报错。
- journal 位置连续不表示执行完整，LocalSync 也不表示业务操作已完成。

单事件限 1 MiB，scope 深度限 16，因果引用限 64。新建文件权限为 0600。
大内容引用、脱敏/分层采集、保留期及跨进程采集随接入模块实现；本版不隐藏字段省略。

## 6. 接入状态与检查

新执行器及 journal 已实现这套定义；已有 `persisting_control::events` v1 记录和各驱动
生产入口尚未迁移。不能以新 Event 的存在推断已有系统调用已获得新核心覆盖。

运行 [IR 示例](pvisor-ir.md) 可生成文件读取及 mock 改写的真实 trace，并通过
`pvisor trace show/check/json` 检查。测试覆盖原请求保持、改写证据、派发链、结果类型、
取消、审计缺口、提交幂等、并发位置、断尾恢复、完整损坏和因果环。
