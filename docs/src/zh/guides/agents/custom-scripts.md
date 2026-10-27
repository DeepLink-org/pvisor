# 任意脚本与自动化命令

脚本和自动化命令也可以用 pVisor 运行，使用同样的执行边界、文件暂存、审查和记录。

```bash
pvisor spawn --safe --overlaynet-deny-all -- ./agent.sh
pvisor status --review last
pvisor apply last --path src
```

## 行为

- 命令在暂存视图中运行，能看到项目当前的文件，写入进入暂存区。
- `pvisor spawn` 原样返回命令的退出码，脚本和 CI 可据此判断成败。
- 命令非零退出时，文件改动仍保留在暂存区，可以审查后决定 apply 或 drop。
- `--` 之后的内容原样作为命令和参数传递。

## 网络

脚本不匹配任何 Agent 预设时，`--safe` 默认拒绝所有出站。按需选择：

| 需要 | 参数 |
| --- | --- |
| 完全离线 | `--overlaynet-deny-all` |
| 只访问少数目标 | `--overlaynet-allow pypi.org:443`（可重复） |
| 拒绝特定目标 | `--overlaynet-deny 169.254.0.0/16` |

`bash`、`sh`、`zsh`、`fish` 作为命令名时沿用 Codex 的预设目标，见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设)。

## 环境变量

`--safe` 只投影必要变量。脚本需要的变量用 `--pass-env NAME` 显式传入，见[凭据与环境变量](../policies/credentials.md)。

## 完整示例

[第一次运行](../../start/first-run.md)用一个"假 Agent"脚本演示了修改、删除、敏感路径被拒和网络被拦截的完整过程；[`examples/pvisor/`](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor) 下有文件隔离、改动集管理和网络隔离的可运行示例。
