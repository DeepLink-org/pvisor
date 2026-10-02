---
status: todo
search:
  exclude: true
---

# 退出码与错误

!!! warning "规划中"
    本页尚无完整参考。已知行为：`pvisor run` 原样返回命令的退出码；`--strict` 在缺少强制证据时以 `UnsupportedPolicy` 拒绝运行。

## 要回答的问题

`pvisor` 各子命令在什么情况下返回什么退出码？pVisor 自身的错误如何与被运行命令的退出码区分？

## 需求

- 列出每个子命令的退出码与含义；
- 列出主要错误类型（策略不支持、隔离安装失败、apply 冲突、Job 未找到等）及对应的退出码和提示文字；
- 说明 CI 中如何区分"Agent 失败"和"pVisor 拒绝运行"。

## 验收标准

- 退出码表与代码中的错误类型一一对应，并有测试覆盖。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[在 CI 中运行 Agent（规划中）](../guides/ci.md)

## 当前退出行为

| 情况 | CLI 行为 |
| --- | --- |
| 被运行命令正常结束 | `run` 返回该命令退出码；成功通常为 0 |
| Run 取消 | 返回 130 |
| 执行失败但没有命令退出码 | 返回 1 |
| 内部 host sandbox 安装失败 | 启动器使用 125；外层错误也可能以 1 返回，需结合诊断 |
| 参数解析错误 | clap 返回 2 |
| `status` / `apply` / `drop` / `kill` 成功 | 返回 0 |
| 上述命令发生解析后的运行错误 | `anyhow` 错误返回 1，stderr 说明原因 |
| `inspect` 中的命令结束 | 返回该检查命令的退出码 |
| 伴随工具 | 派发保留其退出码；replay 成功与质量还要查看结果协议 |

应用冲突、Job 未找到与 UnsupportedPolicy 目前没有独占的数字退出码。工作负载自身也可能返回 1、2、125 或 130，所以数字不能单独区分“Agent 失败”和“pVisor 拒绝”。

## 在自动化里判断

保存 stderr 和执行返回值，然后按明确的 stage 路径查找 Run Bundle。Bundle 中 `run.state`、`run.exit_code` 和 `run.failure` 提供执行结果；准入或准备阶段失败可能尚无完整 Bundle，应记录为基础设施/启动失败，不能当成“无改动的成功”。

不要因 Agent 返回 0 自动 apply。apply 有独立的冲突与恢复路径，其返回值也必须检查。超时、取消和非零退出都可能留下可审查改动；流程见[CI](../guides/ci.md)。
