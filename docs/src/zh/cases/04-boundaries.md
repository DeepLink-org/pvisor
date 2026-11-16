# 4. 为不可信任务声明边界

从暂存文件进一步走向执行隔离。`--safe` 要求可用的隔离实现，并保留工作区改动；明确网络策略和路径规则，最后检查 Run Bundle 中的实际证据。测试使用独立临时 HOME 和 XDG 目录，避免接触个人配置；生产环境的文件系统和 capability 仍要检查。

### S-USE-012：safe 保护工作区外的文件

不可信脚本可能尝试写入绝对路径。用 safe 并验证外部文件未变化，而不是仅看到 stage 就认为隔离。

**语义**：工作区改动暂存，原工作区外文件不可见且宿主保持 sentinel，私有 /tmp 写入不穿透宿主，Evidence 声明 filesystem_non_bypassable。

**违反示例**：外部文件被覆盖，或未执行隔离却报告边界有效。

运行与断言：[Scrut S-USE-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-012.md)。

### S-USE-013：拒绝敏感路径访问

工作区里也可能有凭据。显式 `--access` 规则保护匹配路径，并检查任务确实遇到拒绝。

**语义**：Agent 不能读取 secret.txt，却仍可生成 public.txt，宿主 secret 保持。

**违反示例**：拒绝规则只记录日志而仍返回秘密。

运行与断言：[Scrut S-USE-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-013.md)。

### S-USE-014：禁止网络连接，验证真实拒绝

离线分析和不需 API 的任务可以 deny-all。用本机监听器验证连接确实失败，不依赖公网服务。

**语义**：宿主能连接测试监听器，deny-all Session 的相同连接失败且网络 Evidence 声明 non_bypassable。

**违反示例**：网络隔离静默退化，Agent 仍连到宿主监听器。

运行与断言：[Scrut S-USE-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/isolation/S-USE-014.md)。

[返回学习路线](index.md) · [5. 回放轨迹与选择恢复工具](05-tools-and-restoration.md)
