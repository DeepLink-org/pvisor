# 审查并应用改动

在项目目录中启动暂存运行。把暂存目录放在项目外，每次 Run 使用新目录：

```bash
pvisor run --stage ../stage-001 -- codex
pvisor review last
pvisor inspect last -- git status --short
```

没有 `--stage` 等 OverlayFS 选项时，host Run 可以直接修改真实项目。`review` 展示记录的证据和暂存改动；`inspect` 在只读视图中执行检查命令。

## 分批应用

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
```

每个成功批次写入原工作区，只消费本次选中的改动。其余改动继续保留，之后可以再次应用。不透明目录和硬链接组存在依赖时，必须一起选择。

应用前，pVisor 将目标与记录的原始状态比较。递归删除和目录替换也会检查已记录的子路径。覆盖范围内的文件被其他进程改动后，apply 会报冲突，避免覆盖新内容。应用期间应停止其他写入者：这些检查不能让多文件更新相对于外部编辑器成为原子操作。

`apply-ledger.json` 持久记录批次进度，用于中断后的恢复。恢复只接受与预期结果一致的已应用内容；目标出现其他变化时仍然报错。重试前先检查冲突并保留外部改动。

## 保存检查点或丢弃剩余改动

在全部应用或丢弃暂存内容之前：

```bash
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
```

CLI 检查点要求 Run 已停止。它保存文件系统上层和派生关系，不保存进程内存，也不是所有底层宿主文件的不可变快照。

```bash
pvisor drop last
```

丢弃只移除尚未应用的暂存改动，不能撤销已应用批次、网络调用或其他外部影响。需要跨命令复用工作区时，参见 [CLI 参考中的 `env`](../reference/cli.md)。
