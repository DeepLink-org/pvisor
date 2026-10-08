# 凭据与环境变量

默认情况下，Agent 看不到你的凭据：pVisor 只投影少量必要的环境变量，凭据需要你显式授予。

## 环境变量投影

| 情况 | 行为 |
| --- | --- |
| 大多数命令 | 关闭宿主环境继承，只投影 `HOME`、`PATH`、`LANG`、`SHELL`、`TERM`、`USER`、`LOGNAME` 等必要变量 |
| 直接运行 Codex（不加 `--safe`） | 保留宿主环境继承，以维持账号与路由配置 |
| Linux host 直接运行 `zcode` | 应用兼容策略：继承宿主环境 |
| 显式 `--pass-env NAME` | 把指定变量交给 Agent |
| `--clear-pass-env` | 清空配置文件中的 `run.pass_env`；之后的 `--pass-env` 仍然生效 |

`--safe` 会清空配置文件中的 `run.pass_env`，所以使用 `--safe` 时，凭据只能通过命令行上显式的 `--pass-env` 交付。实际投影的变量列在 `status --review` 的 Environment and resources 一节。

pVisor 还会注入运行时变量，例如 `PVISOR_RUN_ID`、`PVISOR_AGENTCTL_*`；启用网络代理时注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及其小写形式。

## HOME 与本地凭据文件

- **`--safe`**：HOME（以及显式设置的 `CODEX_HOME`）使用独立的私有 stage。macOS 上 Agent 使用临时 HOME，不能直接读取原主目录；Linux 上启动器通过私有 stage 投影宿主 HOME。HOME 的改动在 Run 结束后丢弃。
- **视图内的 `.ssh`、`.gnupg` 与私钥文件**：`--safe` 预设拒绝访问，见[文件策略](files.md)。
- **Linux 注意**：overlay deny 规则不会隐藏工作区视图之外原路径上的秘密文件；需要更强保证时使用 `--filesystem sandbox` 或 VM。

## 两种交付 API Key 的方式

| 方式 | 做法 | Agent 能否看到 Key |
| --- | --- | --- |
| 环境变量 | `--pass-env OPENAI_API_KEY` | 能，并可以按任意方式使用 |
| Gateway 持有 | 配置 Gateway 路由，`api_key_env` 指向可信侧的变量 | 不能；Agent 只连接 Gateway |

```bash
pvisor run --safe \
  --gateway-mode capture \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  -- my-agent
```

优先使用 Gateway：Key 留在可信侧，模型请求还能被记录。配置细节见 [Gateway 捕获](../capture.md)。

Gateway 持有的 key 只委托给支持的模型 POST 端点；未知或管理动作在转发前
被拒绝。模型列表在本地返回。显式模型委托仍可在 `no-network` 下使用；后者
控制普通代理出口。支持路径与自定义上游前缀要求见
[Gateway 动作范围](../../design/gateway.md#delegated-credential-actions)。

## 不在保护范围内

通过 `--pass-env` 交给 Agent 的凭据，pVisor 不追踪它如何被使用，也不会让它自动失效。见[威胁模型](../../security/threat-model.md)。
