# 社区

pVisor 是 Apache-2.0 开源项目。

## 沟通渠道

| 用途 | 渠道 |
| --- | --- |
| 报告 bug、提需求 | [GitHub Issues](https://github.com/DeepLink-org/pvisor/issues) |
| 安全漏洞 | [私密漏洞报告](https://github.com/DeepLink-org/pvisor/security/advisories/new)，见[漏洞披露政策](../security/disclosure.md) |
| 代码贡献 | GitHub Pull Request，见[贡献指南](contributing.md) |

## 从一个可复现问题开始

报告问题时附上版本或提交、操作系统、执行器、最小命令以及预期与实际结果。分享 Run Bundle 或日志前移除凭据和私有内容。较大的接口或行为变更先用 issue 讨论方案、兼容影响和验收条件，再实现。

准备 PR 时只聚焦一件事：修复实现，补上相关回归，并同步中英文权威文档。先阅读修改路径上的 README，按[开发环境](development.md)准备所需依赖，再按[贡献指南](contributing.md)运行格式、lint 和相关测试。说明实际运行了哪些检查、哪些条件未验证，不把未运行的场景列为通过。

## 分开验证与批准

单元和集成测试检查实现；semspec 把产品承诺写成声明、违反示例和可执行检查。可以起草新用例、修复实现和维护 runner 测试，不得削弱已有 claims、checks 或 `xfail` 来获得 PASS。

人工语义审核独立于测试和 PR 合并。AI 不运行 `semspec approve` / `revoke`，不编辑真实 `REVIEWED.toml` 或 `.approved/` 快照；具体规则见[测试与 semspec](testing.md)。贡献内容按 Apache-2.0 授权。
