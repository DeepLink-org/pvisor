# 审查并应用改动

在项目目录中启动暂存运行：

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor inspect last -- git status --short
```

`--safe` 不指定路径也会把工作区改动保留在 Job 存储中，`last` 解析当前工作区最新的 Job。需要指定位置时用 `--stage PATH`（放在项目外，每次 Run 使用新目录），之后的命令传入该路径而不是 `last`，见 [Job 与存储](../concepts/jobs.md)。`status --review` 展示证据和改动；`inspect` 在只读视图中执行检查命令。

## 分批应用

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
```

每个成功批次写入原工作区，只消费本次选中的改动。其余改动继续保留，之后可以再次应用。不透明目录和硬链接组存在依赖时，必须一起选择。

应用前，pVisor 将目标与记录的原始状态比较。递归删除和目录替换也会检查已记录的子路径。覆盖范围内的文件被其他进程改动后，apply 会报冲突，避免覆盖新内容。应用期间应停止其他写入者：这些检查不能让多文件更新相对于外部编辑器成为原子操作。每条保证对应的语义规格见[暂存与 apply 语义](../concepts/staging.md)。

`apply-ledger.json` 持久记录批次进度，用于中断后的恢复。恢复只接受与预期结果一致的已应用内容；目标出现其他变化时仍然报错。重试前先检查冲突并保留外部改动。

## 丢弃剩余改动

```bash
pvisor drop last
```

丢弃只移除尚未应用的暂存改动，不能撤销已应用批次、网络调用或其他外部影响。丢弃之前想保留当前状态继续尝试，见[逻辑检查点与分叉](fork-checkpoint.md)。

## 审查顺序与预期结果

先确认 Job 已停止，再按以下顺序阅读：结果与退出码 → 实际文件/网络边界和警告 → 拒绝或失败的访问 → 净文件改动。允许规则和捕获记录不能代替强制力证据。

```bash
pvisor status --review --diff ../stage-001
pvisor inspect ../stage-001 -- git diff -- src
pvisor apply ../stage-001 --path src
pvisor status --review ../stage-001
```

成功的选择性 apply 后，宿主 `src` 应出现所选改动，审查清单里其他未应用路径仍保留。只看 `changed_files` 不能判断改动质量；文本 diff 有总字节数和单文件大小限制，二进制文件或被截断的内容需另行检查。

## 冲突后怎么处理

遇到冲突时先保留当前工作区和 stage，不删除原像或手改 ledger。比较宿主当前文件、暂存文件与原任务要求，决定保留哪一份。要把两边的改动合在一起，应在新的 Job 或常规 Git 工作流里明确解决，再重试；pVisor 不会自动做三方合并。

drop 前记录最终决定。已 apply 的批次不能被 drop 回滚；全部 apply 或 drop 会清理一次性暂存数据，所以需要 fork 时应先 fork。
