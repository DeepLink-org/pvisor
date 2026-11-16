# 测试与 Scrut

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
| `mise run test-spec-fixtures` | Scrut 原生解析、声明保全、隔离与异常清理 |

## 测试执行与归属

[测试架构](https://github.com/DeepLink-org/pvisor/blob/main/tests/README.md)
定义核心原则、目录职责、runner 和能力门禁。mise 驱动任务，
nextest、unittest、Node、Bats、Go 和 Scrut 发现用例并报告结果。

Rust 私有测试跟随实现模块，功能相关的公开行为规格和回归放在 `tests/spec/<feature>/`，
由 Cargo 原 target 或对应原生 runner 直接发现；其他 crate 契约留在各 crate 的 `tests/`。
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

每个 Scrut 原生 Markdown 用例包含编号、语义声明、违反示例和可执行脚本，
唯一源码位于 `tests/spec/<feature>/S-ID.md`。
`docs/src/zh/cases/06-stage-apply.md` 链接的 STAGE 用例覆盖
[暂存、apply、drop 承诺](../concepts/staging.md)，
`docs/src/zh/reference/cases.md` 的 DOC 用例检查可执行文档。

产品 fixture 保留 PASS、FAIL、SKIP、XFAIL、XPASS 等诊断，并使超时、信号和缺少前提
明确失败。Scrut 原生结果与人工语义批准分开；历史审核状态不由 Scrut 重新计算。

## 人工审核规则

- PASS 表示实现满足脚本，不代表声明已获批准。
- 新增或修改的用例需要人工审核。
- 只有维护者可以批准／撤销规格批准，或编辑 `REVIEWED.toml` 和 `.approved/`。
- 不得为了通过而削弱声明、检查或 xfail 标注。

这些规格仍待人工审核。Scrut 没有原生摘要审核机制；替代策略尚待维护者明确决定，
所以 `--require-reviewed` 当前明确拒绝执行，不会绕过审核。当前审核规则、功能布局与入口见
[规格说明](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/README.md)。
