# Operation 与 Event

Operation 描述请求及有效操作；Event 描述处理过程中发生的事实；Trace 是这些事实组成的记录。定义位于 `pvisor-core`，构造、调度和执行位于 pvisor，持久化位于 Journal。

## Operation：处理对象

当前 `OperationKind::RunExecute` 对应 `run.execute`。操作输入是程序、参数和工作目录；策略决定和 Placement 描述它将怎样执行。Operation schema 当前为 1。

| 字段 | 含义 |
| --- | --- |
| `version` | 操作 schema 版本；未知版本拒绝 |
| `kind` | 操作种类及输入 |
| `run_id` | 运行身份，与实际执行输入一致 |
| `context` | 主体、策略、作用域与执行器资源绑定 |
| `rules` | 文件、网络、环境和执行维度的 `OperationDecision` |
| `placements` | 由内到外排列的 VM／Overlay 放置 |

每项策略决定包含稳定 ID、能力维度、目标、动作及预期控制计划。资源绑定描述能力，不授予权限；预期控制计划不证明安装成功。`rules` 是决定的列表，不是可执行的 Rule／Rewrite 语言。

Operation 不记录环境变量值；程序参数和路径仍可能包含敏感信息，调用方应据实际载荷决定记录范围。它不是完整的进程环境快照。

## 改写与 Placement

pvisor 的准入过程选择执行器，应用实际兼容策略和权限约束，保留原始请求与有效操作两个快照。`PVisor::resolve_operation(RunSpec)` 返回有效操作，供执行前审查。

Requested 快照不含已选 Placement。发布事实时，pvisor 先比较不带 Placement 的请求与有效操作：有变化才发布 Rewritten，并保存 before／after。改写必须保持运行身份和操作种类，不能夹带 Placement 变化。

Placed 再保存包含有序放置列表的有效操作。宿主不使用 VM 或 Overlay 时，列表可以为空，仍会发布 Placed。这个事实描述选定的放置；实际控制是否安装，要看执行器观察。

## Event：外部观察到的事实

Event 信封和 Journal 当前使用版本 5。Event 的 `data` 是一个 Fact。

| Fact | 载荷及含义 |
| --- | --- |
| Context | 主体、策略与资源绑定 |
| Requested | 请求 Operation 的不可变快照 |
| Rewritten | 同一操作实际改写前后的快照 |
| Placed | 含最终放置的有效 Operation |
| Dispatched | 选定后端与运行身份，表示进入派发阶段 |
| Completed | Outcome 与结果来源 |
| Observation | 领域、名称、版本和 JSON 观察载荷 |

正常启动的操作事实链为：

```text
Context → Requested → [Rewritten] → Placed → Dispatched → Completed
```

链中还可以插入生命周期及领域观察。Dispatched 在调用执行器前提交，不代表进程已经成功启动。准入或准备失败可以没有完整操作事实链；外部调用方仍须处理 API 返回的错误。Completed 的来源区分 Backend、Policy、Replay 和 Runtime，运行器失败不能伪装成后端结果。

Outcome 的成功值包含终态和退出码；错误区分 Failed、Denied、Unsupported 和 Unknown。Unknown 保留已知效果，不能理解为“没有副作用”。

## 身份、因果与顺序

| 信封字段 | 作用 |
| --- | --- |
| `id` | 事件身份，用于关联与去重 |
| `trace_id` | 归属的执行记录；当前运行流按 Run 过滤 |
| `producer`、`observed_at_unix_ms` | 生产者与观察时间 |
| `scope` | 事件作用域；操作事实使用 Run／Attempt 范围 |
| `context`、`operation` | 上下文身份与操作身份；不是完整载荷 |
| `caused_by` | 已知前置事实的事件 ID |
| `level`、`granularity` | 展示和筛选信息 |

同一 Attempt 的操作事实使用相同操作身份并串联因果引用。Context 事实没有操作引用；领域 Observation 可以不带执行引用。Journal 校验事件与引用，提交位置表示落入该日志的顺序。

依赖前置结果的处理必须在前置结果确定后执行；不能因为换了 Placement 或转发路径就倒置这个依赖。独立并发操作没有由时间先后自动产生的因果关系。当前机制维护事实提交和已知依赖，不提供跨 Job 全局排序，也不拦截命令内部的每个 syscall。

## 提交、订阅与失败

pvisor 准备驱动后提交启动事实，成功后才调用执行器。必要事实提交失败会阻止派发，准备资源需要清理。执行后记录失败以 failure／warning 报告；记录失败不会撤销已经发生的效果。

Journal 使用单写入者和提交回执，支持事件去重及尾部恢复。提交结果未知时不能盲目重试外部副作用。`RunHandle` 的订阅提供本次 Run 的已提交事件，包括共享 Journal 中的 Gateway 观察；实时接收方还须处理断开与落后，完整历史从 Journal 读取。

关闭文件记录时仍可使用内存事件流，但不产生可供重启恢复的文件日志。结构化 Event 是事实格式，`Event::to_text` 只是人读投影。

## 观察与重建范围

`OperationObservation` 保存结果、规则计数和边界观察，并校验终态及计数一致性。`null` 表示无法观察，零表示观察后没有命中。FUSE 路径表最多保留 8192 项，溢出命中计入 `overflow_hits`；操作次数不替代最终 diff。网络计数仅覆盖经过拦截器的流量。

事件快照可以重建已观察到的请求、改写、放置及结果。完整恢复外部行为还需要初始文件状态、实际环境、外部输入及对应执行机制；当前 Trace 不包含这些全部信息。事件日志、文件检查点和 Agent 轨迹 replay 的恢复范围不能混为一谈。

当前 Run Bundle 为 schema 4；旧 Bundle 和旧 Event／Journal 格式拒绝读取，不静默混用。版本由代码中的常量维护，详细强制力口径见[能力与证据](../concepts/capabilities-and-evidence.md)。

## 代码与验证

| 代码 | 职责 |
| --- | --- |
| `pvisor_core::operation`、`pvisor_core::event` | 公共结构及校验 |
| `pvisor_core::execution` | 执行输入、能力计划与输出定义 |
| pvisor 的 `runtime/operation.rs` | 请求和实际边界的操作／观察构造 |
| pvisor 的 `runtime/event.rs`、`session/lifecycle.rs` | 事实发布、派发和收尾 |
| `pvisor-journal` | 提交、引用校验与恢复 |

```sh
just test core pvisor-journal
just test pvisor
```

Operation 契约测试覆盖身份、改写／放置分离、终态和版本拒绝。生产路径测试覆盖网络权限实际收窄后的事实链、因果引用，以及环境变量值不进入操作快照。
