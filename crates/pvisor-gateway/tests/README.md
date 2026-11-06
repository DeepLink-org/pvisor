# pvisor-gateway 集成测试

**Gateway conversion / capture fixture 回归，以及 Claude 专有轨迹 fixture。**

拥有本 crate 的集成测试入口与 fixture 读取辅助。不拥有 Gateway 实现
（[`../README.md`](../README.md)）或 capture 存储格式。`fixtures/**/README.md`
是 fixture 数据说明，不是组件文档。

## Fixture 数据

| 目录 / 文件 | 说明 |
|-------------|------|
| [`fixtures/`](fixtures/README.md) | 主要来自 agentgateway LLM 测试集（含致谢与许可说明） |
| [`fixtures/local/`](fixtures/local/README.md) | PolicyVisor 自有补充 fixture |
| [`support/ag_fixtures.rs`](support/ag_fixtures.rs) | 读取 fixture、解析 AG `.snap`、归一化比对 |
| [`ag_fixture_tests.rs`](ag_fixture_tests.rs) | 基于 AG 数据的 conversion + capture 回归 |
| [`llm_fixtures.rs`](llm_fixtures.rs) | 基础 smoke 测试 |
| [`semantic_conversion.rs`](semantic_conversion.rs) | 生产 understanding / bridge 入口的 Codex、工具历史和并行结果顺序回归 |
| [`model_api_forwarding.rs`](model_api_forwarding.rs) | Chat Completions、Responses、Messages 的文本与 tool call 非流式/SSE 同协议转发测试 |
| [`capture/apps/claude/`](capture/apps/claude/) | Claude 专有轨迹 fixture（与 AG 无关） |

## Run

事件写入、草稿不入日志、Story 投影恢复和 Journal 行为由
`src/engine/tests/` 覆盖；公开的投影过滤规则由
`src/projection/policy.rs` 的测试覆盖。

```bash
cargo nextest run --locked -p pvisor-gateway --test llm_fixtures --test ag_fixture_tests
cargo nextest run --locked -p pvisor-gateway --test capture_apps_claude
cargo nextest run -p pvisor-gateway --test model_api_forwarding --locked
```

## Links

- [`pvisor-gateway`](../README.md)
- [Capture trajectories](../../../docs/src/zh/guides/capture.md)


## Host 模型流缓冲

`model_wait_http` 包含普通无专用 header 请求的 size/EOF/timeout/wake 交付
屏障、超大首块重放与 shutdown 取消用例。`src/model_wait/stream_buffer.rs`
的内部用例覆盖前缀大小、超额尾部、固定时间预算、心跳不续期、唤醒竞争、
部分数据后的错误以及 EOF 后不再读取。它们不构成 VM 内存回收或延迟证据。


多轮扩展新增：无专用 header 的同一 HTTP 流经过多个 batch resume 屏障、
末批排空后才最终退休；内部 `round_stream` 用例覆盖超大块有序交付、
等待排空时禁止 rearm、断连/停止取消。输出队列的 marker 用例检查 ACK
确实在 body 消费该标记时发出。上述用例已于 2026-10-17 在 macOS 和
Linux 的相关测试选择中通过；真实 VM 交付与回收验证范围见
[`vm_reasoning_rounds` 工程探针](../../pvisor/README.md#long-reasoning-engineering-probe)。

`model_wait_http` 的有界故障用例先确认客户端收到完整 SSE 前缀，再让本地
上游在帧中途 EOF 或返回 body 读取错误。无 lifecycle、单次交付和多轮
交付三种模式都必须向客户端报告 body 错误，且只记录一次取消，不得记录
完整响应。断连用例分别在下一批上游读取期间和下一批 resume 屏障阻塞期间
关闭客户端，验证多轮 wait 只取消一次、上游 body 被释放且没有完整响应。
每个场景顺序执行，仅使用 loopback、少于 1 KiB 的载荷和五秒操作超时；
测试结束关闭 Gateway 并 abort/join 自己的上游任务。这些测试不模拟 VM、
真实供应商或主机级故障。

Messages bridge 的 tool stream 用例在上述三种 lifecycle 模式下分别覆盖
帧中途 EOF、只有 `[DONE]` 而没有 `finish_reason`、body 读取错误、
供应商终止标记之后的 transport 错误、客户端断连，以及完整成功的对照流。
测试先收到译后的 `tool_use` 与不完整参数，再注入故障；失败不得产生
`content_block_stop`、成功的 `message_delta` 或 `message_stop`。
成功对照检查参数精确重组、`tool_use` stop reason 和各终止事件只出现一次。
安装 lifecycle 时还验证首批 headers 等待 resume，transport 错误/断连
取消多轮 wait，正常 HTTP EOF 经过最终交付屏障后再进行语义终止校验。
每种组合顺序执行，译后输出限制在 4 KiB 内，使用相同的五秒超时和清理。

响应 headers 之前的断连用例直接关闭发送完整 HTTP 请求的 TCP socket，
分别定位到 lifecycle entry、首批读取和首次 resume 的阻塞点；单次与多轮
lifecycle 都须在 Gateway 仍运行时取消一次、释放已建立的上游 body、
记录取消而非响应完成。首批读取由实际 poll 的 wake future 通知确认，
不是仅凭供应商收到请求推断。随后才执行单独限时的 Gateway shutdown；
测试不靠 shutdown、供应商 EOF 或放开 admission gate 来触发取消。

请求上传故障用例发送有效首个 chunk 后再发送非法 chunk size，检查返回
400 而不是 413。拒绝发生在 capture/model-wait admission 之前，不产生
取消或完成记录；随后同一 Gateway 的正常请求须成功并只生成一次完成。
私有 body reader 用小字节上限检查精确边界、超限 413、读取错误 400，
并检查失败会释放仍未 EOF 的输入 owner。

HTTP/capture 交叉用例在首个请求的 Journal commit 之后阻塞 observer，
再关闭正在等待 lifecycle entry 的真实客户端 socket。该 HTTP wait 只取消
一次，已接受的 capture 不回滚；同 story 的后续健康 HTTP 请求仍可完成，
但其 capture 不能越过先前 owner。关闭已确认接入的 admin 连接用于确认
shutdown 已进入 transport teardown，此时 shutdown 仍须等待 capture gate。
释放后检查 Journal 与 observer 的四条记录及因果 ID 完全一致：
首个请求及取消、后续请求及完成。此用例最多四条 loopback 连接，gate、
HTTP 操作和清理均使用五秒上限，且 unwind 会释放 observer。

Observer 故障用例令首个 HTTP 请求的 post-commit observer 返回合成错误，
随后同 story 的健康请求继续完成。分别覆盖可写 trajectory dead letter
和临时目录内的目标路径冲突：两者都不得重试/抹除 Journal append、伪造
取消或令 shutdown 失败。可写诊断只包含一次原始已提交 event，沿用
Dialogue retention 与凭据脱敏；不能写诊断时仅告警，不改写冲突目录。
