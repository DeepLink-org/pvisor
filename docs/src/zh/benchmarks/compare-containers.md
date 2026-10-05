# 已有 Docker 或 devcontainer，还需要 pVisor 吗？

## 主要结论 {#conclusions}

**Docker bind mount 的文件访问接近原生，优于 pVisor 的暂存路径；pVisor staged 的已测短修复任务则与 Docker 接近且略快。** 同工具环境修复/测试为 staged **0.70 s**、Docker **0.90 s**；pVisor VM 为 **3.97 s**，明显更慢。pVisor 的选型价值是统一保留改动、冲突检查与选择性合入，不能以“全面比 Docker 快”概括。

已有可靠 Docker + worktree/Git 审查流程时，可以继续沿用；需要多个 Agent 或非 Git 目录共用合入协议时，staged 值得评估。

| 需求 | 选型含义 |
|---|---|
| 已有 Docker + worktree/Git | 保留既有流程，按实测评估工具成本 |
| 跨 Agent 统一暂存与选择性合入 | 评估 pVisor host staged |
| 要求独立 guest kernel | 对比 VM 执行成本 |

## Motivation {#motivation}

容器提供工具环境，但工作区怎样挂载决定改动是否即时抵达宿主。比较速度时，审查、导出、冲突处理和合入所需的流程也影响最终成本。

## 实验设计 {#interpretation}

Linux 同机、两核预算、相同 Python/Node/Rust/Agent 工具与输入；Docker Engine 29.7.2 rootless、镜像和 daemon 已准备，使用 writable bind mount。每格 3 次预热、30 次测量；完整任务包含启动到校验结果，文件操作不含启动。数据使用报告固定的 pVisor 制品，未与当前文件系统集成制品全面重测。Docker Desktop、devcontainer 插件和 overlay2 工作负载未测。

| 配置 | 修改位置与审查方式 |
|---|---|
| Docker writable bind mount | 挂载的宿主文件直接变化；可另用独立 worktree |
| Docker writable layer / volume | 修改在层或卷；通过导出、补丁或提交合入 |
| devcontainer | 按配置挂载或使用卷，可组合 Git/PR 审查 |
| pVisor staged | stage 保留改动，apply 前原目录不变；按路径合入及 preimage 冲突检查 |

Docker 默认 bind 写入宿主的语义见[官方说明](https://docs.docker.com/engine/storage/bind-mounts/)，devcontainer 配置见[开放规范](https://containers.dev/)。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

### 同工具任务 {#reference-comparison}

| 操作 | pVisor staged P50 | Docker P50 | pVisor VM P50 |
|---|---:|---:|---:|
| 修复并测试 | 0.70 s | 0.90 s | 3.97 s |
| Claude 受控工具闭环 | 1.07 s | 1.23 s | 初始化超时 / N=0 |
| Codex 受控工具闭环 | 2.25 s | 6.26 s | 10.93 s |
| 读取并校验 64 MiB | 48.77 ms | 33.24 ms | 89.27 ms |
| 遍历 2,048 文件 | 180.13 ms | 5.06 ms | 310.54 ms |

短修复任务 staged 少约 0.20 s；但逐文件操作中，Docker 接近原生，staged 的元数据成本更高。客户端初始化和工具组合会改变总体结果，单项文件速度不能替代完整任务。CLI 使用受控响应，排除模型推理，不是默认内置沙箱对照。

pVisor VM 提供独立 guest kernel，staged host、Docker namespace 和 VM 的边界并不相同。选择要结合[隔离验证](isolation-tests.md)和[apply 成本](apply.md)。当前文件系统数据见[文件系统性能](filesystem.md)，不将其与这里的 Docker 数字混算精确倍数。

[完整任务与分布](agent-tasks.md#reference-env) · [同工具文件数据](filesystem.md#reference-fs) · [协议与制品](methodology.md#reference-env)
