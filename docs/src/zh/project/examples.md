# 复现 Run 生命周期

[`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
按 pVisor CLI 组织。每个 `run.sh` 管理自己的 `.work/`，并报告持久输出。
它们按文档顺序覆盖：先执行，再治理 Effect。

```bash
just examples
just examples-pvisor
```

## pVisor

| 示例 | 说明 |
|---|---|
| `01-filesystem-isolation` | 事务性工作区隔离 |
| `02-changeset-management` | 审查、应用和丢弃 |
| `03-network-isolation` | 显式代理策略及其边界 |
| `04-gateway-llm-control` | 内嵌 Gateway 路由与捕获 |

需要 macOS 或 Linux、Cargo、Python 3，以及 `jq` 等常见 POSIX 工具。
文件系统示例还需要 macFUSE 或 FUSE3。`just examples-pvisor-filesystem` 跑需要 FUSE 的 01/02；
`just examples-pvisor-portable` 跑不需要 FUSE 的 03/04。

从 `pvisor/01-filesystem-isolation` 开始，再进入 changeset 管理。

任务说明见 [pVisor 指南](../pvisor/guides/index.md)。示例验证产品路径；精确命令语法以
[CLI 参考](../pvisor/reference/cli.md)为准。
