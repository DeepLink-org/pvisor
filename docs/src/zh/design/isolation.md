# 隔离机制与限制

写时复制工作区和安全边界解决不同问题。OverlayFS 保留文件改动供审查；executor 与实际安装的宿主控制决定进程能否访问视图之外的路径、socket 或资源。

## 当前 executor

| Executor | 机制 | 必须明确的限制 |
| --- | --- | --- |
| Linux host | `--filesystem sandbox` 启用 rootless launcher、user/mount/PID namespace、投影根目录和协商后的 Landlock；`--overlaynet-deny-all` 独立启用私有网络 namespace | 依赖内核及宿主配置；选择性代理网络仍是协作式；默认 host 文件系统视图不受限制 |
| macOS host | `--filesystem sandbox` 启用 Seatbelt 文件系统控制；`--overlaynet-deny-all` 独立启用 deny-all socket 策略；`--stage` 独立选择暂存工作区 | 未请求对应策略时，读取和选择性网络访问仍为 ambient/协作式；暂存挂载需要 macFUSE |
| 原生 OCI 容器 | Linux OCI runtime、镜像用户空间和配置的挂载及网络 | 不声明所有 capability 维度均已完整强制执行 |
| libkrun VM | 独立 Linux 客户机内核、virtio-fs 工作区和 smoltcp 网络路径 | 需要 KVM 或 HVF；宿主连接器和共享文件仍是边界的一部分 |

应检查具体 Run Bundle；配置表达请求，实际发生了什么以安装的控制和证据为准，口径见[能力与证据](../concepts/capabilities-and-evidence.md)。safe-best-effort 可能报告控制降级。`--strict` 在任一必需维度缺少强制执行证据时拒绝运行；当前 executor 不声明完整的 Subprocess 强制执行，因此它不是现成的更强沙箱预设。

## 工作区与生命周期

文件系统访问、网络隔离和改动暂存是独立设置。Host 默认保留宿主文件系统视图。需要限制路径访问时使用 `--filesystem sandbox`，需要审查改动时显式启用暂存：

```bash
pvisor run --stage ../stage-001 -- codex
pvisor run --filesystem sandbox --overlaynet-deny-all -- codex
```

默认写入和清理规则见 [暂存与存储](../reference/cli.md#暂存与存储)。暂存的覆盖范围见[能力与证据](../concepts/capabilities-and-evidence.md)。ZCode 的 Linux host 适配会向应用状态目录授予持久写权限；详见 [CLI 参考](../reference/cli.md)。

宿主进程 executor 创建进程组，在完成或取消后向整组发送终止信号，并在宽限期后升级终止。后代持有输出管道时，读取等待也有时限。主动脱离进程组的进程需要更强的平台约束；进程组清理本身不是完整的后代隔离边界。

`fork` 命令要求 Run 已停止；快照范围见[执行模型](../concepts/run-model.md)。嵌入式 AgentCtl 参与者可以配合静默协议，但这不会使任意子进程变成可检查点恢复的进程。

## 网络边界

各执行路径的控制范围、能否绕过以及 VM 数据面的协议覆盖见[网络边界](../guides/network.md#网络边界)，设计机制见 [OverlayNet](overlaynet.md)。

环境配置见[执行环境](../guides/execution.md)，证据解释见[能力与证据](../concepts/capabilities-and-evidence.md)。
