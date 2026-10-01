---
status: todo
search:
  exclude: true
---

# 网络开销

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

经过 pVisor 代理或 VM 数据面的请求会慢多少？

## 需求

- 指标：请求延迟、吞吐、连接建立耗时
- 对照组：直连；Docker 网络
- 工作负载：小请求大量并发；大文件下载；LLM 流式响应
- 环境：覆盖 host 代理、deny-all、VM smoltcp

## 验收标准

- 三条路径分别有数据
- 流式响应延迟可复现
- 明确协作式与强制边界的差别

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
