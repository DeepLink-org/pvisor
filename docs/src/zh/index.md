---
template: home.html
title: 面向 Agent 工作负载优化的容器运行时
description: 面向 Agent 工作负载的容器运行时。每个容器使用独立内核，多个 VM 可共享重复内存页，惰性镜像让工具不必等完整下载即可启动。
hide:
  - navigation
  - toc
---
## 面向 Agent 工作负载优化的容器运行时

用熟悉的 Docker CLI 命令运行容器，用 Agent 任务命令审查文件改动。

- [更好的隔离性](security/executor-boundaries.md)：每个容器使用独立的 Linux 内核，减少 Agent 运行工具时对宿主和其他任务的影响。
- [更节约资源](benchmarks/vm-memory/index.md)：启用内存页共享后，多个 Agent 使用同一套工具时，宿主不必为相同的页保留多份副本。
- [提速智能体应用](benchmarks/lazy-image-startup.md)：惰性镜像按需加载内容，Agent 不必等整个镜像下载完就能开始运行工具。
