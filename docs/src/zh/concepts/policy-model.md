# 策略模型

策略分层：请求 → 准入 → 有效约束 → 实际安装的控制 → 观察。user、workspace、session 与执行器基础策略共同约束权限；后面的 allow 不能覆盖前面的显式拒绝。`--safe` 要求落实隔离，`--strict` 校验全部请求维度。

!!! note "TODO"
    补请求／准入／降级的完整定义，字段见 reference/policy。
    与 concepts/capabilities-and-evidence 去重。

