# 安全与支持范围 {#_1}

<span id="security-overview"></span>

pVisor 的安全目标是让每次 Agent 执行**有界、可逆、可查**：限定出错的影响范围，让文件改动在合入前可以丢弃，并如实记录实际生效的控制。

## 一句话总结

| 保护 | 不保护 |
| --- | --- |
| 暂存工作区：apply 前项目文件不变，冲突时拒绝覆盖你的修改 | 已经发生的外部副作用：远程 API 调用、数据库写入、已发出的消息 |
| 敏感路径：`--safe` 预设拒绝视图内的 `.ssh`、`.gnupg` 与私钥文件 | 视图之外、显式共享或改名后的秘密文件；源码与 Git 历史中的密钥 |
| 强制网络边界：VM `auto`、host `--overlaynet-deny-all`、容器离线模式 | 协作式代理下忽略代理或直接建 socket 的客户端 |
| 如实的证据：区分请求、计划、实际安装的控制与观察结果 | 内核漏洞、侧信道、已显式授予凭据的滥用 |

每次 Run 的实际边界以它自己的能力证据为准，不由配置、预设名称或执行器名称推定。

pVisor 不提供密码学远程证明，也不是敌对多租户隔离。

## 平台、接口与能力状态 {#support-scope}

选择执行环境时，先核对[平台与执行器支持矩阵](../reference/platforms.md)中的准备条件，再检查[执行器边界](executor-boundaries.md)是否覆盖需要的文件、网络和进程控制。原生 Job、独立容器与 daemon 的接口范围不同；原生 Job、Runtime、daemon 和 shim 的能力见[接入方式矩阵](../guides/infrastructure/integration.md)。独立容器命令见 [CLI 参考](../reference/cli.md)。

请求的策略、计划安装的控制与实际生效的控制需要分别核对，含义见[能力、证据与保证边界](../concepts/capabilities-and-evidence.md)。自动等待管理等实验能力按各自的[实现阶段](../design/experimental/index.md)判断；实验结果不代表所有平台或接口都已支持。

## 对一次任务做安全评审

列出任务可接触的文件、凭据、网络和外部服务，按[威胁模型](threat-model.md)确定可信侧，并用[加固建议](hardening.md)配置所需控制。原生 Job 执行后通过 `pvisor status --review` 核对实际控制、警告与文件改动；结合[已知限制](known-limitations.md)决定是否接受结果。缺失的观察保留为未知，协作式控制不能视为强制隔离。

## 发现边界问题时

如果任务越过了自身证据标为 `Enforced` 的边界，或记录误报了控制，按[漏洞披露政策](disclosure.md)私密提交版本、执行器、命令和最小复现，不在公开 issue 披露漏洞细节。

当前没有公开登记的独立安全审计报告；内部回归和 Run Bundle 供核对实现与运行结果，不能代替独立审计。审计范围与报告要求见[第三方审计](audits.md)。
