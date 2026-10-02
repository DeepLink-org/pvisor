---
status: todo
search:
  exclude: true
---

# 文件系统开销

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

在 pVisor 里跑 cargo build / npm install 会慢多少？

## 需求

- 指标：元数据操作延迟、读写吞吐、典型任务耗时比
- 对照组：原生文件系统；Docker bind mount；overlay2
- 工作负载：大仓库 git status、npm install、cargo build、ripgrep 全仓搜索
- 环境：分 Linux FUSE、macFUSE、FSKit 三组

## 验收标准

- 给出相对开销百分比与 p50/p95/p99
- 三组环境分别有数据
- 脚本可一条命令复现

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
