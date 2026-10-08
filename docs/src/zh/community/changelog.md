# 变更日志

完整变更记录见仓库根目录的 [`CHANGELOG.md`](https://github.com/DeepLink-org/pvisor/blob/main/CHANGELOG.md)，每个版本的发布说明见 [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases)。

## 升级时检查什么

先记录当前 pVisor 版本或提交，以及所用 Agent、模型和镜像版本，再核对当前版本到目标版本之间的变化：CLI 参数、TOML 字段、记录 schema、执行器前置条件和平台产物。

用新版本运行独立 Job，验证策略准入、评审输出读取、选择性 apply 与冲突拒绝；使用 replay 时也验证所用适配器。保留旧 Bundle、Journal 和捕获产物，以及能读取它们的工具。不要删除 schema 字段绕过版本检查；当前读取规则见[稳定性与兼容承诺](../reference/stability.md)。

## 提交变更时说明什么

在 PR 中说明用户可见行为、适用平台与执行器、验证方式，以及是否需要调整命令、配置或记录读取器。不兼容变更应明确标注，并给出受影响的接口和升级所需操作；不据此推定长期兼容性或弃用期限。
