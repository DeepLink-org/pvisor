---
status: todo
search:
  exclude: true
---

# 平台与执行器支持矩阵

!!! warning "规划中"
    本页需要研发给出带证据的成熟度等级。各执行器的边界见[执行器边界](../security/executor-boundaries.md)。

## 要回答的问题

在哪个平台上、用哪个执行器、哪个能力维度，当前处于什么成熟度？

## 需求

- 矩阵：平台（Linux x86_64、Linux arm64、macOS Apple Silicon）× 执行器（host、container、VM）× 能力维度；
- 每格的成熟度等级：稳定、Beta、实验、不支持；
- 每个等级必须附证据链接：CI 任务、语义规格、基准或 issue，不凭空标注。

## 验收标准

- 每格都有等级和证据；
- README 的成熟度标识指向本页。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[执行器边界](../security/executor-boundaries.md)、[已知限制](../security/known-limitations.md)
