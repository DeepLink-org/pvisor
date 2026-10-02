---
status: todo
search:
  exclude: true
---

# 对比：Agent 自带沙箱

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

与 Claude Code、Codex、Gemini CLI 自带沙箱相比，差在哪？

## 需求

- 指标：拦截方式、事后选择性合入、冲突保护、证据记录、跨 Agent 一致性、网络控制、性能开销
- 对照组：Claude Code 沙箱；Codex 沙箱与审批模式；Gemini CLI 沙箱
- 工作负载：一次代表性任务（读取、写入、访问模型 API）
- 环境：固定各产品版本，写明日期

## 验收标准

- 每项结论附出处（官方文档或可复现测试）
- 写明对方擅长的地方与何时选它
- 提供「更正」入口

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/comparisons、benchmarks/methodology
