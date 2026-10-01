# 安全概览

pVisor 的目标是「有界、可逆、可查」的执行，而不是密码学证明或敌对多租户隔离。

- 保护什么、不保护什么：[威胁模型](threat-model.md)
- 各执行器的边界矩阵：[执行器边界](executor-boundaries.md)
- 加固建议：[加固](hardening.md)
- 已知限制与不变量缺口：[已知限制](known-limitations.md)
- 漏洞披露：[漏洞披露政策](disclosure.md)

!!! warning "范围"
    pVisor 不防御内核漏洞、侧信道，也不阻止被授予凭据的滥用。实际边界以每次 Run 的能力证据为准。

!!! note "TODO"
    补「保护／不保护」的一句话摘要与适用范围。

