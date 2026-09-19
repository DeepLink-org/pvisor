# System Design

Persisting 提供 Agent 执行与轨迹历史的持久化基础设施。本节聚焦
当前公开产品路径：

- [pVisor](../pvisor/index.md) 虚拟化并治理单个 Agent Run。

Gateway、OverlayFS 与 OverlayNet 是 pVisor 运行时机制。存在稳定 Run identity 时，它会连接
这些产品域，但各域也有独立入口。

![Persisting 产品域与集成关系](../../assets/diagrams/persisting/system-products.svg)

## 跨产品契约

```text
pVisor Run
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> EventRecord JSONL（+ 可选 live Markdown）
  Run Bundle + staged Effects → review / apply / drop
```

Attempt finalization 会写入私有、带版本的 Run Bundle，并保留 staged Effect，供之后执行
review/apply/drop。配置后的 capture 会把 Gateway 轨迹 event 与 pVisor lifecycle record
以 EventRecord JSONL 形式写入这次 Run，包括这些 record 携带的 Evidence。完整 Bundle
及其中的 Artifact、lineage、Effect 与更完整的 Evidence 清单仍留在本地，除非另行搬运。

职责边界可以概括为两点：

- **pVisor 负责执行。** 它定义单个 Run 的边界，以及模型、网络和文件系统 runtime driver。
  私有 Run Bundle 就是执行记录。

## 按问题继续阅读

- [完整架构与目标模型](architecture.md)
- [从本地到集群的连续性](local-to-fleet.md)
- [安全与 Evidence 模型](security-evidence.md)
- [pVisor 实现边界](../pvisor/design/index.md)

交付状态以产品 Design 页面与[项目工程笔记](../project/engineering.md)为准。目标架构不能
作为功能已经实现的证据。
