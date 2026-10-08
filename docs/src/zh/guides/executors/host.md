# host 执行器

host 是默认执行器：命令直接使用宿主内核和已安装的工具链。它最适合"在本机放手运行 Agent、事后审查改动"的场景。

```bash
pvisor run --executor host --stage ../stage-host -- /bin/sh
```

## 三个相互独立的开关

| 需要 | 参数 | 效果 |
| --- | --- | --- |
| 可审查的写时复制工作区 | `--stage PATH`，或 `--safe`/`--ask` | 工作区改动进入 stage，apply 前不影响项目 |
| 限制路径访问 | `--filesystem sandbox` | 启用平台的文件系统访问控制（见下） |
| 拒绝所有普通网络出口 | `--overlaynet-deny-all` | 安装平台的网络边界（见下） |

这三项互不推导：开启暂存不会限制读取，网络参数也不会启用文件系统限制。`--safe` 把它们组合成一个预设，见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设)。

## 平台机制

| 平台 | 文件系统控制 | 网络边界 | 暂存挂载 |
| --- | --- | --- | --- |
| Linux | rootless launcher：user/mount/PID namespace、投影根目录、`chroot` 与内核协商的 Landlock | `--overlaynet-deny-all` 创建私有 network namespace | FUSE |
| macOS | Seatbelt 读写范围 | `--overlaynet-deny-all` 用 Seatbelt 阻断非 loopback IP 和宿主 ambient Unix socket，保留 loopback 代理、AgentCtl 和 Run 私有 IPC | macFUSE |

`--safe` 在两个平台上的具体组合：

- **macOS**：Seatbelt 强制暂存写入，只允许连接 pVisor 分配的 loopback 代理端口；Agent 使用临时 HOME，不能直接读取原主目录。读取范围以 Run 证据为准：当前证据把读取报告为 ambient，见[已知限制](../../security/known-limitations.md)。
- **Linux**：要求 rootless namespace、synthetic root、chroot、Landlock 和 HOME 写时复制视图。选择性出口通过 supervisor loopback 代理协作式转发，**直接 socket 仍可能绕过**；需要不可绕过的网络边界时，用 VM 或 deny-all。

## 已知缺口

- 默认（未加 `--filesystem sandbox`）host 文件系统视图不受限制，读取是 ambient 的；
- 选择性网络策略（allow/deny 列表）在 Linux host 上是协作式的，忽略代理或直接建 socket 的客户端可以绕过；
- Linux 上 overlay deny 规则不会隐藏工作区视图之外原路径上的秘密文件；
- 进程在完成或超时后按进程组清理，并限制等待输出管道的时间；主动脱离进程组的后代不受进程组清理约束。

## 在 Run Bundle 中核对

运行后用 `pvisor status --review` 查看 Safety boundary 一节，或用 `--json` 读取 `safety.network_non_bypassable` 等字段。证据的含义见[能力、证据与保证边界](../../concepts/capabilities-and-evidence.md)。
