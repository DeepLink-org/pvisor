# 单机多 Agent 并行与批量审查

让多个 Agent 并行解决任务时，每个任务使用独立工作区和独立 Stage。这样每份结果都有自己的基线、日志和改动，评审时不会把两个 Agent 的输出混在一起。

从 Git 仓库根目录准备两个工作区：

```bash
git worktree add --detach ../pvisor-task-a HEAD
git worktree add --detach ../pvisor-task-b HEAD
```

分别进入这两个目录，使用下面的命令启动任务。两个 Stage 都放在对应工作区之外。先从两个并发任务开始，按[并发密度实验](../benchmarks/density.md)测量资源，再增加并发。

## 现有工具能完成的手工方案

在两个独立 checkout/worktree 中各运行一个 Job，每个任务使用项目外的独立 stage 和明确选择器。这样 lower 不会因另一任务 apply 而变化，结果可以分别审查；网络和凭据仍由每个 Job 的策略决定。

```bash
# Terminal A / workspace A
pvisor run --safe --stage ../stage-a -- codex
# Terminal B / workspace B
pvisor run --safe --stage ../stage-b -- claude

pvisor status --review ../stage-a
pvisor status --review ../stage-b
```

这不是批量调度接口。选择一个结果后，按各自 workspace 合入并用现有 Git 流程集成；不要将多个任务的 upper 目录拷贝合并，也不要并发 apply 到同一目标树。

## 同一工作区与分叉的限制

从同一已停止 Job fork 可以保留共同的暂存起点，但检查点不冻结所有 lower。第一条分支 apply 后，另一条分支与宿主可能发生冲突；这种拒绝是保护机制，不能通过删原像绕过。要复现相同输入，应固定 workspace 基线、工具版本和镜像摘要。

## 运行数量由什么决定

先从两个任务验证，再记录进程、FUSE 挂载、VM 内存、磁盘、模型服务额度和代理端口占用。每个 Job 使用不冲突的端口；没有数据时不发布 8/32/128 的容量承诺。资源限制的实际效果读 Bundle；容量测试见[并发密度](../benchmarks/density.md)。

## 两个离线任务跑通批量流程 {#batch}

先用 shell 任务验证两个工作区和两份记录能独立运行。从原仓库目录启动下面的脚本；它使用前面创建的两个 worktree，两个任务分别创建自己的文件。此示例需要 Bash 和能够运行 safe 任务的宿主。

```bash
(
  cd ../pvisor-task-a
  pvisor run --safe --overlaynet-deny-all --stdio capture \
    --stage ../stage-batch-a -- /bin/sh -c 'printf "A\n" > result-a.txt'
) &
pid_a=$!
(
  cd ../pvisor-task-b
  pvisor run --safe --overlaynet-deny-all --stdio capture \
    --stage ../stage-batch-b -- /bin/sh -c 'printf "B\n" > result-b.txt'
) &
pid_b=$!
code_a=0
code_b=0
wait "$pid_a" || code_a=$?
wait "$pid_b" || code_b=$?
printf 'A=%s B=%s\n' "$code_a" "$code_b"

pvisor status --review ../stage-batch-a
pvisor status --review ../stage-batch-b
pvisor inspect ../stage-batch-a -- cat result-a.txt
pvisor inspect ../stage-batch-b -- cat result-b.txt
```

预期两个任务的退出码都是零，暂存文件分别是 A、B，两个 worktree 中还没有这些新文件。评审后接受 A、放弃 B：

```bash
pvisor apply ../stage-batch-a --path result-a.txt
pvisor drop ../stage-batch-b
git -C ../pvisor-task-a diff --stat
git -C ../pvisor-task-a status --short
```

A 的文件写回它自己的 worktree；新增文件会显示在 `git status`，尚未加入 Git 索引时不会出现在普通 `git diff`。需要集成到主分支时，在该 worktree 完成测试和提交，再走项目的合并流程。

替换为 Agent 时保留每个任务自己的 Stage，按 [Codex](agents/codex.md)或 [Claude Code](agents/claude-code.md)设置凭据与网络。先将每份结果映射到 workspace，再逐份评审和 apply；同一目标树的合入顺序由你的流水线控制。
