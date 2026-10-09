# 暂存与 apply 语义

暂存把 Job 的工作区改动留在写时复制的 upper 层。只有显式的 `apply` 才会改变目标工作区。因此文件改动在 apply 之前是可逆的：选择合入、丢弃，或分叉。

这里定义暂存契约，每条承诺对应 `docs/src/zh/cases/06-stage-apply.md` 中的一个用例，可用 `just cases --suite stage` 复现。操作流程见[审查与应用](../guides/review-apply.md)。

## 承诺

| 承诺 | 含义 | 语义用例 |
| --- | --- | --- |
| apply 前工作区不变 | 成功、非零退出或信号终止后，工作区的路径、类型、内容、权限位与链接目标都与初始状态一致，直到 apply | S-STAGE-001 |
| Job 读取自己的变更 | Job 能看到自己的写入、删除与重命名，而宿主工作区不变 | S-STAGE-002 |
| Review 表示净效果 | 清单等于净新增、删除与修改；创建后又删除的文件不出现 | S-STAGE-003 |
| 全部 apply 等价直接运行 | 暂存一个脚本后执行 `apply --all`，得到的树与直接在 workspace 中运行脚本一致 | S-STAGE-004 |
| drop 后拒绝 apply | drop 保持工作区不变；之后的 apply 被拒绝且无效果 | S-STAGE-005 |
| 选择性 apply 只影响所选子树 | `apply --path P` 只改 P 及其后代；两个覆盖全部改动的选择性批次等价于一次 `--all` | S-STAGE-006 |
| 重复 apply 无额外效果 | 对已应用的路径再次 apply 不改变工作区 | S-STAGE-007 |
| 冲突保留外部改动 | 目标是 apply 前被外部修改、新建或删除时，apply 被拒绝，外部状态完整保留 | S-STAGE-008 |
| 冲突批次整体无效果 | 所选路径中任一冲突时，本次 apply 不改变任何所选路径 | S-STAGE-009 |
| 未触碰路径的外部改动不阻止 apply | Agent 未触碰的路径被外部改动时，外部内容保留，且不阻止 apply | S-STAGE-010 |
| 删除目录保留外部新增 | Agent 删除了一个目录、外部写入者随后在其中新增文件时，apply 被拒绝 | S-STAGE-011 |
| apply 不越出工作区 | 暂存后某目录被替换为指向外部的链接时，apply 被拒绝，两边目录树都不变 | S-STAGE-012 |
| 返回值与效果一致 | 链接创建成功就留下链接；失败则不留下 | S-STAGE-013（见下方 macOS 已知缺口） |
| 可执行位与链接目标保留 | 可执行位与符号链接目标都被保留，且不解除链接 | S-STAGE-014 |

这些用例是**未经人工审核的草稿**。测试通过只说明实现满足脚本，不代表语义已评审。见[测试与 semspec](../community/testing.md)。

## 已知缺口

- **S-STAGE-013 在 macOS 上是 XFAIL**：macFUSE 在创建符号链接时可能返回 EPERM，尽管链接已创建。见[已知限制](../security/known-limitations.md)。
- **重命名表现为删除加新增**：审查清单没有单独的 rename 表示。
- **多文件 apply 对外部编辑器不是原子的**：冲突检查先于写入；apply 期间应停止其他写入者。中断后，`apply-ledger.json` 让已准备的批次向前恢复。

## 不可逆的部分 {#不可逆的部分}

暂存只覆盖工作区文件。以下影响绕过它，`apply` 和 `drop` 都无法撤销：

- 远程 API 调用、数据库写入、已发出的消息等外部影响；
- 通过 `--mount SOURCE:write` 显式共享的宿主路径；
- 已经 apply 的批次：drop 不会撤销它们。

HOME、VM 根目录等其他位置的写入见[暂存与存储](../reference/cli.md#暂存与存储)。
