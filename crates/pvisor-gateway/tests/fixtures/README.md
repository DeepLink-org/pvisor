# LLM 测试数据（fixtures）

本目录大部分文件来自 [agentgateway](https://github.com/agentgateway/agentgateway) 的 LLM 协议转换回归测试集，供 `pvisor-gateway` 的 conversion 与 capture 测试使用。

## 来源

| 项目 | 路径 |
|------|------|
| **上游仓库** | https://github.com/agentgateway/agentgateway |
| **上游目录** | `crates/llm/src/tests/` |
| **同步基线** | `82cbbf6f5bd988899302c8bce2a8d006355355ef` |

手动更新时，将上游该目录内容复制到本目录（**保留** [`local/`](local/README.md) 子目录，勿覆盖）。

## 目录结构

与 agentgateway 保持一致：

```
fixtures/
├── requests/          # 各协议请求样例（.json）及期望转换 snap（.*.snap）
│   ├── completions/
│   ├── messages/
│   ├── responses/
│   ├── embeddings/
│   ├── count-tokens/
│   └── policies/
├── response/          # 各协议响应样例与跨协议 snap
│   ├── completions/
│   ├── anthropic/
│   ├── bedrock/
│   ├── responses/
│   └── detect/
└── local/             # PolicyVisor 自有 fixture（非 AG），见 local/README.md
```

- **`.json`** / 无后缀文本：原始 wire 输入（请求或上游响应）。
- **`.snap`**：agentgateway 用 [insta](https://github.com/mitsuhiko/insta) 生成的期望输出；YAML frontmatter（`---` … `---`）后为 JSON 或 SSE 正文。
- 原始 SSE fixture 必须保留事件后的空行分隔符，包括文件末尾的 `\n\n`；该空行属于 wire 协议，不能作为多余尾部空白删除。

## 在本项目中的用途

| 测试 | 说明 |
|------|------|
| `tests/ag_fixture_tests.rs` | 经生产 `understanding` 和 `translate_*_for_bridge` 入口对照 AG snap 做 messages/responses ↔ completions 与 Gemini native 转换；capture 用户/助手/usage 矩阵（见 `tests/support/ag_capture_cases.rs`） |
| `tests/llm_fixtures.rs` | 基础 smoke 回归 |
| `tests/semantic_conversion.rs` | 生产入口的 Codex 历史、并行 tool call/result 与 Gemini 结果顺序回归 |
| `src/conversion/*`、`src/dialogue_extract/*` | 部分单元测试通过 `include_str!` 引用本目录 |

协议解析与流式转换还通过 `crates/pvisor-gateway/fuzz/` 下的两个 cargo-fuzz target
覆盖任意输入、分块边界和不完整 SSE frame。
请求 fuzz 对所有五种生产 bridge 解析并渲染；非流式响应同样经过生产解析和
客户端渲染，并检查成功输出的 JSON、模型标识及语义来源协议。

生产语义路径保留 `openrouter_reasoning` 的推理文本，而 AG 旧响应 snap
只列可见文本。测试在完整 snap 上精确补充预期 thinking 块（包含顺序），
不删除原有字段或跳过该用例。Messages 字符串 system 在生产输出中规范化为
text block 数组；属性测试对该数组和文字作精确断言。

PolicyVisor 当前实现 **Messages ↔ Completions**、**Responses ↔ Completions**，以及
**Completions/Messages/Responses ↔ Gemini native generateContent** bridge。支持的请求、响应和
SSE 用例会与上游 snap 做严格 JSON/事件比较；Bedrock、Vertex 和 Detect 等其他 adapter 的
snap 仅作为后续扩展参考，不代表已经实现。

## 致谢与许可

测试数据源自 **agentgateway** 项目及其贡献者。agentgateway 以 [Apache License 2.0](https://github.com/agentgateway/agentgateway/blob/main/LICENSE) 发布。

在此复制并用于回归测试，遵循 Apache-2.0 对再分发与修改的要求。若上游 fixture 有更新，请同步复制并保留本 README 中的来源说明。

```
Copyright agentgateway contributors
SPDX-License-Identifier: Apache-2.0
```
