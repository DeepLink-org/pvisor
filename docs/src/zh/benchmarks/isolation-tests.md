---
status: todo
search:
  exclude: true
---

# 隔离有效性测试

!!! warning "规划中"
    尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

有没有已知的逃逸路径？

## 需求

- 指标：逃逸用例通过率
- 对照组：各执行器之间
- 工作负载：semspec S-STAGE 用例，加公开的沙箱逃逸语料（符号链接替换、路径穿越、Unix socket、/proc 等）
- 环境：每个用例按执行器给出 PASS/FAIL/XFAIL

## 验收标准

- XFAIL 链接到已知限制
- 语料与脚本公开可复现

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：benchmarks/methodology、security/known-limitations
