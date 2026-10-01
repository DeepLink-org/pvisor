# 贡献指南

本页是完整版；仓库根目录的 [`CONTRIBUTING.md`](https://github.com/DeepLink-org/pvisor/blob/main/CONTRIBUTING.md) 是它的英文摘要，两处修改需同步。

## 从哪里开始

- **报告问题或提需求**：提交 GitHub issue。安全问题不要公开提交，按[漏洞披露政策](../security/disclosure.md)处理。
- **较大的改动**：先开 issue 讨论设计，再写代码。
- **认领文档需求**：导航中标有"（规划中）"的页面是公开的需求单，每页写明了要回答的问题和验收标准，可以直接认领。

## 开发循环

```bash
just build            # debug 构建
just test             # 通过 cargo nextest 运行 Rust 测试，再运行 Python 测试
just test pvisor-core # 只运行一个包的 Rust 测试
just fmt-check        # 格式检查
just lint             # clippy 与 ruff
just docs-build       # 构建并检查文档站点
```

平台准备（FUSE/macFUSE、KVM/HVF、OCI runtime）见[开发环境与工程说明](development.md)，测试分工见[测试与 semspec](testing.md)。

## 语义规格的规则

产品承诺写成语义规格（semspec），位于 `tests/semantics/` 与 [`reference/cases.md`](../reference/cases.md)。测试通过不等于人工批准：

- 可以起草新用例、修复实现；
- 不得为了通过而削弱已有的声明、检查或 `xfail` 标注；
- 批准用例（`semspec approve`）以及编辑 `REVIEWED.toml`、`.approved/` 快照只能由维护者人工完成。

## 提交 PR

- 每个 PR 聚焦一件事，说明行为变化以及你如何验证；
- 运行 `just fmt-check`、`just lint` 和相关的 `just test`；
- 同步更新负责该行为的权威文档页，遵守 `docs/README.md` 的约定：每个主题只有一篇权威页面，其他页面一句话加链接。

贡献内容按 [Apache License 2.0](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE) 授权。
