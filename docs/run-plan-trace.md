# RunPlan 与 Trace

生产执行只有一条路径：`PVisor::run(RunSpec) → Session → RunExecutor::execute`。
`RunPlan` 是有效 RunSpec 的不可变审计投影，描述已选放置与计划控制；执行器消费
RunSpec 和实际驱动附件。计划本身不执行操作，也不授予权限。

## RunPlan

`pvisor_control::run_plan::RunPlan` 使用 schema 3，由准入路径直接构造。
`PVisor::resolve_run_plan` 使用相同路径，供启动前审查。

| 字段 | 含义 |
|---|---|
| `version` | 当前为 3，未知版本拒绝 |
| `run_id` | Job 的运行身份，准备驱动时必须与 RunSpec 一致 |
| `context` | 主体、策略、scope 和执行器资源绑定；绑定描述能力，不证明权限 |
| `placements` | 按由内到外顺序保存 VM、Overlay 放置；宿主无暂存时为空 |
| `rules` | 文件、网络、环境及 `run.execute` 的有效决定和计划控制 |

每个 PlanRule 有唯一 ID、能力维度、目标、动作和 `EnforcementPlan`。
计划控制的最高等级是 Planned。执行器收尾返回的 `ExecutorObservations` 才能说明
已安装的强制控制；Bundle schema 3 的安全摘要从观察集计算。

放置直接加入列表，不经过策略重写。不存在文本语法、任意操作表达式、文件读写原语、
Mock/Deny 处理器或 Rule/Rewrite 解释器。

## Trace

公共事件与 Journal 使用版本 4，旧版本拒绝读取，不混写。
事件信封保留稳定 ID、trace_id、producer、观察时间、scope、context、operation、
caused_by、严重程度和粒度。时间不用于推导跨进程因果；scope 不授予权限。

```text
RunSpec → 准入与 RunPlan → 准备驱动 → RunExecutor::execute → 收尾与提交
                          Context → Requested → Dispatched → Completed
```

| Fact | 载荷 |
|---|---|
| Context | 固定的主体、策略和资源绑定，使用 Attempt scope |
| Requested | 完整的不可变 RunPlan |
| Dispatched | 后端名称与 run_id |
| Completed | run_id、Outcome 和实际结果来源 |
| Observation | 领域、名称、版本和 JSON 载荷 |

执行事实共享 context、operation 与 scope，通过 caused_by 关联前置事实。
Context 事实不带 operation；外部领域 Observation 可以不带执行引用。
准入或启动失败可能没有 Dispatched。Completed 的来源区分 Backend、Policy、Replay、Runtime；
运行器失败不能伪装成后端结果。

Completed 的成功值只能是包含终态与退出码的 Run 结果。失败保留 Failed、Denied、
Unsupported 或 Unknown；Unknown 携带已知效果，不能被解释为没有副作用。
`Event::to_text` 是结构化事件的人读投影，不是可解析或可执行的语言。

## 观察与持久化

RunObservation 校验结果终态、计划规则覆盖和文件系统计数一致性。
规则计数的 `null` 表示边界不能观测，零表示已观测且没有匹配操作。
文件系统路径表最多保存 8192 个路径，溢出操作计入 overflow_hits。
成功的变更效果不能超过成功次数，不确定效果不能超过失败次数；观察不替代最终 diff。
网络汇总只描述经过拦截器的流量，不能证明没有绕过代理的连接。

Run Bundle 保留计划和观察；run.json 保存运行状态与执行器选择身份，不持久化强制力声明。
文件及网络授权仍由 Control、执行器、OverlayFS、OverlayNet 的实际边界落实。

Journal 保留单写入者、提交回执、事件去重、因果引用和尾部恢复语义。
写入失败区分确定拒绝与结果未知；未知提交不能盲目重试副作用。
必要的执行前事实写入失败会阻止派发；执行后审计失败通过 failure/warnings 报告。
关闭写入句柄后，`pvisor_journal::Journal::read` 可以验证日志。

## 实现与验证

| 模块 | 职责 |
|---|---|
| `pvisor_control::run_plan` | 计划、放置、结果及观察 schema 与校验 |
| `pvisor_control::trace` | 公共事实、信封校验与人读投影 |
| `pvisor::runtime::plan` | 有效 RunSpec 的计划与观察投影 |
| `pvisor_journal` | 日志提交、去重及恢复 |

```sh
just test pvisor-control pvisor-journal
just test pvisor
```

[RunPlan 契约测试](../crates/pvisor-control/tests/run_plan_contracts.rs) 覆盖 JSON 往返、
放置顺序、终态约束、规则覆盖和旧 schema 拒绝。
生产 Run 测试验证请求、派发、完成的因果事实链；真实隔离由各驱动的回归测试验证。
