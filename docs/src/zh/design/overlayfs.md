---
status: todo
search:
  exclude: true
---

# OverlayCore 设计

!!! warning "规划中"
    实现细节仍待负责人撰写。对外承诺见[暂存与 apply 语义](../concepts/staging.md)，CLI 行为见 [Run 项目发现](../reference/cli.md#run-项目发现)。

## 要回答的问题

暂存与 apply 的承诺（S-STAGE-001 到 014）在实现上如何保证？崩溃时如何恢复？

## 需求

- 写时复制与首次触达原像（first-touch preimage）、durable fingerprint 的记录方式；
- apply 状态机：Prepared → TargetApplied → Committed，每个状态的持久化内容与恢复路径；
- 冲突检测：如何覆盖递归删除、目录替换、链接替换（对应 S-STAGE-008、S-STAGE-011、S-STAGE-012）；
- macFUSE、FSKit、Linux FUSE 与 VM virtio-fs 的差异；硬链接组与不透明目录的处理；
- 已知问题：`copied_hard_links` 目前只在内存中维护。

## 验收标准

- 每个状态转换给出对应的代码位置与测试；
- 与[apply/drop 成本与崩溃一致性（规划中）](../benchmarks/apply.md)的崩溃注入结果互相引用。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关代码：`crates/pvisor-overlay-core`、`crates/pvisor-overlayfs`

## 当前实现：从写入到应用

`core.rs` 中的 `OverlayCore` 管理 lower/upper 视图；宿主 FUSE 与 VM 文件服务接入这套操作。第一次修改路径前，`record_preimage` 从真正的 apply 目标取指纹，而不是从覆盖在它上面的可见 lower 取值。已有条目不会因后续写入而重置。

指纹区分不存在、常规文件、目录、符号链接和其他节点。常规文件保存 SHA-256 与权限/所有权；链接保存目标字节而不解引用；目录还保存修改时间。原像条目位于 `preimages/entries/`，文件与目录同步后才继续操作。标为完整的原像日志缺少所选路径时，apply 拒绝；旧 stage 的 apply 时取样不能声称提供同样的运行期冲突检测。

## apply 的持久状态

`apply.rs::apply_overlay_selected` 先取得目标锁、恢复待完成批次、计算净改动与选择集合，并检查整个选择集合的原像，再保存应用意图。该锁协调 pVisor apply，不能阻止外部编辑器写入。

| 状态 | 已持久化的信息 / 恢复动作 |
| --- | --- |
| `Prepared` | apply ID、Overlay ID/generation、目标、选择器、改动、恢复路径和原像已写入 `apply-ledger.json`；恢复时检查目标仍为原像或本批次期望结果，再向前完成写入 |
| `TargetApplied` | 目标已更新，状态先于 upper 清理保存；恢复只完成已应用 upper 的裁剪与原像消费，避免把裁剪过的不透明目录重新当作完整修改 |
| `Committed` | 剩余改动已计算并保存，批次完成；仍有改动时 Overlay 为 `Staged`，否则为 `Applied` |

恢复拒绝 Overlay ID、generation 或目标不匹配的旧批次。目录删除和替换还检查被折叠的后代原像，避免遗漏递归树中的冲突。选择性 apply 扩展相关硬链接组；创建后又删除的文件不进入净改动。

## 验证入口与剩余工作

```bash
just test pvisor-overlay-core pvisor-overlayfs
just semantics
```

`core.rs` 的 `first_touch_preimage_is_durable_and_never_rebased` 验证首次原像；`apply.rs` 的 `apply_rejects_a_target_changed_after_first_touch`、`directory_replacement_checks_descendants_and_recovers_after_mutation`、`prepared_apply_recovers_before_or_after_target_mutation`、`target_applied_recovery_only_finishes_partially_pruned_opaque_upper` 覆盖冲突与恢复。

多文件写入仍可能被外部读者看到中间状态；恢复是向前完成，不是自动回滚。`copied_hard_links` 的内存状态、各挂载后端差异及系统级崩溃注入矩阵仍属于上方待完成的设计审查。
