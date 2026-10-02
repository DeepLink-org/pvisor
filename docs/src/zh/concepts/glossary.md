# 术语表

| 术语 | 含义 |
| --- | --- |
| [Job](jobs.md) | pVisor 里一项持久的工作：命令、证据与暂存改动 |
| [Stage](staging.md) | 写时复制的暂存工作区 |
| [apply / drop](staging.md) | 把暂存改动选择性合入目标／丢弃 |
| [Run Bundle](../reference/run-bundle.md) | 一次运行的结果、控制观察、产物与摘要 |
| [Capability](capabilities-and-evidence.md) | 一个能力维度（文件、网络、子进程…）的请求与实际控制 |
| [Evidence](capabilities-and-evidence.md) | 执行器实际安装的控制与观察到的结果，而不是声明 |
| [Placement](../design/operations-events.md) | 选定的执行位置（host／container／VM，以及 Overlay 组合） |
| 控制计划等级 | 准入时执行器计划安装的等级：`Unsupported`、`Cooperative`、`Planned`，不等于已安装 |
| 实际控制等级 | 执行器收尾观察到的等级：`Unenforced`、`Cooperative`、`Enforced` |
| [Interception](../guides/policies/network.md) | 网络层对出口流量的拦截（显式代理或 VM 数据面） |
| [L0–L3](../why/trust-ladder.md) | 信任阶梯的级别 |

机制与字段见[设计与研究](../design/index.md)和[参考](../reference/index.md)。
