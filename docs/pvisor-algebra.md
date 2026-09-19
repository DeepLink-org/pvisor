# pVisor 核心：操作、组合与改写

设计草案 v0.8。本文使用 Haskell 风格的类型和等式描述核心语义，供后续实现与契约测试使用。

pVisor 在 Agent 与执行环境之间建立操作边界。文件读写、网络请求和模型调用进入这个边界后，
被表示为有类型的操作。策略决定如何改写操作，后端完成实际执行，Event 保存执行经过。

一次文件读取可以由本地文件、远程存储或 Overlay 提供。Agent 使用同一个读取接口；
pVisor 根据上下文选择资源和执行方式，并将结果交还给 Agent。

整个模型围绕两层副作用展开：

| 层次 | 描述的行为 | 文件读取示例 |
|---|---|---|
| Agent 接口层 | Agent 提出的操作，以及接口承诺的结果和资源变化 | 读取指定范围，得到字节、EOF 或错误 |
| pVisor 实现层 | 完成操作所需的内部动作 | 检查权限、远程取数、协调 VM、记录审计 |

`Program` 组合 Agent 接口层的操作。后端用自己的执行流程实现这些操作。
两层通过操作契约连接：后端交付接口规定的结果，同时遵守内部资源和执行顺序要求。

## 1. 操作与结果

`Op e a` 表示一个待执行操作，成功结果的类型为 `a`，领域错误的类型为 `e`。
操作的参数包含在构造子中：

```haskell
data Op e a where
  ReadFile    :: FileRef -> Offset -> Length -> Op FsError Bytes
  WriteFile   :: FileRef -> Offset -> Bytes -> Op FsError WriteResult
  RequestHttp :: HttpRequest -> Op HttpError HttpResponse
```

例如，`ReadFile file offset count` 表示从文件的指定偏移读取至多 count 个字节。
`FileRef` 由 pVisor 绑定到提供方、资源身份与有效代次；Agent 持有接口所需的资源引用。
网络、时间、随机数和模型调用采用相同方式定义，形成按平台和版本维护的操作目录。

操作通过 `Outcome` 交付结果：

```haskell
data Failure e
  = Failed e EffectStatus
  | Denied DenialReason
  | Unsupported SupportReason
  | Unknown Uncertainty

type Outcome e a = Either (Failure e) a
```

`Right a` 表示成功。`Failed` 保留领域错误及已知的部分效果；`Denied` 表示策略拒绝；
`Unsupported` 表示执行环境不支持该契约；`Unknown` 表示执行结果仍待确认。
例如，远端写入后连接中断时，后端可能返回 Unknown，由后续查询确定写入状态。

每个原语的契约固定以下内容：

| 内容 | 定义的问题 |
|---|---|
| 输入与资源 | 接受哪些参数，操作哪个资源，如何识别其生命周期 |
| 效果与结果 | 成功后返回什么，哪些资源状态发生变化 |
| 错误与取消 | 如何表达部分完成、未知结果，以及取消后的处理 |
| 执行约束 | 在哪些位置授权，哪些步骤需要满足先后关系 |
| 观察与重放 | 记录哪些事实，复现时需要哪些输入和内容 |

pVisor 的覆盖范围由实际接入的操作入口确定。系统调用、代理协议、文件系统接口等入口
分别声明覆盖范围，包含 mmap、异步 I/O 等路径的处理方式。外部程序在这些入口交出请求。

## 2. 计算与顺序组合

`Program a` 表示由操作组成、最终返回 a 的计算。`perform` 将一次操作放入计算中；
`(>>=)` 将它的结果传给后续计算：

```haskell
perform :: Op e a -> Program (Outcome e a)

pure  :: a -> Program a
(>>=) :: Program a -> (a -> Program b) -> Program b
```

这对应过程式程序中“执行一步，取结果，再决定下一步”的结构。
Haskell 的 do 记法提供这种组合的顺序写法；各步之间的数据依赖保留在表达式中。

Program 遵循 Monad 三律：

```haskell
pure x >>= f    = f x
p >>= pure      = p
(p >>= f) >>= g = p >>= (\x -> f x >>= g)
```

这些等式用于整理和组合计算，保持原有执行次序。附录给出计算树和组合定义，
可对有限、良类型的计算按结构归纳验证这些等式。

## 3. 错误与上下文

常用的错误处理是“成功继续，错误短路”。它由 ExceptT 表达：

```haskell
type Action e a = ExceptT (Failure e) Program a

call :: Op e a -> Action e a
call = ExceptT . perform

update file offset count transform = do
  bytes <- call (ReadFile file offset count)
  call (WriteFile file offset (transform bytes))
```

`transform` 是纯函数。读取成功后，写入使用转换后的内容；读取失败时，错误直接返回。
已经完成的效果和事件保留在执行历史中。跨领域组合通过显式错误映射保留原因及部分结果，
恢复和重试作为后续操作表达。

```haskell
throwError err >>= f = throwError err
catchError (pure x) h = pure x
catchError (throwError err) h = h err
```

Context 是不可变的执行配置，包含身份与版本、主体、资源绑定、策略和记录要求。
`ask` 取得配置，`local` 在一段计算中使用修改后的配置：

```haskell
ask   :: Program Context
local :: (Context -> Context) -> Program a -> Program a

local id p = p
local f (local g p) = local (g . f) p
local f (pure x) = pure x
local f (p >>= k) = local f p >>= (\x -> local f (k x))
```

这些等式适用于纯配置变换和有效的资源绑定。退出 local 后，后续计算使用父配置。
子配置沿用主体的权限边界；资源绑定由执行端验证。

绑定一个已有 Overlay 属于配置变换。创建 Overlay 则属于后端的资源获取过程，具有
独立的创建与释放时机。一个 Overlay 中的写后读共享同一个 upper；分别创建的 Overlay
各自维护状态。资源获取、使用和收尾采用 bracket 的组织方式，取消与故障由执行端处理。

## 4. 策略改写

策略将操作替换成另一段具有相同结果类型的计算：

```haskell
data Rule = Rule
  { ruleRef :: RuleRef
  , rewrite :: forall e a.
       Context -> Op e a -> Maybe (Program (Outcome e a))
  }
```

Nothing 表示规则不适用，Just p 表示使用 p 产生此次请求的结果。
替代表达式可以直接应答、访问另一个资源，或组合多个操作。
规则匹配使用纯计算；依赖的外部事实由受控执行路径取得并记录。

每轮按顺序选取首条匹配规则，未匹配的操作保留原样。本轮生成的表达式从下一轮或执行阶段继续；
需要多轮改写时，配置明确的有限轮次。规则有界求值，后续调用按其结果逐次展开，
执行阶段随上下文和远端委托传递。

原请求先经过准入检查，命中的规则负责提供替代结果。实际子操作分别授权，使用独立身份
关联原请求。分派前验证最终目标、参数和强制要求；目标或参数变化后重新验证。
例如，LLM 调用展开为 HTTP 后，对最终出站内容执行脱敏检查。

### 4.1 文件读取示例

对于 `ReadFile file offset count`，三种典型替换为：

```haskell
pure (Right bytes)                         -- 返回模拟内容
pure (Left (Denied reason))                -- 拒绝读取
perform (ReadFile remote offset count)     -- 读取绑定的远程资源
```

模拟结果遵守此次请求的长度等约束，审计标明其合成来源。前两种替换由规则直接完成应答，
第三种替换执行实际文件访问。模拟完整文件视图时，写入与后续读取共享相应的模拟状态。

远程读取由所选后端实现。若 VM backend 需要暂停 Guest 来完成响应，其过程为：

```text
Agent：ReadFile ─────────────────────────────→ Outcome FsError Bytes
pVisor：确认暂停 → 远程读取 → 准备响应 → 释放本次暂停 → 交付
```

暂停与读取由 Guest 之外的宿主控制路径推进。该后端在成功时先准备响应，再释放自己
持有的暂停原因；其他暂停原因继续有效。读取失败或取消时保留暂停并交接恢复流程。
暂停状态待确认时等待协调结果，实际恢复以控制端确认为准。

控制端负责获取暂停与登记收尾义务之间的取消处理，以及崩溃后的恢复。
原操作结果与收尾结果分别记录。Agent 接口呈现一次读取，内部审计保留这段执行过程。

## 5. 后端执行

后端将操作解释为实际执行：

```haskell
execute :: Context -> Op e a -> IO (Outcome e a)
```

运行时根据资源绑定选择后端，在授权后调用 execute。使用点验证实际目标、资源代次、
契约版本和有效权限；远端执行端验证自己的资源与权限。后端负责将执行结果映射为 Outcome。

新后端通过实现已有原语接入，并使用这些原语的契约测试验收。需要改变 Agent 接口行为时
增加 Rule；实现层的暂停、传输、copy-up 等步骤使用普通函数或异步流程组织。
策略通过上下文选择资源与后端。已有请求沿用其固定的绑定与规则版本，不支持的契约返回
Unsupported。

后端验收同时检查两个观察面：

- **Agent 接口行为**：结果、错误和可见资源变化符合所选契约。
- **内部执行过程**：资源访问、权限、步骤顺序和释放满足实现契约。

例如 Overlay 写入更新 upper，随后读取反映新内容，而 base 保持原样。
远程读取的契约包含超时和未知结果。内部步骤及其权限范围由后端契约规定，
测试检查实际资源变化与调用记录。

Agent 接口提供操作语义，pVisor 管理内部资源和审计。内部审计使用专属执行路径；
对 Agent 的披露由访问权限决定。延迟、超时等跨越这两个观察面的影响纳入接口契约。
恢复与补偿各自作为有前提的操作定义，执行历史保留已发生的效果。

## 6. 改写与优化

策略改写规定希望得到的行为。优化在给定观察范围内保持行为，同时调整实现方式。
两者都遵守内部执行约束，Event 记录实际采用的过程。

例如，两个操作满足独立性条件时可以交换：

```haskell
do { x <- p; y <- q; pure (x, y) }
  ≈
do { y <- q; x <- p; pure (x, y) }
```

这里的等价要求两种顺序产生相同的 Agent 可见结果、错误和资源效果，并保持约定的
执行顺序要求。证明需覆盖数据依赖、授权条件、取消和外部交互。

两个全定义的私有不可变快照读取是一个简单例子。相反，“先失败、后写入”交换后会
产生额外写入，即使它们访问不同资源。完整有序审计若属于比较范围，也需要保持该顺序。

优化以已经取得的操作及其依赖为输入。每条优化与适用条件、等价推导及反例测试一起定义。

## 7. Event 与重放

Event 是一次请求、改写、执行或外部状态变化的不可变观测记录。它包含身份、来源、
上下文、领域、粒度、严重程度及明确关联，连接 Agent 接口与 pVisor 内部执行。

一条读取轨迹能够回答：Agent 请求了什么，哪条规则改变了它，哪个后端访问了什么资源，
内部完成了哪些步骤，最后交付了什么结果。记录保留规则、后端与契约版本，区分模拟应答、
实际执行和重放来源。

因果关系由已知的触发与依赖建立。scope 描述执行归属，日志位置描述提交顺序。
VM 自发退出等外部观察也有自己的事件。操作结果、收尾结果和审计提交结果分别保留，
因此外部操作成功而日志提交失败时，执行历史可以明确表达审计缺口。

重放使用对应的初始状态、版本、输入、内容和必要执行/交付顺序，在模型中重现所覆盖的
操作与状态。重复投递去重，缺失输入报告缺口，Unknown 保留当时的未知判断。
改变策略或调度形成新的分叉实验。

[Event v2 草案](event-contract-v2.md)进一步定义事件信封、披露与存储协议。

## 8. 实现与验证

核心的扩展单位是原语、规则和后端实现。组合已有原语可以表达的流程，作为普通组合保留；
具体后端的资源管理留在后端。Rust 实现可沿用请求类型、函数和 async 流程来落实这些定义。

验证从文件操作闭环开始：原语测试资源效果与错误，规则测试替换关系，后端测试实际执行，
Event 测试来源与关联。预期由操作契约逐步推导；外部输入尚未确定时，契约给出允许的
结果和失败分支。

[有限模型检查](pvisor-algebra-check.py)覆盖组合律、上下文、改写、后端视图、强制检查、
乱序反例、重放与暂停。它验证这些有限模型；真实驱动的并发、持久化和隔离由对应契约测试
验证。运行：`python3 docs/pvisor-algebra-check.py`。

## 附录：Program 的组合定义

以下 Haskell 风格语义片段给出本文使用的计算树。continuation 是纯函数，产生后续计算；
所有 Agent 接口操作由 Call 表达。领域类型和标准类型类实例的细节在此省略。

```haskell
data Term a where
  Pure :: a -> Term a
  Call :: Context -> Op e b -> (Outcome e b -> Term a) -> Term a

bindTerm :: Term a -> (a -> Term b) -> Term b
bindTerm (Pure x) f = f x
bindTerm (Call ctx op k) f =
  Call ctx op (\r -> bindTerm (k r) f)

newtype Program a = Program { at :: Context -> Term a }

returnP x = Program (\_ -> Pure x)
bindP p f = Program $ \ctx ->
  bindTerm (at p ctx) (\x -> at (f x) ctx)

perform op = Program $ \ctx -> Call ctx op Pure
ask = Program Pure
local f p = Program (\ctx -> at p (f ctx))
```

Program 的 pure 和 `(>>=)` 分别采用 returnP、bindP；Functor 和 Applicative 采用由此
导出的顺序定义。等式按函数外延相等解释，适用于良类型、无隐藏 I/O 的有限计算。
local 的等式由代入定义得到，Monad 三律由 Pure/Call 的结构归纳得到。

对应的 Haskell 接口：[Monad](https://hackage.haskell.org/package/base/docs/Control-Monad.html)、
[ExceptT](https://hackage.haskell.org/package/mtl/docs/Control-Monad-Except.html)、
[Reader](https://hackage.haskell.org/package/mtl/docs/Control-Monad-Reader-Class.html)、
[bracket](https://hackage.haskell.org/package/base/docs/Control-Exception.html#v:bracket)。
