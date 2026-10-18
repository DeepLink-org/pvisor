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

## 讨论约定

讨论实现、报告问题和评审贡献时，把注意力放在可观察的行为与证据上：指出问题时附上版本、平台与复现方式；评审时解释影响与可接受的修改，不评价贡献者的能力或身份；出现分歧时先复述双方需要解决的问题，再比较方案和验收结果。侮辱、骚扰、歧视、公开他人私人信息以及持续针对个人的攻击不被接受。项目尚未正式公布 Contributor Covenant 的采用记录与行为事件私密联络渠道，设立后将在此公布；安全漏洞按[披露政策](../security/disclosure.md)提交。

## 试用与案例

当前没有在此登记、获得公开授权的组织案例；组织名称、logo 与生产规模需要使用者确认后再发布。如果你试用了 pVisor，一份有用的案例记录包括：原来的工作流与要解决的问题、pVisor 版本、平台与执行器、Agent 和模型版本、关键命令或配置、任务后的评审方式，以及采用之后实际改变了什么。性能数字请带上任务数量、机器与测量方法；失败案例和后来调整的配置同样有价值。提交前确认组织名、引用和数据得到公开授权，私有代码与凭据替换为可公开复现的内容。
