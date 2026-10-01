# 测试与 semspec

- Rust 测试通过 `cargo nextest`（`just test`），可传入 Cargo 包名限定范围。
- Python 测试随 `just test` 一起运行。
- 语义规格（semspec）用 `just test-semspec` 验证 runner，`just semspec lint` 校验规格。

语义规格验证产品承诺；PASS 不等于人工批准。改动语义源需人工审核。

!!! note "TODO"
    补覆盖率、CI 分工与人工审核流程；与 tools/semspec 文档互相引用。

