---
status: todo
search:
  exclude: true
---

# apply/drop 成本与崩溃一致性

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

apply 大改动集要多久？崩溃后工作区是否一致？

## 需求

- 指标：apply/drop 耗时与改动规模的关系；冲突检测成本；崩溃一致性
- 对照组：cp -a；git apply
- 工作负载：10、1k、100k 个文件的改动集
- 环境：在 Prepared、TargetApplied、Committed 各状态 kill -9

## 验收标准

- 崩溃注入后工作区一致率必须 100%，并说明恢复路径
- 冲突检测有回归

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
