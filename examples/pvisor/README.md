# PolicyVisor（pVisor）：让自主 Agent 的执行可以规模化

**问题：pVisor 的事务工作区、changeset、显式网络代理和 Gateway 能否用 `run.sh` 定量复现？可复现结论：四个场景分别断言 lower/upper、review/apply/drop、cooperative proxy 边界，以及 Gateway 捕获计数。**

这组示例依次展示 pVisor 的事务工作区、changeset、显式网络代理和 Gateway。每个
`run.sh` 只保留场景准备、pVisor 命令和产物展示；`just examples` 中的场景配方
调用同一个 `run.sh`，再对 lower/upper、Run Bundle、日志或事件日志 执行回归断言。
这里不拥有隔离后端或 Gateway 实现。

| 示例 | 可复现结论 |
|---|---|
| [01-filesystem-isolation](01-filesystem-isolation) | Agent 写入 upper，lower 在 apply 前保持不变 |
| [02-changeset-management](02-changeset-management) | changeset 可 review，并可分别 apply 或 drop |
| [03-network-isolation](03-network-isolation) | 三条平铺命令展示 allowlist、deny-all 与 cooperative proxy 的 direct-socket 边界 |
| [04-gateway-llm-control](04-gateway-llm-control) | Gateway 路由并捕获两次 OpenAI-compatible 调用 |
| [05-zcode-cli](05-zcode-cli)（可选） | 真实 ZCode CLI 的文件工具、SSE 捕获、选择性 apply/drop 与超时清理 |
| [06-tui-interception](06-tui-interception)（交互式） | 在 TUI 中查看 `touch` 的文件操作与 `curl` 的目标拒绝证据 |

文件系统示例需要 macOS 的 macFUSE 或 Linux 的 FUSE3。这里的“轻量级隔离”特指
事务工作区和示例中 cooperative public proxy 所覆盖的数据面；直接 socket 可绕过
该代理。Host executor 的完整文件系统与 deny-all 网络边界因平台而异，以 Run Bundle
和 pVisor 隔离文档为准。

## Run

```bash
just examples
just examples 01-filesystem-isolation 02-changeset-management  # 01/02，需要 FUSE
just examples 03-network-isolation 04-gateway-llm-control    # 03/04，普通 CI runner
just examples 03-network-isolation
```

`just examples` 统一构建、运行并验证场景，也可直接运行各场景的 `run.sh` 演示；
通过 `WORK_ROOT` 把临时产物放到指定目录。

## Links

- [Reproducible examples](../../docs/src/zh/community/examples.md)
- [pVisor get started](../../docs/src/zh/start/first-run.md)
- [Isolation architecture](../../docs/src/zh/design/isolation.md)

`05-zcode-cli` 需要额外安装 ZCode CLI 和 Node.js，默认批量运行仍只包含 01–04。配置方式见该示例的 README。
`06-tui-interception` 需要交互式终端，需单独运行；`just examples 06-tui-interception` 可无 TUI 回归同一拦截路径。
