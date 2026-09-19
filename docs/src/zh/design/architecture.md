# 架构

PolicyVisor（pVisor）通过能力准入、执行器、运行时控制和执行记录，管理 Agent CLI、脚本和自动化命令。当前交付围绕本地 Run 及其可审查的结果展开，集群调度仍属于设计方向。

## 组件职责

| Crate | 职责 |
| --- | --- |
| `persisting-pvisor` | CLI、准入、Attempt 生命周期、执行器、Run Bundle、审查／应用／检查点 |
| `persisting-control` | 运行契约、能力策略、AgentCtl 消息与客户端、共享事件记录 |
| `persisting-overlay-core` | 共享写时复制语义与首次修改时的文件指纹 |
| `persisting-overlayfs` | 宿主 FUSE 适配器与可选 Jujutsu 后端 |
| `persisting-overlaynet` | 网络授权、解析、代理转发与 VM 网络接入 |
| `persisting-gateway` | 模型路由、协议转换与捕获 |
| `persisting-replay` | Agent 原生轨迹的回放与续跑适配 |

## 执行链路

```text
CLI／配置 → RunSpec → 能力准入 → 准备运行时驱动
  → 执行器启动命令 → 退出／取消／超时 → 清理
  → 本地 Run 记录 + Run Bundle + 最终结果
  → 后续审查／应用／丢弃
```

当前每次运行创建一个 Attempt。宿主执行在主进程退出后清理进程组，并限时排空输出，超时和取消路径也遵循此规则。主动脱离进程组的后代不在进程组清理范围内；输出排空期限可以避免其继承的管道阻塞 Run 完成。更强的后代进程隔离取决于所选平台机制。

## 文件应用

OverlayCore 在首次修改时记录目标的原始状态。Apply 将选择扩展到必要的目录和硬链接成员，校验受影响的原始状态，写入持久化意图，更新目标，然后移除已经应用的 upper 条目。

递归删除或替换目录时，除了目录本身，还校验已记录的后代。分批应用保留剩余子文件需要的目录新基线。恢复流程区分 `Prepared`、`TargetApplied` 和 `Committed`，避免把清理了一部分的 upper 当成新的改动。

单文件替换使用临时文件与 rename。整个批次不是一次原子文件系统事务：中断后可能只有部分改动可见，需要恢复。目标锁只串行化 pVisor 的 apply，不会锁住编辑器或其他写入者；应用时应停止并发写入。

## 运行记录与捕获

`run.json` 是本地 Run 记录，`run-bundle.json` 汇总结果、控制、产物及文件系统／网络证据。可选的 EventRecord JSONL 保存生命周期和 Gateway 事件，与 Bundle 的内容范围不同。

Gateway 使用有界应用队列和尽力而为的异步 WAL。进入队列不等于已同步持久化。捕获和协议转换也不能证明全部网络流量都经过了拦截。

## 扩展点与限制

执行器和 sink 接口支持嵌入集成，不代表已实现自动重试调度、外部动作恰好一次执行、分布式事务或密码学节点证明。后续部署思路见[从本地到集群](local-to-fleet.md)，平台强制执行见[隔离设计](isolation.md)。
