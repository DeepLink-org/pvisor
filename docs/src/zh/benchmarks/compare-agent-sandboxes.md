# Agent 自带沙箱何时够用，何时需要 pVisor？

## 主要结论 {#conclusions}

**使用单一 Agent 时，其内置沙箱可直接管理工具权限；pVisor 适合跨 Agent 统一暂存、审查、冲突保护与记录。** 两者可组合，但默认双层沙箱的兼容性和完整开销没有本章实测结论。

受控任务中，staged 的 Claude/Codex 闭环为 **1.07/2.25 s**，接近原生 **0.82/1.97 s**；VM 的 Codex **10.93 s**，Claude 初始化超时。这是执行环境对照，不是内置沙箱产品速度排名。

| 需求 | 选型含义 |
|---|---|
| 单一 Agent 的工具权限 | 可使用内置沙箱 |
| 多个 Agent 共用改动审查协议 | 评估 pVisor |
| 默认双层沙箱 | 需独立兼容性测试 |

## Motivation {#motivation}

权限限制控制动作能否执行；审查与合入控制改动何时进入原目录。用户需要根据这两个需求选择内置沙箱、独立 worktree 或 pVisor stage。

## 实验设计 {#interpretation}

能力依据官方文档，实测使用固定 CLI 版本、同工具环境、受控响应与相同修复计划，各可用格 30 次。Codex 内层统一为 `danger-full-access`，外层环境提供声明边界；没有比较默认内置沙箱。Gemini CLI 未执行本机任务。

| 方案 | 权限与工作区方式 |
|---|---|
| Claude Code | 沙箱限制 Bash 及子进程的文件/网络访问；其他工具具有各自权限机制 |
| Codex | 沙箱边界与审批策略分别配置；workspace-write 可在允许工作区内直接编辑 |
| Gemini CLI | 可选 OS 或容器沙箱；Docker/Podman 方式挂载工作区 |
| pVisor | 选择 host/隔离 host/OCI/VM；stage 保留改动，apply 时检查原像冲突 |

出处：[Claude Code](https://code.claude.com/docs/en/sandboxing)、[Codex](https://developers.openai.com/codex/security/)、[Gemini CLI](https://geminicli.com/docs/cli/sandbox/)。pVisor 边界见[隔离实测](isolation-tests.md)。官方当前能力与实测固定版本分别标识。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

### 已测 CLI 闭环 {#reference-comparison}

Claude 在 native/staged/Docker/参考 VM 上各 30/30 通过，pVisor VM 初始化超过 90 s，正式 N=0。Codex 全部八组各 30/30 通过。固定响应排除了推理时间；结果只证明指定工具路径和版本可用。

暂存适合先审查再合入的流程：执行后可以按路径 apply/drop，宿主并行修改相同文件时拒绝冲突。内置沙箱也可组合 Git/worktree，不因缺少 pVisor 协议而缺少审查能力。单独 stage 不自动限制宿主视图外访问。

[任务数据与兼容性](agent-tasks.md#reference-env) · [合入成本](apply.md) · [执行器边界](../guides/executors/index.md)
