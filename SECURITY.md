# Security Policy

PolicyVisor (pVisor) is an execution layer whose value depends on its
boundaries. We treat reports about those boundaries as a priority.

## Reporting a vulnerability

**Do not open a public issue for a vulnerability.**

Report privately through GitHub:
[Report a vulnerability](https://github.com/DeepLink-org/pvisor/security/advisories/new)
(Security → Advisories → Report a vulnerability).

Please include:

- the pVisor version or commit, platform, and executor (host, container, or VM);
- the command or configuration used, and the Run Bundle if it is safe to share;
- what boundary you expected, and what the workload was able to do instead;
- a minimal reproduction, if you have one.

## What we commit to

| Step | Target |
| --- | --- |
| Acknowledge the report | within 3 business days |
| Initial assessment and severity | within 10 business days |
| Fix or mitigation plan | communicated with the assessment |
| Public advisory | after a fix is released, coordinated with the reporter |

We credit reporters in the advisory unless they ask otherwise.

## Scope

In scope: any case where a Run exceeds the boundary that its own capability
evidence reports as `Enforced`; staged changes reaching the workspace without
`apply`; `apply` writing outside the target or overwriting a conflicting
external change; evidence that misreports installed controls.

Out of scope: limits that the documentation already states, such as
cooperative host proxies being bypassable, kernel vulnerabilities, side
channels, and misuse of credentials that were explicitly granted. See
[security overview](https://deeplink-org.github.io/pvisor/zh/security/) and
[known limitations](https://deeplink-org.github.io/pvisor/zh/security/known-limitations/).

## Supported versions

Security fixes target the latest release and `main`.

---

## 中文摘要

请勿在公开 issue 中披露漏洞。通过 GitHub
[私密漏洞报告](https://github.com/DeepLink-org/pvisor/security/advisories/new)提交，
附上版本、平台、执行器、复现步骤，以及你预期的边界和实际越过的地方。我们的目标是
3 个工作日内确认收到，10 个工作日内给出初步评估。范围与流程见
[漏洞披露政策](https://deeplink-org.github.io/pvisor/zh/security/disclosure/)。
