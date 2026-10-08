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


## 给单次任务增加约束 {#scoped-example}

下面的 Run 在 Session 层允许普通流量，但拒绝 metadata 地址，把拦截流量限制到 1 MiB/s，并阻止读取项目私密文件。请求仍须满足基础 OverlayNet 策略以及工作区、用户策略。

```toml
[run]
command = ["/bin/sh", "-c", "printf 'ready\n'"]

[policies.session.network]
default_action = "allow"
deny = [{ host = "169.254.0.0/16" }]
limits = [{ bytes_per_second = 1048576 }]

[policies.session.filesystem]
deny = ["secrets/**"]
ask = ["config/**"]
warn = ["**/.env*"]
```

改成 allowlist 时，省略 `default_action` 并填写 `allow`。例如 Session 层允许 `api.example.com:443`，但工作区层拒绝该目标，最终仍会拒绝。设置 `default_action = "allow"` 时，匹配的 `deny` 仍然优先。仅配置限速而未设置此默认动作的层，会拒绝所有未匹配的目标；限速不等于授权。

## 加载与合并策略层 {#inheritance}

CLI 读取工作区的 `.pvisor/policy.toml` 和用户配置目录下的 `pvisor/policy.toml`。文件不存在时不增加策略。显式配置 `[policies.workspace.network]` 会替代自动加载的整个工作区 network 维度，不会逐条合并规则。filesystem 维度独立加载。`policies.user` 的处理方式相同。

空的作用域 `[network]` 表表示该层存在，会拒绝未匹配请求；完全省略作用域 network 表则不增加这一层的网络约束。这两种写法含义不同。

基础网络策略与每个存在的作用域层都允许，请求才能通过。所有匹配的带宽限制都会生效。文件策略跨作用域取最严格的匹配结果：deny、ask、warn、allow。`filesystem.allow` 可以约束策略的普通匹配行为，但不能撤销外层拒绝。各执行器如何处理 `ask`，见[文件策略](../guides/policies/files.md)。

## 文件规则语法 {#file-globs}

用户规则数组为 `deny`、`ask`、`warn`、`allow`，均默认空。规则是不区分大小写、相对工作区的 glob，路径分隔符按字面匹配：`*` 不跨越 `/`，跨目录使用 `**`。绝对路径、NUL、空路径分量、`.` 分量、`..` 分量均无效。使用 `secrets/**`，不要写 `/secrets/**` 或 `../secrets/**`。

序列化的 filesystem 策略记录还可能包含 `context` 和 `layers`，用于保留运行时绑定与组合的作用域信息；普通策略文件无需用它们增加授权。完整的网络条目字段与类型见[配置字段](config.md#all-fields)。

## Safe 与 audit 预设 {#presets}

`--safe`、`--audit` 在加载 TOML 之后、应用显式 CLI 参数之前构建预设。两者都请求暂存、清空配置中的 `pass_env`，并拒绝以下敏感路径：

```text
**/.ssh
**/.gnupg
**/id_rsa
**/id_dsa
**/id_ecdsa
**/id_ecdsa_sk
**/id_ed25519
**/id_ed25519_sk
```

Safe 对以下匹配发出 warn，audit 改为 ask：

```text
**/.env
**/.env.*
**/*.pem
**/*.key
**/*.pub
**/*.p12
**/*.pfx
**/.aws/credentials
**/.netrc
**/.npmrc
```

请求命令的 basename 决定模型目标预设：

| 命令 | 主机名，端口 443 |
| --- | --- |
| `codex`、`bash`、`sh`、`zsh`、`fish` | `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` |
| `claude` | `api.anthropic.com` |
| `gemini` | `generativelanguage.googleapis.com` |
| `zcode` | `api.z.ai`、`open.bigmodel.cn` |
| 其他命令名 | 拒绝普通出口 |

启用 capture 且提供显式 Gateway routes 时，也会拒绝 Agent 普通出口，让模型请求经过 Gateway。显式 CLI 目标选项覆盖预设；作用域策略仍与其取交集。预设为 VM 选择 `auto` 网络，其他执行器选择 `proxy`。按[网络指南](../guides/policies/network.md)选择能落实所需边界的执行器。
