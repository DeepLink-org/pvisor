# pVisor 核心：操作项、上下文链与改写

核心契约 v3。pVisor 把进入受控边界的一次操作表示为一个操作项，策略改写它的上下文链，
后端解释改写后的表达式，事件保留原请求、改写依据和实际结果。

```text
fs.read("file-17", offset: 0, length: 4096)
  |> vm("sandbox")
  |> remote("node-a")
```

操作项固定“要做什么”，后缀说明“如何处理这个计算”。原请求始终保留，派生表达式记录
策略作出的选择。具体 Rust 接口、文本格式与命令见 [IR 实现](pvisor-ir.md)。

## 1. 操作项与结果

用 Haskell 风格记法说明类型，以下是语义定义而非另一门运行时语言：

```haskell
data Op a where
  ReadFile  :: FileRef -> Offset -> Length -> Op Bytes
  WriteFile :: FileRef -> Offset -> Bytes  -> Op Word64

data Outcome a = Success a | Error Failure
```

`FileRef` 是逻辑资源引用。运行器提供固定的主体、scope、策略版本和能力绑定，后端把引用
对应到实际资源、代次与权限。文本中的路径或资源名本身不授予访问能力。

每个原语固定参数、结果类型、允许的效果及错误语义：

| 原语 | 成功与效果契约 |
|---|---|
| ReadFile | offset 与 length 为 u64，范围不溢出；返回不超过 length 的字节，允许 EOF 和短读，不修改文件内容 |
| WriteFile | offset 为 u64，范围不溢出；返回不超过输入长度的写入量，允许短写；不依赖共享游标，不隐含 fsync |

零长度操作不修改文件内容。后端必须检查资源代次与实际权限，保留错误时已确认的部分
效果；短 IO、存储一致性和宿主级副作用由对应后端契约说明。

失败分为 `Failed(domain, code, effects)`、`Denied(reason)`、`Unsupported(reason)` 和
`Unknown(reason, known_effects)`。Denied 是策略拒绝，Unsupported 是能力或组合不受支持，
Unknown 保留已知信息与不确定性。一个错误结果不会自动触发重试，也不能抹去已发生的效果。

## 2. 表达式与包裹

```haskell
data Layer a
  = VM VMRef
  | Remote NodeRef
  | Overlay OverlayRef
  | Mock a
  | Deny Reason

-- 后缀按内层到外层存储。
data Expression a = Expression (Op a) [Layer a]
```

管道借用“将左值放入右侧调用第一个参数”的规则：

```text
x |> f(a) = f(x, a)
```

因此 `read |> vm(A) |> remote(B)` 的嵌套关系是 `remote(vm(read, A), B)`。
构造表达式只生成数据；解释器开始解释后才可能执行副作用。外层先接管计算，内层随后被解释。

在组合后表达式有效的范围内，上下文链遵守以下结构规律：

```text
wrap(e, [])          = e
wrap(wrap(e, a), b)  = wrap(e, a ++ b)
operation(wrap(e,a)) = operation(e)
```

这些是结构规律，不能据此交换上下文或改变副作用顺序。
`read |> vm(A) |> remote(B)` 与 `read |> remote(B) |> vm(A)` 是不同的计算：前者在 B 的
VM A 中读取；后者在 VM A 内发起到 B 的读取。后端不支持某种嵌套时必须返回 Unsupported，
不能只保留一个“最终后端”而忽略其余层。

Mock 与 Deny 是接管整个内部计算的处理器。本版将它们限定在最外层：

```text
fs.read("file-17", offset: 0, length: 5) |> vm("sandbox") |> mock(bytes([104,105]))
```

这个计算直接得到 `hi`，内部 VM 和读取均不执行。`mock(...) |> vm(...)` 被拒绝，直到有
明确的需求与契约定义“在 VM 内执行模拟处理器”。Mock 的值仍须满足原语结果类型与范围。
Maybe、重试及其他改变结果含义的处理器，应先明确错误映射和效果契约，再加入核心。

## 3. 策略改写

规则由身份、版本、匹配条件及改写动作组成。当前匹配条件为原语种类、可选资源引用、
可选完整后缀；参数保持原语自身的类型。匹配针对当前派生表达式进行，原请求保持不变。

```text
Append(contexts)       -- 在现有后缀外继续包裹
SetContexts(contexts)  -- 替换整个后缀，包括删去包裹层
Replace(expression)   -- 明确替换操作参数或资源，保持原语契约
```

Append 与 SetContexts 保证操作项不变。Replace 单独标明非常规操作替换，并记录原请求
与派生表达式的关系。多操作展开、通用子树模式和自动重排尚未加入这版核心。

规则组织为有限、有序的 pass。每个 pass 在当前表达式上选取第一条匹配规则，执行一次
改写，再进入下一 pass；新表达式不回到此前 pass。没有匹配则原样进入下一 pass。
这给出确定的有限推导：

```text
request = e0
          -- rule_1 --> e1
          -- rule_2 --> e2
          -- ...    --> en
result = interpret(en)
```

每一步保存完整规则与前后表达式，因此可以核对 `apply(rule, before) == after`。
上下文修改没有隐式执行，也不会重新执行原操作。最终结果同时经过派生操作与原操作的契约
检查；例如将读取长度扩大后，不能直接把超出原请求长度的结果交给调用者。

## 4. 后端与两层副作用

Agent 接口层的副作用是 ReadFile、WriteFile 等操作。pVisor 实现层的副作用是授权检查、
远端连接、VM 暂停与恢复、资源释放及审计提交。内部动作由后端实现并记录观察，不必变成
Agent 的操作项。

Rust 后端边界为：

```text
authorize(trusted_context, expression) -> allowed | Failure
execute(trusted_context, expression, execution_context) -> Outcome
```

后端收到完整、有序的表达式，负责解析绑定和处理每一层。引入后端时，给出支持的原语、
上下文组合、资源代次检查、错误效果及取消收尾契约；改写机制保持不变。
例如 `read |> remote(B)` 可以由远程后端完成网络读取，而 `read |> vm(A) |> remote(B)`
需要能在 B 的 A 中解释剩余计算的后端。支持前者不自动意味着支持后者。

若某个 VM 后端要求远程读取期间暂停 Guest，其内部顺序应为确认暂停、读取、准备响应、
释放本次暂停、交付。暂停必须有实际确认与所有权，释放不得取消别人的暂停；断连或取消时
须保存已知状态，无法确认完成的远程操作返回 Unknown。此生命周期属于该后端的实现契约，
当前核心不假定它已经具备。

准入与执行顺序为：

1. 校验操作、上下文和规则定义，记录原始请求。
2. 检查请求准入；每次候选改写通过授权后再记录和采用。
3. 若最外层是 mock/deny，直接产生对应结果；否则由后端检查完整表达式并执行。
4. 检查派生及原请求结果契约，执行披露检查，记录完成事实。

请求携带的后缀也经过准入，不能通过自行添加 mock、remote 等层绕过授权。结果披露被拒绝
时，已经观察到的执行结果仍记录在内部事件中，后续拒绝不等于此前没有效果。

## 5. Event、日志与回放

事件使用 [Event v3 契约](event-contract-v3.md)，区别三类信息：原请求是什么、如何推导
执行表达式、实际观察到了什么。operation ID 连接同一请求的事实，caused_by 连接实际因果；
scope 表示归属，时间和 journal 位置不替代因果。

```text
op=17 requested  fs.read("file-17", offset: 0, length: 4096)
op=17 rewritten  [...] => [fs.read("file-17", offset: 0, length: 4096) |> remote("node-a")]
op=17 dispatched backend="router" fs.read("file-17", offset: 0, length: 4096) |> remote("node-a")
op=17 completed  ... => ok(bytes([...]))
```

存储使用类型化记录，管道文本只是人读投影。回放可以先从 Requested 开始逐步核对规则，
重建最终表达式，再核对记录的结果。重放真实副作用还需要初始状态、资源与版本、完整输入
和必要顺序；日志本身不产生这些前提，也不应直接导致写入操作被重新执行。

取消不会自动生成“失败且无效果”的结论。派发事实没有完成事实时，结果仍待确认。
执行后日志失败也不能覆盖已知结果，调用方须检查 `Execution.audit_errors`。

## 6. 实现与验证

核心代码位于 `persisting-control::ir`、`persisting-control::trace`、
`persisting-pvisor::core` 和 `persisting-pvisor::trace`。当前执行原语仅为文件范围读写；
VM、HTTP、网络与模型观察可先使用领域事件，执行原语按模块契约逐个加入。
现有 FUSE、Gateway 等入口尚未迁移，新核心不会自动获得未接入操作的覆盖。

验证分为两层：

- [有限代数检查](pvisor-algebra-check.py)：包裹恒等/结合、顺序敏感、原请求保持及改写复原。
- Rust 契约与性质测试：实际解析往返、后缀改写、规则证据、授权、结果检查、取消、审计缺口
  和 journal 持久化/恢复。测试后端验证包裹顺序；真实 VM/远端驱动的正确性须由驱动测试建立。

这版不承诺普遍可逆、任意重排、完整 Agent 状态复现或性能提升。核心先固定小而可检查的
请求、改写与观测边界，再以同一契约逐个接入真实后端。
