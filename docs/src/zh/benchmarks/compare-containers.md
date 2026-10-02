---
status: todo
search:
  exclude: true
---

# 对比：Docker / devcontainer

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

Docker 加 git diff 不就够了？

## 需求

- 指标：隔离强度、改动审查、冲突保护、选择性合入、证据、启动与文件系统开销、本地工具链可用性
- 对照组：Docker；devcontainer；Docker 加 git diff
- 工作负载：一次代表性任务，含冲突场景
- 环境：固定镜像摘要

## 验收标准

- 正面回答「Docker 加 git diff」的缺口
- 引用 benchmarks/startup 与 filesystem 数据
- 提供「更正」入口

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/comparisons、benchmarks/methodology
