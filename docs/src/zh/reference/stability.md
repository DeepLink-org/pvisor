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

## 今天可以依赖的读取规则

| 接口 | 当前规则 |
| --- | --- |
| CLI / Run TOML | 以当前版本 `--help` 与配置类型为准；未知字段拒绝；版本间变动应核对变更日志 |
| Run Bundle | schema 4 严格读取；未知版本、旧观察契约拒绝；没有自动迁移承诺 |
| Event / Journal | 正式 Event 与 Journal 版本 5；旧 JSONL 不兼容读取 |
| Operation | schema 1；未知版本拒绝 |
| replay | `sandbox-playback.result/v3`；适配器使用指南中的固定版本 |
| Rust 嵌入 API | 随 crate 修订；不要从 CLI 的可用性推导 ABI 或源码兼容性 |

上表描述实际读取行为，不新增维护者尚未确认的长期兼容承诺。自动化应固定 pVisor 版本或提交、记录 schema、在升级前用自己的代表性任务验证。

## 升级前后

1. 保留原有 Bundle、Journal 和捕获产物；不要覆盖旧记录。
2. 用新版本运行一个独立的新 Job，验证策略准入、输出读取与 apply 冲突路径。
3. 旧记录需要读取时，保留能够读取它的旧工具；不要手工删除 schema 字段绕过校验。
4. 在变更日志中明确标注 CLI、配置、记录格式与平台要求的变化。正式弃用窗口仍由维护者决定。
