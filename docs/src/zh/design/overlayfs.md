---
status: todo
search:
  exclude: true
---

# OverlayCore 设计

!!! warning "规划中"
    本页需要实现负责人撰写。对外承诺见[暂存与 apply 语义](../concepts/staging.md)，CLI 行为见 [Run 项目发现](../reference/cli.md#run-项目发现)。

## 要回答的问题

暂存与 apply 的承诺（S-STAGE-001 到 014）在实现上如何保证？崩溃时如何恢复？

## 需求

- 写时复制与首次触达原像（first-touch preimage）、durable fingerprint 的记录方式；
- apply 状态机：Prepared → TargetApplied → Committed，每个状态的持久化内容与恢复路径；
- 冲突检测：如何覆盖递归删除、目录替换、链接替换（对应 S-STAGE-008、011、012）；
- macFUSE、FSKit、Linux FUSE 与 VM virtio-fs 的差异；硬链接组与不透明目录的处理；
- 已知问题：`copied_hard_links` 目前只在内存中维护。

## 验收标准

- 每个状态转换给出对应的代码位置与测试；
- 与[apply/drop 成本与崩溃一致性（规划中）](../benchmarks/apply.md)的崩溃注入结果互相引用。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关代码：`crates/pvisor-overlay-core`、`crates/pvisor-overlayfs`
