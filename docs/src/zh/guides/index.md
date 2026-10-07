# 任务指南

把一次任务拆成运行、审查和发布三个步骤：命令在选定执行环境中完成，候选文件保留在 stage，审查后才写回工作区。先按[第一次运行](../start/first-run.md)准备能运行暂存 Job 的宿主环境，并确认脚本或 Agent 已安装。

## 固定输入与访问范围

从任务的项目目录启动，为每次 Run 使用项目外的新 stage。固定仓库基线；使用镜像时固定摘要。按命令需要选择 host、container 或 VM，分别配置文件访问、网络目标和凭据；单独启用 stage 不会限制读取或网络。

完全离线的脚本可以从下面的命令开始；项目中的 `task.sh` 应已有执行权限：

```bash
pvisor run --safe --overlaynet-deny-all --stage ../task-stage-001 -- ./task.sh
pvisor status --review --diff ../task-stage-001
pvisor inspect ../task-stage-001 -- git status --short
```

替换为联网 Agent 时，按[网络策略](policies/network.md)选择出口边界并声明目标，按[凭据与环境变量](policies/credentials.md)交付凭据。

## 先审查，再发布

Job 停止后，依次检查结果与退出码、实际控制和警告、拒绝或失败的访问、净文件改动。`inspect` 要求 OverlayFS 工作区，在只读视图中使用宿主工具；文本 diff 被截断或文件是二进制时，另行检查内容。

按任务要求选择路径，用 `apply --path` 分批合入，未选候选继续保留；全部合入或 drop 后清理一次性暂存数据。apply 期间停止其他写入者；冲突与中断恢复按[审查与应用](review-apply.md)处理。drop 仅丢弃未合入的文件改动，不能撤销已合入批次或外部服务调用。

## 把闭环接进自动化

并行任务各用独立 workspace、stage 和明确的 Job selector，不并发 apply 到同一目标树。CI 保留 stage 与运行证据，把运行退出码和上传产物分开处理；评审步骤决定合入。评测任务先检查执行结果，再读取候选文件评分，完成后丢弃不需发布的改动。

需要从文件状态重新尝试时，先按[后续步骤](../start/next-steps.md)检查工作区 fork 的前提，再在全部 apply/drop 之前分叉。命令语法以 [CLI 参考](../reference/cli.md)为准。
