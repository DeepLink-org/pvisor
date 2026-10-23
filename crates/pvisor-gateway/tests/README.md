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
