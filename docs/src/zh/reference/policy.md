# 策略字段参考

策略回答两个实际问题：任务可以接触哪些文件、可以连接哪些目标。把项目共用的规则放到 `.pvisor/policy.toml`，把个人的规则放到用户配置目录；单次任务的额外限制放到 Run 配置里。

先写最小授权，再从 Run Bundle 检查实际安装的控制。策略的填写方法和执行边界分别见下面的示例与[网络策略指南](../guides/policies/network.md)。

## 当前策略文件写法

下面的内容用于工作区 `.pvisor/policy.toml` 或用户配置目录 `pvisor/policy.toml`。Run TOML 使用相同结构，但表路径改为 `[policies.session.network]` 和 `[policies.session.filesystem]` 等。

```toml
[network]
allow = [{ host = "api.example.com", ports = [443], transports = ["tcp_tunnel"] }]
deny = [{ host = "169.254.0.0/16" }]

[filesystem]
deny = ["secrets/**"]
ask = ["config/**"]
warn = ["**/.env*"]
```

| 字段 | 取值与含义 |
| --- | --- |
| `network.default_action` | 可选 `allow` / `deny`；省略时默认 deny；仅拒绝或仅限速的层需显式 allow |
| `network.allow` / `deny` | 规则数组；deny 优先，各层都必须允许 |
| 规则 `host` | hostname、通配后缀、IP 或 CIDR；不能用 URL 代替 |
| 规则 `ports` | 端口数组，禁止 0；空数组不限制端口 |
| 规则 `transports` | `http`、`https`、`tcp_tunnel`；空数组不限制协议 |
| 规则 `allow_private_ips` | 默认 false；hostname 解析到私网/loopback 默认拒绝，显式 IP/CIDR 的语义另见[网络指南](../guides/policies/network.md) |
| `network.limits` | `host`、`port` 可省略；`bytes_per_second` 为每秒字节数；匹配限制叠加 |
| `filesystem.deny/ask/warn/allow` | 相对工作区的 glob 数组；取最严格结果；allow 不覆盖外层 deny |

策略文件不是任意可信配置：目录和文件须归当前用户所有、不可由组或其他用户写入、无符号链接，文件须为不超过 1 MiB 的普通文件。不安全的策略会阻止启动。

## 从配置到证据

1. 用户、工作区、Session 与基础策略共同约束权限。
2. 准入计划记录执行器打算安装什么，最高只到 `Planned`。
3. 执行器收尾返回实际控制，`executor_observations` 决定强制力摘要。
4. 审查同时查看规则、拒绝记录和观察缺口；“没有拒绝”不等于“没有越界”。

`--safe` 的命令名预设和默认敏感文件清单分别见 [Agent 接入](../guides/agents/index.md)和[文件策略](../guides/policies/files.md)。完整 CLI 语法见 [CLI 参考](cli.md)。
