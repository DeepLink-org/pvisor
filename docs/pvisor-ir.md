# pVisor IR v4 与 Trace v3

IR 表示操作及其上下文链，是数据与证据契约。生产 Run 由 `RunSpec` 编译成
`RunPlan`，记录 VM/Overlay 放置、策略规则及执行结果；它不是任意表达式的执行入口。

```text
fs.read("input", offset: 0, length: 5) |> vm("sandbox") |> remote("node-a")
```

这表示在 node-a 的 sandbox 中读取 input。后缀按内层到外层排列，等价于
`remote(vm(read, "sandbox"), "node-a")`；构造或改写表达式不会触发读取。

[代数规范](pvisor-algebra.md) 定义结构语义，[Event 契约](event-contract-v3.md) 定义记录。
本页说明当前实现和生产接口。IR 在运行路径中用于计划表达与副作用归集，不提供独立的 `pvisor ir` 子命令。

## 数据结构与文本

```rust
pub struct Expression {
    pub version: u16,
    pub operation: Operation,
    pub contexts: Vec<Layer>,
}
```

`Operation` 固定实参及结果契约，`contexts` 保存有序的包裹层。当前文本语法为：

```text
fs.read("input", offset: 0, length: 5)
fs.write("output", offset: 0, data: bytes([104,101,108,108,111]))

fs.read("input", offset: 0, length: 5) |> overlay("workspace")
fs.read("input", offset: 0, length: 5) |> remote("node-a") |> mock(bytes([104,105]))
fs.write("output", offset: 0, data: bytes([104,105])) |> deny("read-only")
```

- 操作：`fs.read(file, offset: u64, length: u64)`、`fs.write(file, offset: u64, data: bytes([...]))`、`run.execute(run_id)`。
- 包裹：`vm(name)`、`remote(name)`、`overlay(name)`、`mock(value)`、`deny(reason)`。
- 值：字节数组、u64 或 Run 结果（state、exit_code）；字符串使用 JSON 转义，字节范围为 0–255。
- 命名实参顺序任意；规范输出固定顺序，重复、缺失或未知实参报错。
- 允许空白、换行及 `//` 注释；规范输出为单行表达式，不保留注释。
- 最多 32 层上下文，文本与结构化表达式各限 1 MiB；文件范围不允许溢出。
- mock/deny 只能位于最外层，mock 必须符合原语结果契约。这是结构校验，不表示生产入口支持执行这些处理器。

文本是一个表达式，不含版本头、函数、变量绑定或控制流。结构化 JSON 的 `version` 为 4；
`Expression::from_str`、`to_text` 和 serde JSON 表示可往返相同结构。
多个实际调用分别产生请求，通过事件身份与因果引用连接。

执行身份与能力绑定使用独立的 `Context`：principal、scope、policy、revision 以及资源绑定。
例如 `input` 可绑定一个已打开的文件句柄对应的资源与代次。这些信息由可信运行器提供，
表达式中的 `remote("node-a")` 本身不授予网络或远端权限。

## 改写

`Rule` 保存 id、version、Pattern 与 Rewrite。Pattern 匹配操作种类，并可限定资源及完整后缀。
Rewrite 有三种动作：

| 动作 | 用途 |
|---|---|
| Append | 保持操作不变，向后缀追加外层上下文 |
| SetContexts | 保持操作不变，替换整个后缀；空列表表示移除后缀 |
| Replace | 明确替换表达式，可改变资源或实参，保持原语种类 |

例如对同一个原始请求应用 Append：

```text
before: fs.read("input", offset: 0, length: 5)
after:  fs.read("input", offset: 0, length: 5) |> remote("node-a")
```

生产计划只按顺序追加由运行器生成的 VM/Overlay 放置层，不接受调用者提供的规则或后缀。
Requested 中的原请求始终保留。通用有序 pass 属于代数规范，不存在第二个生产改写执行器。

`Rule::apply` 是纯结构改写；规则不匹配、类型不兼容或产生无效上下文链都会报错。
`Fact::Rewritten` 保存完整规则、pass、before 和 after，校验时重新应用规则核对结果。

## 执行与事件

唯一生产派发入口是 `PVisor::run(RunSpec)`，执行器实现 `RunExecutor`：

```text
RunSpec → resolve_run → RunPlan → prepare → RunExecutor::execute → teardown
                          Context → Requested → Rewritten* → Dispatched → Completed
```

`resolve_run` 校验 RunSpec，选择支持 invocation 的执行器，合并应用及网络策略，
检查所需执行边界，再编译计划。`PVisor::resolve_run_plan` 使用相同解析路径，供启动前审查。
计划中的上下文只描述可信运行器已选择的 VM/Overlay 放置，不授予能力。
执行器消费 AttemptContext 中的 RunSpec 与实际控制附件，不解释任意 Expression。

执行前必要事实提交失败会阻止执行器运行。完成时 RunObservation 在生产构建中校验计划、
原请求与派生操作的结果契约及计数一致性；无效观察导致 Run 失败，并以 Unknown 保存已知结果。
执行后日志失败按 RunResult 的 failure/warnings 报告，不自动重试副作用。

不存在独立的 Engine、Backend 或 Admission API。一般文件表达式执行、调用者自带后缀、
每次候选改写授权、mock/deny 短路和通用披露检查均未接入生产，代数规范不证明这些已被实施。
文件系统与网络控制由实际执行器、FUSE 和 OverlayNet 边界实施。

IR 文本或 JSON 可通过 `persisting_control::ir::Expression` 解析和校验，但不会执行操作。
关闭写入句柄后可用 `persisting_journal::Journal::read` 校验事实日志；
`Event::to_text` 提供人读投影。

## 实现位置与验证

| 模块 | 职责 |
|---|---|
| `persisting_control::ir` | 操作、包裹、规则、契约及文本编解码 |
| `persisting_control::trace` | 公共事件、结构校验与可读投影 |
| `persisting_pvisor::runtime` | RunSpec 准入、计划编译、执行器派发与收尾 |
| `persisting_pvisor::trace` | 单写入者 journal、提交回执及恢复 |

测试覆盖解析往返、结构改写与规则证据，以及生产 Run 的准入拒绝、执行事实链、
结果检查和 Journal 恢复。代数检查只验证结构规律，不代表通用表达式授权或真实驱动的执行证明。

```sh
just test persisting-control
just test persisting-pvisor
python3 docs/pvisor-algebra-check.py
```

生产 Run 入口已编译 RunPlan，并将请求、实际计划改写、派发和完成写成独立 Trace v3 事实。
Gateway 捕获和 replay 使用同一 Event 信封及 Journal；文件系统与网络汇总使用领域 Observation，
不宣称每个 FUSE 操作或网络包都有逐条 IR 执行事实。EventRecord 与命令 WAL 已退场，不提供历史读取兼容。
正式日志与旧 JSONL 不混写；参见 [事件契约](event-contract-v3.md) 的接入与迁移说明。
IR 使用 v4（增加 `run.execute`），Trace/Event 使用 v3；旧草稿程序和 journal 不混读。回放测试重建改写过程并核对记录结果；
真实副作用回放须由后续适配器提供资源初态与必要输入。
