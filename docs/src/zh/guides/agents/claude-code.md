# Claude Code

```bash
pvisor run --safe --pass-env ANTHROPIC_API_KEY -- claude
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## `--safe` 为 Claude Code 做了什么

- **网络**：按命令名 `claude` 匹配预设，只放行 `api.anthropic.com:443`，其他目标默认拒绝；
- **工作区**：改动进入暂存区，审查后再 apply；
- **HOME**：使用独立的私有 stage，Claude Code 在 HOME 里写的状态在 Run 结束后丢弃，不进入 Run Bundle；
- **敏感路径**：视图内的 `.ssh`、`.gnupg` 与私钥文件被拒绝。

## 凭据

`--safe` 不会把宿主环境中的凭据传给 Agent。二选一：

- 用 `--pass-env ANTHROPIC_API_KEY` 显式交付；
- 配置 Gateway 路由，由可信侧持有上游 Key，Agent 看不到 Key，见[凭据与环境变量](../policies/credentials.md)。

macOS 上 `--safe` 使用临时 HOME，宿主上已登录的会话状态不可见；Linux 上 HOME 通过私有 stage 投影，Agent 写入的状态不会回到宿主。

## 需要访问其他目标时

Agent 需要安装依赖或访问文档站点时，显式追加目标。注意 `--overlaynet-allow` 会覆盖预设列表，所以要把模型 API 一起写上：

```bash
pvisor run --safe \
  --overlaynet-allow api.anthropic.com:443 \
  --overlaynet-allow pypi.org:443 \
  --pass-env ANTHROPIC_API_KEY -- claude
```

## 网络边界的强度

在 macOS host 上，`--safe` 阻断直接外部连接；在 Linux host 上，选择性规则通过协作式代理执行，直接 socket 仍可能绕过。需要不可绕过的边界时使用 VM，见[网络边界](../policies/network.md#网络边界)。
