# 对比：Agent 自带沙箱

Agent 自带沙箱适合控制单个 Agent 的工具权限；pVisor 的额外价值是让不同 Agent 共用暂存、审查、冲突保护和执行记录。两者可以叠加。性能选择应看[任务开销](agent-tasks.md)与[文件系统开销](filesystem.md)，本页不提供未经同机测量的产品速度排名。

## 比较范围

2026-10-04 核对官方文档。本机安装 Claude Code 2.1.128、Codex CLI 0.160.0；Gemini CLI 未安装。下表比较文档中的能力，不能推断这些安装版本支持官方最新页面上的每个选项。受控 CLI 实验的版本和参数见[任务报告](agent-tasks.md)。

| 方案 | 执行边界与网络 | 工作区修改 | 何时选它 |
|---|---|---|---|
| Claude Code sandbox | 对 shell 命令及子进程施加 OS 边界，macOS Seatbelt、Linux bubblewrap；网络代理检查域名。内置文件工具、MCP 和 hooks 有独立权限机制 | 允许目录中的修改直接发生；命令审批与事后文件合入是不同流程 | 以 Claude Code 为唯一入口，需要成熟的交互式权限配置 |
| Codex sandbox | `read-only` / `workspace-write` / `danger-full-access`；沙箱限制与审批策略独立，Linux bubblewrap、macOS Seatbelt | workspace-write 内直接编辑；可用 worktree 管理文件并行 | 主要使用 Codex，需要与其审批、规则和会话紧密结合 |
| Gemini CLI sandbox | 支持 Seatbelt、Docker/Podman、runsc 等配置；边界取决于选定运行时和配置 | 容器默认挂载工作区，写入挂载目录会作用于相应文件 | 主要使用 Gemini，希望复用其工具与镜像配置 |
| pVisor | host、隔离 host、OCI、libkrun VM；host proxy 与 VM TCP 数据面的边界不同，查看实际 Run Bundle | staged 写入在 apply 前保留；按路径合入，原工作区变化触发冲突保护 | 多 Agent 共用执行协议，或需要在修改抵达工作区前审查 |

前三行分别依据 [Claude Code sandbox](https://code.claude.com/docs/en/sandboxing)、[Codex sandbox](https://learn.chatgpt.com/docs/sandboxing)、[Gemini CLI sandbox](https://geminicli.com/docs/cli/sandbox/)。pVisor 行依据[执行器](../guides/executors/index.md)、[审查与合入](../guides/review-apply.md)及[隔离实测](isolation-tests.md)。

## 审查与证据

上述自带沙箱的官方页面介绍权限限制和审批，没有定义 pVisor 的 stage/preimage/apply ledger 协议；这不意味着对应产品没有会话日志、diff 或 Git 工作流。pVisor 将执行结果、实际限制和阶段产物写入 Run Bundle，并用 stage 保存可选择的改动。网络与文件暂存也独立配置，单独使用 `--stage` 不会自动限制宿主外部路径。

代表性流程是：读取仓库、修改文件、请求模型 API，随后宿主修改同一文件。pVisor 的写入与冲突实验见 [apply/drop](apply.md)，CLI 的工具回路见[任务开销](agent-tasks.md)。本版未用 Gemini CLI 执行该流程，也未测各产品内置沙箱的全链路开销；能力比较与性能实验分别标注。

## 完整环境兼容性实测 {#reference-comparison}

本节的 Docker/CLI 数据来自相同工具制品的旧受控批次，pVisor 使用准备目录，不使用镜像。新增默认 `--rootfs host` 与完整 Ubuntu 对照、工具内部时间及本轮客户端通过/失败情况见[完整 Agent Env](agent-tasks.md#full-ubuntu)；配置与样本分别保留。

真实 Claude/Codex CLI 已在 native、staged、Docker、Firecracker、QEMU 完成受控修复测试闭环；Codex 也在 pVisor VM 30/30 通过，Claude/VM 在初始化阶段超时。这不能证明所有客户端支持 pVisor VM。Codex 内部使用统一 `danger-full-access`，Claude 只开放受控 Bash 动作；本轮没有比较各客户端默认沙箱，也没有验证默认内外沙箱叠加。实际耗时、失败与外层边界见[完整工具环境](agent-tasks.md#reference-env)。


## 更正

若某项能力描述不准确，请在 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 提供产品版本、配置、官方链接或可复现命令。比较对象升级后，应追加日期与证据。
