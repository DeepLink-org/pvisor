---
status: todo
search:
  exclude: true
---

# 稳定性与兼容承诺

!!! warning "规划中"
    本页需要维护者决定版本策略。

## 要回答的问题

用户可以依赖哪些接口不变？CLI 参数、配置字段、Run Bundle schema、JSON 输出和嵌入式 API 分别遵循什么兼容规则？

## 需求

- 版本号策略（例如 1.0 之前的次版本是否允许不兼容变更）；
- 每类接口的稳定性等级与弃用流程（提前多少个版本告知、如何提示）；
- Run Bundle schema 升级时旧记录的处理方式。

## 验收标准

- 维护者确认版本策略；
- 变更日志按本页规则标注不兼容变更。

## 关联

- 跟踪 issue：TODO
- 负责人：维护者
- 相关页面：[Run Bundle 格式（规划中）](run-bundle.md)、[变更日志](../community/changelog.md)
