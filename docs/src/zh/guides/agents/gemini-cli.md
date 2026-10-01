---
status: todo
search:
  exclude: true
---

# Gemini CLI

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

如何在 pVisor 边界内运行 Gemini CLI 并控制其网络与文件访问？

## 需求

- 指标：接入步骤可复现、注入方式正确、实际控制有证据
- 对照组：Agent 自带沙箱或审批模式
- 工作负载：一次代表性任务（读取、写入、访问模型 API）
- 环境：固定 Agent 版本；平台相关需注明

## 验收标准

- 给出可复制的接入命令与验证步骤
- 写明注入方式与已知限制
- 受支持版本固定并有回归

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：guides/agents/index、reference/platforms
