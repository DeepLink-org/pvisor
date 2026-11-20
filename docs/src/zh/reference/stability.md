# 验证集成

在独立 Session 中运行代表性任务，核对流水线依赖的执行器、实际安装的策略控制、
输出和失败行为。

1. 用 `pvisor help COMMAND` 核对所需参数，在[配置参考](config.md)中查询 TOML 字段。
2. 读取 [JSON 结果](json-output.md)和 [Run Bundle](run-bundle.md)，确认运行结果、
   所选执行器和实际安装的控制。用[记录矩阵](../design/records-and-versions.md)
   查找回执及其所有权绑定。
3. 验证选择性 apply，并检查宿主文件发生冲突后是否阻止再次 apply。
   结果要求恢复时，保留暂存区和 ledger。[审查并应用](../guides/review-apply.md)
4. 使用 replay 的流水线应通过已配置的适配器运行一份已记录的任务，并核对结果。
   [回放指南](../guides/replay.md)
