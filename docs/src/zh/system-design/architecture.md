# 端到端架构

本文定义 pVisor 内部的契约。Provider 机制属于 pVisor Design，命令属于 pVisor Reference。

![Persisting 产品域与集成关系](../../assets/diagrams/persisting/system-products.svg)

## 产品 Ownership

| 产品或层次 | 拥有 | 不拥有 |
| --- | --- | --- |
| `persisting-events` 契约 | 存储无关的 `EventRecord` identity/envelope | 存储引擎、查询或 projection |
| pVisor | 一个 Run、Attempt、执行环境、capability admission、Effect 与运行时 Evidence | 多 Run 调度 |
| Runtime Provider | 一种物理执行机制 | 逻辑 Run identity 或产品策略 |

Gateway、OverlayFS 和 OverlayNet 是 pVisor 运行时机制，不构成独立控制面。

## 运行位置与平台边界

逻辑 Run 契约可以跨 Provider 迁移，但 enforcement 边界取决于选中的平台：

| 运行位置 | 工作负载边界 | Workspace 行为 | 安全声明 |
| --- | --- | --- | --- |
| Linux host | 私有 user/mount/PID namespace 与 Landlock | staged FUSE workspace | filesystem 与 network capability 分开报告；准备失败时在执行前失败 |
| macOS host | 可用时使用 Seatbelt，并用 staged macFUSE 处理写入 | staged host workspace | safe best-effort host 隔离；host kernel 与 ambient read 仍记录在 Evidence 中 |
| Linux 或 Apple Silicon macOS VM | guest kernel 加 OCI 或准备好的 Linux rootfs | guest 内 staged workspace | kernel 边界更强，但 macOS VMM 仍以调用用户的 host 权限运行 |
| 原生 OCI container | pVisor 选择的 OCI runtime 与 bundle | bundle 挂载的 rootfs 与 staged path | 记录 container isolation；不将其视为完整的敌对多租户边界 |

Provider 会在 Run Bundle 中分别记录请求的 capability 与实际生效的维度。进程成功退出
不代表请求的边界已经安装；workspace stage 也独立于产生它的 Provider，仍可 review。
Provider 的行为与前置条件见 [pVisor 隔离设计](../pvisor/design/isolation.md) 和
[执行指南](../pvisor/guides/execution.md)。

## 独立 Ingress 路径

```text
Configured runtime capture
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> canonical event Source ──────────────┐
Pinned external Sources                                                │
  local/S3 ATIF, ACTF, OpenAI Messages files ──────────────────────────┼─> Snapshot
  local/S3 Storyline Sources ──────────────────────────────────────────┘
                                                                         └─> normalized Dataset views
```

pVisor 可以用 staged Effect 与私有、带版本的
Run Bundle 及 terminal RunResult 完成独立闭环。配置 capture 并不是 pVisor 运行时前置条件。
外部文件与 Storyline Source 会被直接固定版本并规范化；它们既不经过 pVisor，也不会变成
canonical runtime event，因此不会获得 pVisor 执行保证。

## 稳定对象

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      └── Attempt finalization
          ├── terminal RunResult
          ├── private versioned Run Bundle
          └── staged Effects → later review / apply / drop

Optional configured event handoff
  └── Gateway trajectory events + pVisor lifecycle records
```

逻辑 Run 可以迁移，Attempt 与 Provider 绑定。基础设施重试创建新 Attempt；语义重试创建
派生 Run。一个 Run 可以有多个 Attempt，但只能有一个可见终态结果。

Source 携带 `run_id` 时，它就是跨产品稳定身份。Session、Step、call、event 和 Artifact
identity 保留自己的 scope 与 Source lineage。进程 ID、Container ID、VM ID 或 worker lease
都不能代替 Run identity。

## 单 Run 路径

```text
User or Agent framework
  → RunSpec
  → pVisor admission
  → capability-by-dimension provider selection
  → Attempt execution
  → terminal RunResult + private versioned Run Bundle + staged Effects
  → later review / apply / drop
```

Admission 比较请求的 capability 维度与选中 Provider 能提供的 Evidence。必需维度无法
enforce 时，在 workload 执行前失败；可选降级必须明确写入 Run Bundle。

文件 promotion 是 Effect 决策，不是 Run 终态提交。只要 stage 仍存在，就可以多次 apply
不同路径。网络请求和远程工具修改属于不同 Effect 维度，不能从文件状态推断。

配置后，pVisor 会把 Gateway 轨迹 event，以及 `run.created`、`run.state_changed`
和终态 lifecycle record 以 EventRecord JSONL 形式写入这次 Run。这些 record 携带
Run/Attempt identity、lifecycle fact 与其中可用的 Evidence。Artifact reference、
lineage、staged filesystem Effect、AgentCtl/network/resource Evidence 和完整 Run Bundle
仍留在本地，除非另行搬运。

## Dataset 路径

Canonical runtime writer 与固定版本的外部 Source 是相互独立的 Source 路径；它们只在
Snapshot 与规范化 Dataset 视图处汇合：

```text
configured Gateway and pVisor lifecycle writers
  → canonical event Source ────────────────────────────────┐
pinned local/S3 external Sources                           │
  → ATIF / ACTF / OpenAI Messages files ───────────────────┼─> Snapshot
  → Storyline Sources ─────────────────────────────────────┘     ├─> normalized Run / Step / ToolCall views
                                                                 └─> query / export / revision lineage
```

Canonical fact 采用 append-oriented 模型。Storyline 等规范化视图是可重建 projection；交换
文件是互操作边界，不替代事实源。每次读取固定一个 Snapshot，但不会虚构跨无关
Source 的全局事务。固定外部文件不会把它转换为 canonical runtime event Source。

## Source 特定保证

| Source 路径 | 支持的主张 | 明确不主张 |
| --- | --- | --- |
| 外部文件或 imported Source | 已发现的内容、固定的 Source version、规范化表示，以及在已实现位置记录的 conversion lineage | 外部 task manifest 的完整性，或不存在未报告轨迹 |
| Gateway capture | 通过所配置 Gateway 路径观察到并持久发布的 request 与 response | 不存在绕过 Gateway 的流量 |
| pVisor Run | Run/Attempt identity、已记录终态事实、已安装机制、观察到的 Effect 与 Provider 特定 Evidence | 所选 Provider 未提供的 enforcement |

Ingestion 保留这些边界。规范化表示或 Snapshot 不会升级 Source 提供的 Evidence。

Capture 把 `EventRecord` 以本地 JSONL 写进这次 Run；本仓库不再启动单独的历史进程。
标志名见 [pVisor CLI](../pvisor/reference/cli.md)。

## 故障与恢复

| 故障 | Owner | 必需行为 |
| --- | --- | --- |
| Attempt 退出或 Provider 消失 | pVisor | finalise Evidence；暴露失败或创建 fenced replacement Attempt |
| sidecar append queue 饱和或关闭 | pVisor/Gateway producer | 提交前明确拒绝并报告失败；不得声称已经 durable |
| append 连接丢失 | producer | 保留 unknown 状态；不得按“明确拒绝”复用序号 |
| 历史发布冲突 | capture sink | 保留已发布记录；按 sink contract 报错或重试 |
| 视图生成失败 | Gateway | 保持 canonical event 可读 |

恢复不能把不确定性升级为成功。缺失终态事实、丢失 callback、未 enforce 的 capability 都必须
保持可见。

## 安全与 Evidence 链

安全性按 capability 维度报告。pVisor 记录请求策略、实际机制、Provider identity、
enforcement 结果与观察到的 Effect。配置后的 capture 会保存 Gateway 轨迹 event、pVisor lifecycle
record，以及这些 event 实际携带的 Evidence。更宽的 Run Bundle 清单仍留在本地。

下面保留 Evidence 的形成层级；它描述本地 Run 的证据链，不表示各层都会自动交接到持久历史：

```text
requested policy
  → admission decision
  → installed mechanism
  → provider-bound evidence
  → observed effects
  → terminal result

Optional configured persistence
  Gateway trajectory events + pVisor lifecycle records
    → event-carried Evidence only
    → Run 本地 capture
```

Evidence 层级见[安全与 Evidence](security-evidence.md)，可迁移要求见
[从本地到集群](local-to-fleet.md)。

## 公共边界

| 边界 | 契约 Owner | 详细文章 |
| --- | --- | --- |
| 逻辑运行事件 | `persisting-events` | [pVisor Gateway 设计](../pvisor/design/gateway.md) |
| Agent 执行与 Effect review | pVisor | [pVisor 概念](../pvisor/concepts/index.md)与[指南](../pvisor/guides/index.md) |
| Provider 与运行时机制 | pVisor | [pVisor Design](../pvisor/design/index.md) |
| 稳定命令语法 | pVisor | [pVisor Reference](../pvisor/reference/index.md) |

只有跨产品契约变化时才修改本文。产品实现状态和 roadmap 属于对应 Design 页或 Project
工程说明。
