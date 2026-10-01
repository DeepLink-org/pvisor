# 测试与 semspec

pVisor 把正确性分成两层：代码层面的正确性由单元测试和集成测试保证，可以交给自动化；产品承诺层面的正确性由语义规格（semspec）表达，必须经过人工审核。

## 常用命令

| 命令 | 作用 |
| --- | --- |
| `just test` | 通过 `cargo nextest` 运行全部 Rust 测试（debug 模式），再运行 Python 测试 |
| `just test pvisor-core` | 只运行一个包；别名 `pvisor`、`core`（`control`、`agentctl`）、`capture`（Gateway）、`shim` |
| `just test-py -k NAME` | 只运行 Python 测试，可附加 pytest 参数 |
| `just test-isolation` | Linux rootless/FUSE 严格回归，缺少 user namespace 时不跳过 |
| `just smoke` | 构建 debug CLI 并检查主要子命令 |
| `just examples [场景]` | 运行 `examples/pvisor/` 下的端到端示例 |
| `just semantics` | 在全新临时工作区运行 STAGE 领域的语义规格 |
| `just cases` | 运行文档语义规格（S-DOC，来源 [`reference/cases.md`](../reference/cases.md)） |
| `just semspec lint` | 静态检查规格格式 |
| `just test-semspec` | 测试 semspec 工具本身 |

只有需要 doctest 或文档明确要求的特殊 runner 时，才直接使用 `cargo test`。

## 语义规格

语义规格是 Markdown 用例，每条包含编号（如 `S-STAGE-008`）、**语义**、**违反示例**和一段可执行脚本。当前领域：

- **STAGE**（`tests/semantics/stage-apply.md`）：暂存、apply、drop 的承诺，见[暂存与 apply 语义](../concepts/staging.md)；
- **DOC**（`docs/src/zh/reference/cases.md`）：文档中可执行示例的行为。

结果分为 PASS、FAIL、SKIP、XFAIL、XPASS、ERROR；审核状态分为 UNREVIEWED、STALE、REVIEWED。

## 人工审核规则

- PASS 只说明当前实现满足脚本，不说明语义本身正确；
- 新用例和修改过的用例是 UNREVIEWED 或 STALE，发布门禁不把它们当作已批准；
- `semspec approve`/`revoke` 以及编辑 `REVIEWED.toml`、`.approved/` 快照只能由维护者人工执行；
- 任何人（包括 AI 工具）都不得为了通过而削弱已有的声明、检查或 `xfail` 标注。

完整设计见 `tools/semspec/DESIGN.md`。
