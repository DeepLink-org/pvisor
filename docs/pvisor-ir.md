# pVisor IR v3 与 Trace v3

IR 表示一个尚未执行的操作及其上下文链。适配器把截获的调用构造成表达式，策略改写
后缀，后端执行整个表达式，trace 保存请求、推导过程与结果。

```text
fs.read("input", offset: 0, length: 5) |> vm("sandbox") |> remote("node-a")
```

这表示在 node-a 的 sandbox 中读取 input。后缀按内层到外层排列，等价于
`remote(vm(read, "sandbox"), "node-a")`；构造或改写表达式不会触发读取。

[核心契约](pvisor-algebra.md) 定义语义，[Event 契约](event-contract-v3.md) 定义记录。
本页说明当前实现和可运行入口。

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

- 操作：`fs.read(file, offset: u64, length: u64)`、`fs.write(file, offset: u64, data: bytes([...]))`。
- 包裹：`vm(name)`、`remote(name)`、`overlay(name)`、`mock(value)`、`deny(reason)`。
- 值：字节数组或 u64；字符串使用 JSON 转义，字节范围为 0–255。
- 命名实参顺序任意；规范输出固定顺序，重复、缺失或未知实参报错。
- 允许空白、换行及 `//` 注释；规范输出为单行表达式，不保留注释。
- 最多 32 层上下文，文本与结构化表达式各限 1 MiB；文件范围不允许溢出。
- mock/deny 只能位于最外层，直接返回结果，内部操作与上下文不执行。mock 必须符合原语结果契约。

文本是一个表达式，不含版本头、函数、变量绑定或控制流。结构化 JSON 的 `version` 为 3；
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

规则组织成有限的有序 pass，每个 pass 仅应用第一条匹配规则一次，再进入下一 pass。
每一步作用于当前派生表达式，Requested 中的原请求始终保留。最多 32 个 pass、合计 1024 条规则。

`Rule::apply` 是纯结构改写；规则不匹配、类型不兼容或产生无效上下文链都会报错。
`Fact::Rewritten` 保存完整规则、pass、before 和 after，校验时重新应用规则核对结果。

## 执行与事件

核心入口是 `Engine::run(&Context, &Expression)`：

```text
Context → Requested → Rewritten* → Dispatched? → Completed
```

准入检查覆盖原请求及自带后缀，每一步改写也单独授权。最外层 mock/deny 由核心处理，
其余表达式交给后端。`Backend::authorize` 检查完整上下文链与资源能力；`execute` 解释
全部层并返回 Outcome。不支持的嵌套必须明确拒绝，不能丢掉其中的层。

运行结果包含最终表达式、Outcome、operation ID 和 `audit_errors`。成功值必须同时满足
原请求与派生操作的结果契约。执行前必要日志失败会阻止后续派发；执行后的审计失败通过
`audit_errors` 返回，已知结果不会被覆盖。取消期间未记录 Completed 的操作仍待确认。

后端内部动作使用 `ExecutionContext` 关联 Observation，可记录 VM 暂停、网络传输等领域事件。
当前文件原语的结果为 `Bytes` 和 `U64`，HTTP、网络、VM、模型等执行原语按各模块契约继续加入。

## 可运行示例

[read.pv](../crates/persisting-control/examples/read.pv) 是一个单独的读取请求。
[core_trace.rs](../crates/persisting-pvisor/examples/core_trace.rs) 将它绑定到示例拥有的临时文件，
先真实读取 `hello`，再为同一个请求追加 mock，得到 `mock`；第二次不调用文件后端。

```sh
cargo run --locked -p persisting-pvisor --bin pvisor -- ir check crates/persisting-control/examples/read.pv
cargo run --locked -p persisting-pvisor --bin pvisor -- ir format crates/persisting-control/examples/read.pv
cargo run --locked -p persisting-pvisor --bin pvisor -- ir json crates/persisting-control/examples/read.pv

cargo run --locked -p persisting-pvisor --example core_trace -- /tmp/read.trace.jsonl
cargo run --locked -p persisting-pvisor --bin pvisor -- trace check /tmp/read.trace.jsonl
cargo run --locked -p persisting-pvisor --bin pvisor -- trace show /tmp/read.trace.jsonl
cargo run --locked -p persisting-pvisor --bin pvisor -- trace json /tmp/read.trace.jsonl
```

`ir` 子命令接受文本或 JSON。`trace` 子命令只读 journal，要求写入句柄已关闭。
`trace show` 展示操作优先的管道行；`trace json` 保留完整信封、规则与结构化结果。
`trace check` 检查单事件、位置、身份和已知因果环，同时报告尚未解析的因果引用。

## 实现位置与验证

| 模块 | 职责 |
|---|---|
| `persisting_control::ir` | 操作、包裹、规则、契约及文本编解码 |
| `persisting_control::trace` | 公共事件、结构校验与可读投影 |
| `persisting_pvisor::core` | 有限改写、授权、后端派发和完成记录 |
| `persisting_pvisor::trace` | 单写入者 journal、提交回执及恢复 |

测试覆盖 Unicode/任意文本解析、JSON 往返、上下文顺序、原请求保持、规则证据、mock/deny
短路、权限边界、结果约束、取消和日志恢复。模型后端测试证明传入的包裹顺序被保留，
文件描述符示例验证真实读取；VM、远程和 OverlayFS 驱动仍需逐个接入和验证。

```sh
just test persisting-control
just test persisting-pvisor
python3 docs/pvisor-algebra-check.py
```

现有 `persisting_control::events` v1 及 FUSE/Gateway 等生产入口尚未迁移。
新 IR/Trace 使用 v3，旧草稿程序和 journal 不混读。回放测试重建改写过程并核对记录结果；
真实副作用回放须由后续适配器提供资源初态与必要输入。
