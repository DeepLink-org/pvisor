# 测试与 semspec

单元和集成测试检查实现行为。语义规格表达产品承诺，需要人工审核。

## 常用命令

| 命令 | 作用 |
| --- | --- |
| `mise run test` | 默认 Rust 和工具检查，包含 API doctest |
| `mise run test -- pvisor-core` | 通过 nextest 运行一个 Rust 包 |
| `mise run test-py-unit` | Python 逻辑、格式和 mock 操作测试 |
| `mise run test-py-regression` | Python 真实子进程、工具链和集成回归 |
| `mise run test-py -- -k NAME` | 通过原生 unittest 筛选 Python 测试 |
| `mise run test-scripts` | 任务和 shell 回归 |
| `mise run test-benchmark-tools` | benchmark 工具正确性检查，不执行性能测量 |
| `mise run test-doc` | Cargo 原生 API doctest |
| `mise run test-isolation` | Linux rootless/FUSE 严格验收 |
| `mise run test-macos-fuse` | macOS 显式挂载验收 |
| `mise run examples -- SCENARIO` | 端到端产品示例 |
| `mise run cases -- --suite stage` | STAGE 语义规格 |
| `mise run test-semspec` | semspec 实现测试 |
| `mise run semspec -- lint docs/src/zh/cases` | 规格静态检查 |

## 测试执行与归属

[测试架构](https://github.com/DeepLink-org/pvisor/blob/main/tests/README.md)
定义核心原则、目录职责、runner 和能力门禁。mise 驱动任务，
nextest、unittest、Node、Bats、Go 和 semspec 发现用例并报告结果。

Rust 私有测试跟随实现模块，外部契约和集成测试放在各 crate 的 `tests/`。
Cargo target 类型不保证测试没有进程或挂载副作用。
Python 的 `tests/unit/` 验证隔离操作，`tests/regression/` 验证真实进程边界。
通过原生 runner 参数选择模块和用例：

```bash
mise run test -- pvisor-cli -- --test apply_cli
mise run test-py -- tests.unit.test_docs
mise run test-py-regression -- -k stage
```

硬件验收需要有能力的宿主和匹配制品。编译、mock、skip 和 ignored 测试
不构成真实 VM 或挂载覆盖。前提见[平台要求](../reference/platforms.md)。

## 语义规格

每个 Markdown 用例包含编号、语义声明、违反示例和可执行脚本。
`docs/src/zh/cases/06-stage-apply.md` 的 STAGE 用例覆盖
[暂存、apply、drop 承诺](../concepts/staging.md)，
`docs/src/zh/reference/cases.md` 的 DOC 用例检查可执行文档。

执行结果为 PASS、FAIL、SKIP、XFAIL、XPASS、ERROR；
审核状态为 UNREVIEWED、STALE、REVIEWED。

## 人工审核规则

- PASS 表示实现满足脚本，不代表声明已获批准。
- 新增或修改的用例需要人工审核。
- 只有维护者可以运行 `semspec approve`/`revoke`，或编辑 `REVIEWED.toml` 和 `.approved/`。
- 不得为了通过而削弱声明、检查或 xfail 标注。

规格和审核模型见 `tools/semspec/DESIGN.md`。
