---
status: todo
search:
  exclude: true
---

# 单机多 Agent 并行与批量审查

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

在一台机器上同时跑多个 Agent 时，如何隔离、如何批量审查、单机上限是多少？

## 需求

- 指标：并发 Job 数、每个 Job 的 CPU／内存开销、尾延迟
- 对照组：顺序运行；Docker 同等密度
- 工作负载：8／32／128 个并发 Job 的固定任务集
- 环境：Linux（KVM/FUSE）与 macOS（HVF/macFUSE）各一组

## 验收标准

- 给出单机并发上限与资源模型
- 批量审查流程可复现（按 workspace 聚合、批量 apply）
- 与 benchmarks/density 对齐

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：why/use-cases、benchmarks/density

## 现有工具能完成的手工方案

在两个独立 checkout/worktree 中各运行一个 Job，每个任务使用项目外的独立 stage 和明确选择器。这样 lower 不会因另一任务 apply 而变化，结果可以分别审查；网络和凭据仍由每个 Job 的策略决定。

```bash
# Terminal A / workspace A
pvisor run --safe --stage ../stage-a -- codex
# Terminal B / workspace B
pvisor run --safe --stage ../stage-b -- claude

pvisor status --review ../stage-a
pvisor status --review ../stage-b
```

这不是批量调度接口。选择一个结果后，按各自 workspace 合入并用现有 Git 流程集成；不要将多个任务的 upper 目录拷贝合并，也不要并发 apply 到同一目标树。

## 同一工作区与分叉的限制

从同一已停止 Job fork 可以保留共同的暂存起点，但检查点不冻结所有 lower。第一条分支 apply 后，另一条分支与宿主可能发生冲突；这种拒绝是保护机制，不能通过删原像绕过。要复现相同输入，应固定 workspace 基线、工具版本和镜像摘要。

## 运行数量由什么决定

先从两个任务验证，再记录进程、FUSE 挂载、VM 内存、磁盘、模型服务额度和代理端口占用。每个 Job 使用不冲突的端口；没有数据时不发布 8/32/128 的容量承诺。资源限制的实际效果读 Bundle；容量测试见[并发密度（规划中）](../benchmarks/density.md)。
