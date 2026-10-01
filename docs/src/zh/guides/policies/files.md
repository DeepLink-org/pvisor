# 文件策略

文件策略决定 Agent 在工作区视图里能看到和改动什么。它由 OverlayFS 执行，因此只作用于**经过工作区视图**的访问；视图之外的宿主路径由执行器边界约束，见[执行器边界](../../security/executor-boundaries.md)。

## 默认保护

`--safe` 默认加入下面的规则：

| 级别 | 路径 |
| --- | --- |
| deny | 任意层级的 `.ssh`、`.gnupg` 目录；`id_rsa`、`id_dsa`、`id_ecdsa`、`id_ecdsa_sk`、`id_ed25519`、`id_ed25519_sk` 文件 |
| warn | `.env`、`.env.*`、`*.pem`、`*.key`、`*.pub`、`*.p12`、`*.pfx`、`.aws/credentials`、`.netrc`、`.npmrc` |

预设只认常见文件名，不保证识别所有私钥；项目里自定义命名的秘密需要自己追加规则。

## 追加规则

```bash
pvisor run --safe \
  --access 'config/secrets/**:deny' \
  --access '**/.env*:warn' \
  -- agent-command
```

| 级别 | 效果 |
| --- | --- |
| `deny` | 路径在目录枚举中隐藏，访问、创建和修改都被拒绝 |
| `ask` | 访问时弹窗询问（自动启用审计 TUI 和 `--safe` 暂存视图） |
| `warn` | 放行，并在监督进程 stderr 打印路径（不打印内容）；**不是只读** |

多条规则命中时取最严格的：deny > ask > warn。`--access` 追加到配置和预设之后，不会替换默认保护；要清空默认保护必须显式使用 `--clear-access`。

## 匹配规则

- 规则相对于挂载根目录匹配：`*` 不跨目录，`**` 可跨目录，匹配目录时覆盖全部后代；
- 匹配**不区分大小写**，防止大小写不敏感文件系统上的别名绕过；
- 绝对路径、空规则以及含 `.`、`..` 的路径分量无效；
- 开启 deny 时，多硬链接普通文件和新建硬链接被保守拒绝；目录改名、交换或删除会检查受影响子树，含受保护文件时拒绝操作。

## 只读共享与直接写入

需要让 Agent 读取工作区之外的目录时，用显式共享，而不是 warn 规则：

```bash
pvisor run --safe --mount /opt/tool:read -- agent-command
```

`--mount SOURCE:read` 授予原绝对路径的只读访问，要求 host 执行器和 `--safe` 或 `--ask`；`SOURCE:write` 直接写宿主路径，不经过暂存和 apply。显式共享不经过工作区的 ask 规则，含秘密的目录不要共享。

## 审批弹窗

`ask` 命中时，弹窗可以选择授权范围：仅此文件、同级目录或相同后缀；默认选中拒绝。选择写入当前 Job 目录的 `audit-policy.json`，之后命中相同范围时自动应用，每次决策记录在 `audit.jsonl`。已保存的决定只用于 ask，不能覆盖静态 deny 或外层沙箱。

## 分层策略

除了命令行，策略还可以写在工作区 `.pvisor/policy.toml` 和用户 `~/.config/pvisor/policy.toml` 中，跨层取最严格的决定。见[策略模型](../../concepts/policy-model.md)。

完整参数见 [文件访问规则](../../reference/cli.md)，TOML 写法见 [一套配置模型](../../reference/cli.md)。
