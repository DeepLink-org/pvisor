# Contributing

This is the full guide. The repository-root [`CONTRIBUTING.md`](https://github.com/DeepLink-org/pvisor/blob/main/CONTRIBUTING.md) is its English summary; keep changes synchronized.

## Where to start

- **Report a bug or request a feature** through GitHub Issues. Do not report security issues publicly; follow the [Disclosure policy](../security/disclosure.md).
- **Discuss substantial changes** in an issue before implementation.
- **Claim documentation work**: pages marked “(planned)” in navigation are public requirements with questions and acceptance criteria.

## Development loop

```bash
just build            # debug 构建
just test             # 通过 cargo nextest 运行 Rust 测试，再运行 Python 测试
just test pvisor-core # 只运行一个包的 Rust 测试
just fmt-check        # 格式检查
just lint             # clippy 与 ruff
just docs-build       # 构建并检查文档站点
```

See [Development environment and engineering](development.md) for FUSE/macFUSE, KVM/HVF and OCI runtime preparation, and [Testing and semspec](testing.md) for test responsibilities.

## Semantic specification rules

Product promises are semantic specifications under `tests/semantics/` and [`reference/cases.md`](../reference/cases.md). Passing tests is separate from human approval:

- You may draft new cases and repair implementations;
- Never weaken existing claims, checks or `xfail` annotations to obtain a pass;
- Maintainers must manually approve cases with `semspec approve` and edit `REVIEWED.toml`/`.approved/` snapshots.

## Submit a PR

- Focus each PR on one change; explain behavior and validation;
- Run `just fmt-check`, `just lint` and relevant `just test` checks;
- Update the authoritative page for the behavior, following `docs/README.md`: one authoritative article per subject, with concise links elsewhere.

Contributions are licensed under [Apache License 2.0](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE).
