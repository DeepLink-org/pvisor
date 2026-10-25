# PolicyVisor Examples

**按产品问题组织的可复现 CLI 示例。**

每个 `run.sh` 管理自己的 `.work/`、运行产品命令、直接打印生成的文件与报告。
运行后可继续检查 `.work/`。这里不拥有产品实现；pVisor 的行为以文档站和对应 crate 为准。

## pVisor

| 示例 | 指标 |
|---|---|
| [1.1 文件系统隔离](pvisor/01-filesystem-isolation) | lower 值、upper 文件数、Bundle changes |
| [1.2 changeset 管理](pvisor/02-changeset-management) | review/apply/drop 文件数 |
| [1.3 pVisor 网络边界](pvisor/03-network-isolation) | allowlist、deny-all，以及 direct socket 可绕过 cooperative proxy 的边界 |
| [1.4 Gateway 捕获与管控 LLM](pvisor/04-gateway-llm-control) | upstream POST、sink requests、LLM events |

## Run

```bash
mise run examples
```

这些入口统一增量编译并使用 release targets，之后复用 Cargo 缓存。需要
macOS/Linux、Cargo、Python 3、`jq`、`awk`、`curl` 和常见 POSIX 工具；
OverlayFS 示例还需要 macFUSE 或 FUSE3。

## Links

- [Reproducible examples](../docs/src/zh/community/examples.md)
- [pVisor get started](../docs/src/en/start/first-run.md)

## 工具执行接入示例

[07-vm-task](pvisor/07-vm-task) 以调用脚本和 CLI 适配函数执行 `qutebrowser-667` SWE 工具片段，
自动准备任务镜像、应用候选补丁，并收集两次原始工具调用的反馈与执行记录。需要安装 pVisor、Linux/KVM 及节点权限；
真实框架集成待补充。不需要源码编译。独立运行，不加入 `mise run examples` 场景集合。
