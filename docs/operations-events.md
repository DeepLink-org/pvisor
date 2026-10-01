# Operation 与 Event

pVisor 的核心处理对象是 Operation。`pvisor-core` 定义操作、策略决定、Placement、结果和对外交互；`pvisor` 实现准入、调度、驱动准备和执行；Journal 持久化 Event。

## Operation

`pvisor_core::Operation` 使用 schema 1。目前唯一生产操作是 `OperationKind::RunExecute`（`run.execute`），包含程序、参数和工作目录。不引入文本语言、通用规则解释器或虚构的文件操作。

| 字段 | 含义 |
|---|---|
| `kind` | 操作及其输入；不记录环境变量值 |
| `run_id` | 运行身份，与执行输入一致 |
| `context` | 主体、策略与资源绑定；绑定不证明权限 |
| `rules` | 文件、网络、环境及执行的有效策略决定与预期控制 |
| `placements` | 由内到外排列的 VM／Overlay 放置 |

`PVisor::resolve_operation(RunSpec)` 使用实际准入路径，返回有效操作供执行前审查。调度器保留原始请求快照和准入后的快照；真实策略变化产生 Rewritten，实际放置另产生 Placed。执行器消费有效 RunSpec 和驱动附件，Operation 本身不会执行或授予权限。

## Event 与 Trace

Event 是外部观察 pVisor 的事实格式，Trace 是这些事件组成的记录。Event 与 Journal 使用版本 5，旧版本拒绝读取。信封包含稳定 ID、trace_id、producer、时间、scope、context、operation 和 caused_by。

```text
RunSpec → pvisor 准入／策略改写／Placement → Session → RunExecutor::execute → 收尾
            Context → Requested → [Rewritten] → Placed → Dispatched → Completed
```

| Fact | 描述 |
|---|---|
| Context | 主体、策略和资源绑定 |
| Requested | 原始 Operation 快照 |
| Rewritten | 同一操作真实改写前后的快照；不承载 Placement 变化 |
| Placed | 包含实际放置的有效 Operation |
| Dispatched | 后端名称与运行身份 |
| Completed | 终态结果与来源；区分 Backend、Policy、Replay、Runtime |
| Observation | 领域观察及版本化载荷 |

同一 Attempt 的执行事实使用同一操作身份并以 caused_by 串联。必要的执行前事实写入失败会阻止派发；执行后审计失败单独报告，不能把已经发生的副作用解释为没有发生。失败或拒绝不保证有 Dispatched。

事件可以重建已观察到的请求、改写、放置及结果；它们不包含恢复全部外部状态所需的输入，不能承诺确定性重放。当前操作粒度是整个命令，不能据此声称每个 syscall 都被中介，或不同 Job 有全局副作用顺序。保持已知因果依赖，不用时间戳推导跨进程因果，也不把事件记录顺序冒充外部副作用顺序。

## 观察和实现边界

`OperationObservation` 校验终态、规则覆盖和计数一致性。`null` 表示无法观察，零表示已观察且没有命中。FUSE 路径表最多保留 8192 项，其他命中计入 overflow_hits；操作次数不替代最终 diff。网络统计只覆盖经过拦截器的流量。

准入时的 `ExecutorPlan` 最高为 Planned；执行器返回的 `ExecutorObservations` 才能证明已安装的控制。Bundle schema 4 从观察集派生安全摘要。策略纯校验和公共协议留在 core；AgentCtl 客户端、权限询问 socket、生命周期和调度位于 pvisor；实际文件／网络拦截位于各驱动。

| 模块 | 职责 |
|---|---|
| `pvisor_core::operation` | 操作、放置、结果与观察契约 |
| `pvisor_core::event` | 事实、信封校验与人读投影 |
| `pvisor_core::execution` | 执行输入、能力计划及输出定义 |
| `pvisor::runtime::operation` | 从实际输入构造操作与观察 |
| `pvisor::session` | Attempt 生命周期和执行结果公布 |
| `pvisor_journal` | 提交回执、去重、因果引用与恢复 |

```sh
just test pvisor-core pvisor-journal
just test pvisor
```

[契约测试](../crates/pvisor-core/tests/operation_contracts.rs)验证身份、改写与放置约束、终态、JSON 往返和版本拒绝。生产路径测试验证真实网络策略收窄的事件链及凭据值不进入操作快照。
