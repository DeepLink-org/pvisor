---
status: todo
search:
  exclude: true
---

# 在 CI 中运行 Agent

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

如何在 GitHub Actions 等流水线里让 Agent 无人值守地修问题，并把结果交给审查？

## 需求

- 指标：单次运行墙钟时间、资源占用、失败率、需要人工介入的次数
- 对照组：同一 Agent 在 CI 中直接运行
- 工作负载：用 Agent 修复失败的测试或执行例行重构
- 环境：GitHub Actions runner（Linux/macOS），固定 Agent 版本

## 验收标准

- 给出可复制的 workflow 示例，含 `--safe`、暂存路径与产物上传
- 明确 `apply` 在 CI 中的语义（谁审、何时合）
- 失败与超时路径有回归

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：guides/parallel-agents、reference/cli
