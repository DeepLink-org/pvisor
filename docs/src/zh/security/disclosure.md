# 漏洞披露政策

## 如何报告

**不要在公开 issue 中披露漏洞。** 请通过 GitHub 私密漏洞报告提交：
[Report a vulnerability](https://github.com/DeepLink-org/pvisor/security/advisories/new)（仓库 Security → Advisories → Report a vulnerability）。

请附上：

- pVisor 版本或提交号、平台、执行器（host、container 或 VM）；
- 使用的命令或配置；可安全分享时附上 Run Bundle；
- 你预期的边界，以及工作负载实际做到了什么；
- 最小复现步骤（如果有）。

## 响应目标

| 步骤 | 目标时限 |
| --- | --- |
| 确认收到 | 3 个工作日内 |
| 初步评估与严重程度 | 10 个工作日内 |
| 修复或缓解计划 | 随初步评估一起告知 |
| 公开公告 | 修复发布后，与报告者协调时间 |

除非报告者要求匿名，公告中会致谢报告者。

## 范围

**在范围内**：

- Run 越过了它自己的能力证据中标为 `Enforced` 的边界；
- 暂存改动未经 `apply` 就到达工作区；
- `apply` 写到目标之外，或覆盖了冲突的外部改动；
- 证据错误地报告了实际安装的控制。

**不在范围内**：文档已经说明的限制（例如协作式 host 代理可被绕过）、内核漏洞、侧信道、已显式授予凭据的滥用。见[安全概览](index.md)与[已知限制](known-limitations.md)。

## 支持的版本

安全修复针对最新发布版本和 `main` 分支。

仓库根目录的 [`SECURITY.md`](https://github.com/DeepLink-org/pvisor/blob/main/SECURITY.md) 与这份政策内容一致；修改需同步两处。
