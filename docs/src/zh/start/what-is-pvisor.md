# PolicyVisor 是什么

**PolicyVisor（pVisor）让 Agent 全自动执行，文件改动由你决定去留。**

它运行你现有的 Agent CLI、脚本或自动化命令：命令在策略边界内无人值守地跑完，文件改动先进入暂存区；结束后你像审 PR 一样查看改动和证据，只合入想留下的部分。

不少 Agent CLI 已经自带沙箱或审批模式。它们回答的是"能不能挡住"，没有回答"到底改了什么、哪些该留下、有什么可核对的记录"，也不跨 Agent、跨执行器。为什么这三件事决定了 Agent 自主能扩展到多大，见[为什么是 pVisor](../why/index.md)；与 Docker、Agent 自带沙箱等方案的逐项对比见[对比](../why/comparisons.md)。

## 你会得到什么

- **放手**：不用守着批准弹窗。Agent 的文件改动先进暂存区，网络与敏感路径按策略约束。
- **把关**：像审 PR 一样查看改动，选择合入哪些路径，其余一键丢弃。若你期间改了同一个文件，pVisor 拒绝覆盖你的修改。
- **有据**：每次运行留下一份可核对的记录，包括实际生效的限制、被拦下的访问，以及可选的模型请求。

## 三条命令

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

!!! tip "先跑一次不需要 API Key 的演示"

    [第一次运行](first-run.md)用一个"假 Agent"脚本演示完整闭环：它修改源码、删除文件、尝试读取敏感路径和访问外网，你在审查中看到改动与拦截记录，最后只合入 `src`。

## 今天能做到哪一步

今天 pVisor 在本机逐个 Job 运行：让一个 Agent 全自动跑完，事后审查每个改动并选择性合入。这一级已经具备通往更高自主级别所需的三种性质，路线见[信任阶梯](../why/trust-ladder.md)。各执行器的边界见[执行器边界](../security/executor-boundaries.md)。

## 保证范围

一次运行的边界以它的能力证据为准；`apply` 和 `drop` 只管理暂存文件，不撤销外部副作用。完整口径见[能力、证据与保证边界](../concepts/capabilities-and-evidence.md)，威胁模型见[安全](../security/index.md)。
