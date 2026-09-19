# persisting-gateway 集成测试

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

移除 pChronicle 时，AgenticMD 编解码、Markdown 生成、草稿替换和投影去重
的实现及测试一并移除。保留的事件写入、草稿不入日志、Story 快照和 WAL
行为由 `src/engine/tests/` 覆盖；仍公开的投影过滤规则由
`src/projection/policy.rs` 的测试覆盖。

```bash
cargo nextest run --locked -p persisting-gateway --test llm_fixtures --test ag_fixture_tests
cargo nextest run --locked -p persisting-gateway --test capture_apps_claude
cargo nextest run -p persisting-gateway --test model_api_forwarding --locked
```

## Links

- [`persisting-gateway`](../README.md)
- [Capture trajectories](../../../docs/src/en/guides/capture.md)
