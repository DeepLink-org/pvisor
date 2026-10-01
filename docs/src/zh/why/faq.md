# 常见问题

## pVisor 和 Agent 自带的沙箱有什么不同？

Agent 自带的沙箱回答"能不能挡住"。pVisor 还回答另外三件事：到底改了什么（暂存后的改动清单），哪些该留下（按路径选择性合入、冲突时拒绝覆盖），以及有什么可核对的记录（实际生效的限制与被拦下的访问）。它对 Claude Code、Codex 和任意脚本使用同一个入口和同一套语义。见[对比](comparisons.md)。

## 和 Docker 加 `git diff` 有什么区别？

Docker 提供隔离环境，`git diff` 能看到改动。但冲突时拒绝覆盖你的修改、按路径分批合入并在中断后恢复、记录实际生效的网络与文件控制、敏感路径的默认保护，这些都要自己搭。pVisor 把它们做成了有语义规格保证的能力，见[暂存与 apply 语义](../concepts/staging.md)。

## 会拖慢 Agent 吗？

目前只有 VM 执行器 guest 初始化阶段的数据（Apple M4/HVF 上就绪 p50 约 114 ms），见[启动延迟](../benchmarks/startup.md)。文件系统开销和端到端任务开销的测量还在建设中，见[基准与对比](../benchmarks/index.md)。

## 支持哪些 Agent？

任何命令都可以运行。Claude Code、Codex、Gemini CLI 和 ZCode 有 `--safe` 预设，会自动放行各自的模型 API；其他命令默认拒绝出站，需要显式授权。见[接入你的 Agent](../guides/agents/index.md)。

## 数据会离开本机吗？

pVisor 不收集或上传使用数据，Run Bundle 和捕获记录都保存在本地；它只在你使用 VM 镜像等功能时下载镜像或固件。Agent 的网络访问由你的策略决定：`--safe` 只放行对应 Agent 的模型 API。见[网络策略](../guides/policies/network.md)和[威胁模型](../security/threat-model.md)。

## Agent 能读到我的 SSH 私钥吗？

`--safe` 默认拒绝工作区视图内的 `.ssh`、`.gnupg` 与常见私钥文件，并给 Agent 独立的 HOME。但视图之外的路径由执行器边界决定：Linux host 上 overlay 规则不会隐藏视图外原路径上的秘密，需要更强保证时使用 `--filesystem sandbox` 或 VM。见[执行器边界](../security/executor-boundaries.md)。

## Agent 调用的外部 API 能撤销吗？

不能。`apply` 和 `drop` 只管理暂存的工作区文件，远程 API 调用、数据库写入和已发出的消息都不可撤销。见[暂存与 apply 语义](../concepts/staging.md#不可逆的部分)。

## 能在 CI 里跑吗？

可以运行：`pvisor run` 会原样返回命令的退出码。CI 集成的完整指南还在建设中，见[在 CI 中运行 Agent（规划中）](../guides/ci.md)。

## `last` 为什么找不到我的 Job？

`last` 只在默认存储中查找属于当前工作区的 Job。用 `--stage PATH` 时，Job 存储在暂存目录里，请把该路径或 Job ID 传给后续命令。见 [Job 与存储](../concepts/jobs.md)。
