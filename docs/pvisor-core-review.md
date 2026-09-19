# pVisor 核心设计与实现审查

[KNOWN]（HIGH）审查日期：2026-09-19；当前基线：`4f8076d`，其运行时代码与开始审查时的 `bc95c2a` 相同；语义提案：`pvisor-algebra.md` v0.8。本次只新增审查报告，未修改运行时代码，也未改动已有设计草案。

## 判断

[INFERRED]（MED）**新核心适合成为 pVisor 的操作语义边界。它能够明显减少身份传递、策略处理、错误分类和审计收尾的重复实现；目前没有证据支持“整个项目代码大幅减少”或“性能自动改善”。** 文件系统提交、进程回收、协议转换和 VM 网络栈仍需要具体算法与平台实现。

[INFERRED]（MED）现在的主要问题是跨模块契约没有收拢：同一个 Attempt 在不同授权入口具有不同上下文；不同事件生产者对提交、序号和失败的理解不同；策略拒绝、传输失败与正常完成走不同记录路径。新核心应优先消除这些分歧，再逐步接入更多操作。

[INFERRED]（MED）Rust 实现宜保留普通类型、函数和 async 控制流。Haskell 表达用于定义操作组合、上下文作用域和改写规律。第一阶段不需要通用 Free Monad 运行时、动态操作注册中心、统一字节流解释器或全局优化器。

## 证据与验证范围

[COMPUTED]（HIGH）七个产品 crate 的 `src/**/*.rs` 共 174 个文件、72,827 行，包含内联测试。此数字只说明范围，不作为代码冗余的依据。

[KNOWN]（HIGH）审查追踪了 Run 创建与收尾、授权与网络分派、模型请求与流式捕获、Overlay 首次修改与提交恢复、原生 Agent replay，以及测试与性能基线。重点函数进行了逐路径阅读；这不是对所有源文件、平台和故障模式的穷尽证明。

[COMPUTED]（HIGH）验证结果如下。Rust 与 Python 首次运行受到沙箱权限限制，以下使用放开本地服务与缓存访问后的结果：

| 验证 | 结果 |
|---|---|
| `just test persisting-control persisting-overlay-core persisting-overlayfs persisting-overlaynet` | 120 通过 |
| `just test persisting-pvisor persisting-gateway persisting-replay` | 618 通过、1 失败，nextest 另报告 3 skipped |
| 单独重跑 macOS 失败用例 | 同样失败，见 F5 |
| `just test-py` | 36 通过 |
| `just test-benchmark` | 4 通过；验证报告逻辑，不是性能采样 |
| `python3 docs/pvisor-algebra-check.py` | 有限模型检查通过 |
| 直接包含现有 `event.rs` 的取消回归检查 | 失败并复现重复序号，见 F1 |

[KNOWN]（HIGH）本机为 macOS，未执行 Linux 隔离、真实 VM 和断电恢复验证。部分平台测试会在缺少条件时直接返回，测试进程成功不能替代对该后端的实际覆盖。有限模型检查不执行 Rust 后端，也不能证明其实现正确。

## 需要优先处理的问题

### F1 · P1：发布任务取消后，已写入事件的序号会被复用

[KNOWN]（HIGH）位置：[event.rs](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/event.rs:143)。`next_seq` 在等待 `sink.append()` 返回后才递增。显式 `Unknown` 错误会消耗序号，但取消 future 不经过错误分支。

[COMPUTED]（HIGH）使用现有源文件和一个合法的异步 sink 复现：sink 接收第一条记录后等待；取消该发布任务；随后发布第二条记录。记录序号实际为 `[0, 0]`，期望 `[0, 1]`。两个事件的 UUID 不同，冲突发生在 Attempt 序号。

[INFERRED]（HIGH）触发条件是 sink 在产生写入效果后仍有可取消的等待点。当前同步执行写入的 CLI JSONL sink 不包含这个等待点，但公开的异步 `EventSink` 允许这样的实现。此缺陷会破坏依赖单调序号的排序、消费游标和去重。

[INFERRED]（HIGH）最小修复是在可取消等待前保留序号，允许出现空洞；如保留“明确拒绝时复用”的行为，只在持锁且已确认无提交时回退。后续统一 Event 提交契约时，将事件身份与日志位置分开。取消和 `Unknown` 都不能解释为“肯定没有写入”。

[KNOWN]（HIGH）复现文件：[独立测试](/tmp/pvisor-review-cancel/src/lib.rs)、[manifest](/tmp/pvisor-review-cancel/Cargo.toml)、[输出](/tmp/pvisor-review-cancel.log)。运行 `cargo nextest run --offline --manifest-path /tmp/pvisor-review-cancel/Cargo.toml`。这些临时文件直接引用当前项目源码，没有复制一份被修改的 publisher。

### F2 · P1：删除与目录提交缺少目标持久化屏障

[KNOWN]（HIGH）位置：[apply_selected_directory](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/runtime/overlay.rs:1417)。whiteout 删除、opaque 清空和目录创建/元数据更新没有同步受影响的目标目录。相对地，文件替换路径在 [copy_upper_entry](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/runtime/overlay.rs:1735) 中同步了文件和父目录。

[KNOWN]（HIGH）[apply_overlay_selected](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/runtime/overlay.rs:1046) 随后持久化 `TargetApplied`，清理 upper，再提交 ledger。同步 ledger 所在目录不能提供目标目录已持久化的保证；Linux 对目录项持久化要求单独同步目录，见 [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html)。

[INFERRED]（MED）对于只有删除或目录变更的 apply，断电后可能出现 ledger 已完成、upper 已清理，而目标变更未保留的状态。这里确认的是持久化顺序缺口，未进行真实断电复现。

[INFERRED]（HIGH）最小修复是复用现有目录同步工具，在 `TargetApplied` 前完成目标变更的持久化，并覆盖仅删除、仅空目录、opaque 清空的故障测试。新核心可规定提交顺序，实际屏障必须由 Overlay 实现。

### F3 · P2：模型拒绝与发送失败没有对应的调用结束记录

[KNOWN]（HIGH）位置：[llm_capture.rs](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/gateway/llm_capture.rs:190)。代码先记录 `Event::Request`，但策略拒绝在 [279 行](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/gateway/llm_capture.rs:279) 直接返回 403；认证解析、请求构造及上游发送失败也直接返回。外层代理将错误转换为 502，没有补充此调用的终态事件。

[INFERRED]（HIGH）因此已记录请求的轨迹可能无法区分“策略正常拒绝”“网络失败”和“尚未结束”。现有捕获 Event 只有 Request、Draft、Complete、Cancelled，进一步限制了错误语义的表达。

[INFERRED]（HIGH）这是新核心最适合解决的问题：请求进入后固定身份，所有可处理分支汇入同一个 `Outcome` 收尾出口，领域信息保留在结果中。流式请求要单独固定完成边界，返回响应头并不代表响应体已经完成；进程崩溃留下的未决操作则通过恢复记录解释，不能伪造正常终态。

### F4 · P2：模型授权丢失已存在的 Attempt 身份

[KNOWN]（HIGH）[Gateway dispatch](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/gateway/dispatch.rs:56) 为网络请求传递 `state.attempt_id`；同一 Gateway 中的 [ModelCallRequest](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/gateway/llm_capture.rs:244) 却固定使用 `attempt_id: None`。

[INFERRED]（HIGH）读取 Attempt 身份的自定义 controller 无法对模型调用使用与网络调用相同的隔离、配额或审计逻辑。内置 controller 当前没有使用该字段，因此不能据此声称已经发生内置策略绕过。

[INFERRED]（HIGH）最小修复是传递现有身份。迁移核心后，应在受信入口创建执行上下文，由后续路径继承；模型和网络模块不再分别拼装相同身份字段。

### F5 · P2：多层 Overlay 的实现能力与集成测试契约冲突

[COMPUTED]（HIGH）`safe_profile_stages_reviews_and_applies_on_macos` 在完整运行和单独运行中均失败。它使用 `--overlayfs-compose`，最终预期 apply 成功；[CLI mutate](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/cli/runtime.rs:298) 明确拒绝 base 之上有只读层的 apply。

[INFERRED]（HIGH）需要先确定此配置承诺的操作集：若暂不支持合并层提交，测试与用户可见能力必须一致；若承诺支持，则需要完整 merged diff 的实现。不能为了测试通过删除当前保护条件。

[INFERRED]（HIGH）新核心的 `Unsupported` 可以统一表达这种结果，但后端支持哪些契约必须由真实适配测试确认。当前报告将其列为契约回归，不将它扩大解释为所有 Overlay apply 都失败。

### F6 · P2：文件指纹一次读入整个文件，并占用共享 preimage 锁

[KNOWN]（HIGH）[fingerprint_at](/Users/reiase/workspace/pVisor/crates/persisting-overlay-core/src/core.rs:101) 使用 `sha256_hex(&fs::read(...))`；[record_preimage](/Users/reiase/workspace/pVisor/crates/persisting-overlay-core/src/core.rs:275) 在 preimage 锁内完成指纹、序列化及文件和目录同步。首次 copy-up 还需要复制文件内容。

[COMPUTED]（HIGH）通过独立 release 二进制直接调用该函数，在本机对临时稀疏文件测得：1 MiB 输入的进程峰值 RSS 为 2,654,208 字节，256 MiB 输入为 270,057,472 字节，约 2.5 MiB 与 257.5 MiB。该测量验证整文件内存开销，没有测量生产吞吐或磁盘延迟。测量入口：[main.rs](/tmp/pvisor-review-cancel/src/main.rs)。

[INFERRED]（HIGH）先改成固定缓冲区的流式摘要即可消除与文件大小线性增长的摘要缓冲，无需改变操作语义。锁粒度与复制/摘要合并应根据随后测得的争用决定；不能跳过 preimage 持久化来换速度。Monad 组合本身不会消除这些 I/O。

### F7 · P2：透明 HTTP 转发逐请求创建客户端

[KNOWN]（HIGH）[transparent_forward_authorized](/Users/reiase/workspace/pVisor/crates/persisting-overlaynet/src/forward.rs:112) 每次创建新的 reqwest Client，授权后的地址绑定也在这里设置。Gateway 模型上游已经在 [GatewayState](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/gateway/state.rs:258) 中复用 Client，两条路径行为不同。

[INFERRED]（HIGH）透明转发无法跨请求复用该客户端的连接池，对重复访问增加建连成本。修复必须保留 DNS 授权和地址绑定：连接池应按适当的目标及授权边界复用，每次操作仍执行当前授权。不能换成一个会自行重新解析目标的全局客户端。

[INFERRED]（MED）性能收益需用重复请求的上游连接次数、延迟和并发吞吐验证，本次未测量网络加速比例。核心可把最终授权入口固定下来，连接复用仍是网络后端的实现优化。

### F8 · P3：每次部分 apply 都重写完整历史 ledger

[KNOWN]（HIGH）[append_apply_record](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/runtime/overlay.rs:1550) 和 [update_apply_state](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/runtime/overlay.rs:1583) 都读取所有记录、序列化整个 JSON 并原子替换；一次 apply 会经历追加、TargetApplied、Committed 三次写入。

[COMPUTED]（HIGH）若每条记录大小近似固定，连续 n 次部分 apply 的累计 ledger 序列化/写入量随 `1 + 2 + … + n` 增长，即 Θ(n²)。单次成本随已有历史长度增长；这不是已测得的生产延迟。

[INFERRED]（MED）应先测 1/100/1000 次部分 apply 的 ledger 成本。若历史成为瓶颈，将活动事务与已完成历史分离，或复用可靠的追加日志；无须为此引入通用事务框架，也不应让审计日志未经设计直接接管恢复职责。

## 哪些代码能收拢，哪些应继续独立

[INFERRED]（MED）按已追踪路径，建议以责任和契约统一为目标，而非按 crate 数量或文件长度合并：

| 模块 | 交给核心的责任 | 保留在模块内的责任 |
|---|---|---|
| Control | 操作上下文、改写选择、统一结果表达 | 网络和模型匹配规则；已有纯策略判断 |
| Gateway | 逻辑调用身份、改写来源、授权与结果收尾 | provider 协议转换、SSE 分片解析、流量背压 |
| OverlayNet | 操作身份、最终目标授权、连接结果 | DNS 地址固定、TCP 隧道、带宽控制、smoltcp |
| OverlayCore / OverlayFS | 文件引用的绑定和操作结果契约 | whiteout、opaque、copy-up、hardlink、FUSE/virtio-fs 接口 |
| pVisor runtime | Agent 操作结果与宿主收尾结果的关联 | Run lease、进程回收、隔离配置、VM 与容器生命周期 |
| Event / capture | 统一身份与提交语义、因果关联 | LLM 对话、Markdown、storyline 等领域投影 |
| Replay | 受控操作匹配、替代结果的来源与校验 | 原生 Agent session 导入、启动和续跑适配 |

[KNOWN]（HIGH）当前 [Run 收尾](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/pvisor.rs:455) 会在 teardown、Bundle 或终态事件失败时通过 [fail_finalization](/Users/reiase/workspace/pVisor/crates/persisting-pvisor/src/pvisor.rs:669) 改写 Run failure，标记 Infrastructure 且 retryable；原 `exit_code` 仍保留。

[INFERRED]（MED）两层副作用在这里有直接价值：分别保存 Agent 执行结果与宿主收尾结果，再导出最终 Run 状态。宿主审计失败不意味着可以安全重跑已经产生外部效果的 Agent。现有信息不证明已有自动重试重复执行，但未来调度器不能直接把该标志理解为重跑许可。

[KNOWN]（HIGH）Gateway 当前有 WAL、每个 story 的 apply queue、actor mailbox、canonical sink、Markdown/index 更新；[CaptureRuntime](/Users/reiase/workspace/pVisor/crates/persisting-gateway/src/engine/coordinator.rs:25) 承担这些协调。CLI JSONL append 做 flush，finish 才 sync_all；Run publisher 与 Gateway sink 的序号范围及失败处理也不同。

[INFERRED]（MED）最值得压缩的是这部分记录协调：先固定一份事实的身份与提交结果，再由领域消费者维护派生视图。可以减少重复排序、回填和收尾分支。是否删除 actor 或某一队列，要在迁移后证明背压、恢复和吞吐仍满足要求；“有多个队列”本身不足以判定冗余。

[KNOWN]（HIGH）现有 [Replay engine](/Users/reiase/workspace/pVisor/crates/persisting-replay/src/engine.rs:149) 会执行原生 Agent adapter，并产生观察比较；结果中的 `comparison_is_gating` 为 false。

[INFERRED]（HIGH）它与新核心的“按记录返回受控操作结果”承担不同任务。后者可以减少跨 Agent 重复的效果重放逻辑，不能自动替代前者的 session/工具协议适配。重放的范围应由实际截获并保留的操作决定，不能仅凭共有 Event 类型宣称整台 VM 或整个 Agent 可确定复现。

## 对新核心的必要收敛

[INFERRED]（MED）建议保留草案已有概念，不再增加一套后端描述语言。实现时先落实以下五项约定：

1. **一个操作契约明确一个可观察边界。** 文件读写规定偏移、部分结果与句柄生命周期；HTTP 规定响应头与响应体各自在何时完成；取消规定已发生效果如何表达。
2. **上下文由入口建立、由执行路径继承。** Run、Attempt、主体、策略版本和资源绑定不得由各模块重新猜测或拼装。内部宿主操作沿来源关联，但不冒充 Agent 请求。
3. **改写输出经过最终执行检查。** mock/deny 直接产生明确来源的结果；remote/overlay 绑定由后端执行。规则结果类型相同，只保证接口可组合，实际返回值和效果仍需契约校验。
4. **后端通过同一组可执行契约测试。** 新后端改变资源绑定和内部实现；只有提供新的 Agent 可见能力或主动采用不同策略时，才扩展操作目录或改写规则。VM pause/resume 留在后端，并测试取消及错误时的成对释放。
5. **Event 记录实际经过与提交状态。** 操作身份、规则来源、后端执行、结果和内部动作之间有可追踪关联。持久化等级明确，审计未决状态可恢复；同一 journal 的位置不承担跨 journal 的全局因果排序。

[INFERRED]（HIGH）无需把每个字节传输、锁操作或 VM exit 都变成通用计算节点。可以在进入边界时选择处理路径，让无改写的常见操作直接调用现有后端，数据流保持现有缓冲与背压机制。这样才能避免通过统一模型引入新的分配和队列开销。

[INFERRED]（HIGH）暂不实现通用乱序优化。Monad 三律保留组合结构，不能授权交换任意副作用；文件别名、流消费、远端结果和审计顺序都可能产生依赖。先对一个有明确资源独立性条件的优化证明等价，再考虑扩展。

## 最小实施顺序与验收

[INFERRED]（MED）建议按以下顺序推进，每一步都替换已有路径，避免长期并存两套核心：

| 阶段 | 工作 | 验收条件 |
|---|---|---|
| 0：修复基线 | F1、F2、F3、F4；明确 F5 支持范围；流式文件摘要 | 对应回归用例通过；存储提交顺序有故障注入覆盖 |
| 1：Event + 模型调用贯通 | 用一条请求路径统一 Context、改写、Outcome、Event | allow/mock/deny/发送失败/流式取消均有正确结果与来源；移除原路径中的重复收尾 |
| 2：文件操作与第二个后端 | 在已有文件接口上接入契约，比较本地/Overlay/远端实现 | 范围读、部分写、句柄失效、未知结果、取消均通过共享契约用例；确认 base/upper 观察差异符合约定 |
| 3：收拢投影和重放 | 将对话与 Markdown 消费建立在统一事件之后；接入操作级 replay | 投影可重建；结果匹配失败明确停止；重放不会意外再次执行被替代的外部操作 |

[INFERRED]（MED）测试应围绕边界性质而非类型外形：取消后身份不复用；最终检查覆盖改写后的目标与内容；mock 不执行原后端；Unknown 不自动重试非幂等效果；资源在取消后按后端约定释放；提交记录不先于目标耐久；重放只使用匹配的操作及上下文记录。

[KNOWN]（HIGH）现有 [benchmark](/Users/reiase/workspace/pVisor/benchmark/pvisor/README.md:3) 只覆盖最小 host Run、status 与 review。上述热点需要补充相应采样，现有基线不能判定它们是否退化。

[INFERRED]（MED）性能验收先覆盖四个负载即可：文件首写/摘要的峰值内存和延迟；重复 HTTP 请求的连接次数与吞吐；审计开启后的操作 p95/p99 与队列压力；连续部分 apply 的历史增长成本。对比必须保持同样的授权、记录和持久化要求，不把降低保证算作优化。

[INFERRED]（MED）据此，采用新核心的可验证收益是：新增后端可以复用操作契约与结果处理，新增策略可以复用改写入口与审计关联，现有重复分支能够逐步删除。是否达到“整个项目大幅简化”，应在第一条完整路径迁移后用删除的重复实现、共享契约覆盖和性能对比决定。

[RULES I BROKE]: none
