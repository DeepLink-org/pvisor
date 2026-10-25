# 稳定性与兼容承诺

把 pVisor 接入长期运行的流水线时，固定工具版本、记录格式版本和 Agent 版本。升级前用同一组代表性任务验证运行、评审、apply 和 replay，旧记录保留对应的读取工具。

当前读取器采用明确的格式版本检查。下面列出每种接口的读取规则，以及一次升级应当验证的操作。

## 今天可以依赖的读取规则

| 接口 | 当前规则 |
| --- | --- |
| CLI / Run TOML | 以当前版本 `--help` 与配置类型为准；未知字段拒绝；版本间变动应核对变更日志 |
| Run 记录 / 完成协议 | 新 Run schema 2 要求完成协议 1；owner 退出后缺少已提交回执的记录为 Unknown；schema 1 保留旧读取规则 |
| Run Bundle | 新 schema 5 要求匹配 Run/Attempt 的完成回执和 Bundle 摘要；schema 4 按旧规则读取；未知版本拒绝；没有自动迁移承诺 |
| Event / Journal | 正式 Event 与 Journal 版本 5；旧 JSONL 不兼容读取 |
| Operation | schema 1；未知版本拒绝 |
| replay | `sandbox-playback.result/v3`；适配器使用指南中的固定版本 |
| Rust 嵌入 API | 随 crate 修订；不要从 CLI 的可用性推导 ABI 或源码兼容性 |

自动化固定 pVisor 版本或提交，并在数据中记录 schema；升级前用自己的代表性任务验证这些接口。

## 升级前后

1. 保留原有 Bundle、Journal 和捕获产物；不要覆盖旧记录。
2. 用新版本运行一个独立的新 Job，验证策略准入、输出读取与 apply 冲突路径。
3. 旧记录需要读取时，保留能够读取它的旧工具；不要手工删除 schema 字段绕过校验。
4. 在变更日志中明确标注 CLI、配置、记录格式与平台要求的变化。以所升级版本的发布说明为准。
