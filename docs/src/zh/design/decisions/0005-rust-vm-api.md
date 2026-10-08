# 0005：将 VM 后端差异收敛到单一 Rust 运行时 {#adr-0005}

**状态。** 实现回填；记录已实现的边界，不代表维护者另行批准了 ADR。

**背景。** VM 配置、设备定制与快照逻辑分散在 pVisor 和多个 libkrun crate。C context、借用裸指针和公开后端结构使所有权与平台差异泄漏给调用方。

**备选。** 保留外部 C context ABI 和独立 vendor 组件，或合并私有运行时模块并提供统一 Rust 契约。

**当前选择。** `pvisor-vm` 统一编译核心模块。只有 `api` 对外公开；跨平台 struct 与 trait 定义全部公开方法签名，不包含条件编译或默认实现。私有适配器实现 trait。架构布局与虚拟化行为使用独立的内部 trait；调用方查询能力并接收明确的不支持错误。固件数据 ABI 与操作系统 FFI 保持私有。

**迁移与后果。** Executor、shim、snapshot/checkpoint/pager、示例和 init 基准使用新 API。硬件/设备测试归入本 crate。静态内核打包归运行时负责。旧核心 crate 依赖和 VM 控制 C 函数移除；已有证据标识及快照序列化字段兼容。统一 API 不承诺跨架构恢复。`api_contract` 与 `repository_boundary` 检查边界；本机测试和目标平台编译检查提供不同证据。见[开发文档](../../community/development.md)和[运行时契约](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/README.md)。
