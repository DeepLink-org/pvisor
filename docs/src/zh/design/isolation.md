# 隔离机制与限制

写时复制工作区和安全边界解决不同问题。OverlayFS 保留文件改动供审查；executor 与实际安装的宿主控制决定进程能否访问视图之外的路径、socket 或资源。

## 当前 executor

| Executor | 机制 | 必须明确的限制 |
| --- | --- | --- |
| Linux host | `--filesystem sandbox` 启用 rootless launcher、user/mount/PID namespace、投影根目录和协商后的 Landlock；`--overlaynet-deny-all` 独立启用私有网络 namespace | 依赖内核及宿主配置；选择性代理网络仍是协作式；默认 host 文件系统视图不受限制 |
| macOS host | `--filesystem sandbox` 启用 Seatbelt 文件系统控制；`--overlaynet-deny-all` 独立启用 deny-all socket 策略；`--stage` 独立选择暂存工作区 | 未请求对应策略时，读取和选择性网络访问仍为 ambient/协作式；暂存挂载需要 macFUSE |
| 原生 OCI 容器 | Linux OCI runtime、镜像用户空间和配置的挂载及网络 | 不声明所有 capability 维度均已完整强制执行 |
| libkrun VM | 独立 Linux 客户机内核、virtio-fs 工作区和 smoltcp 网络路径 | 需要 KVM 或 HVF；宿主连接器和共享文件仍是边界的一部分 |

应检查具体 Run Bundle。配置表达请求，安装的控制及证据说明实际发生了什么。safe-best-effort 可能报告控制降级。`--strict` 在任一必需维度缺少强制执行证据时拒绝运行；当前 executor 不声明完整的 Subprocess 强制执行，因此它不是现成的更强沙箱预设。

## 工作区与生命周期

文件系统访问、网络隔离和改动暂存是独立设置。Host 默认保留宿主文件系统视图。需要限制路径访问时使用 `--filesystem sandbox`，需要审查改动时显式启用暂存：

```bash
pvisor run --stage ../stage-001 -- codex
pvisor run --filesystem sandbox --overlaynet-deny-all -- codex
```

没有 OverlayFS 选项时，host 命令可能直接写入项目。暂存不能回滚远程 API 调用或覆盖工作区之外的写入。

宿主进程 executor 创建进程组，在完成或取消后向整组发送终止信号，并在宽限期后升级终止。后代持有输出管道时，读取等待也有时限。主动脱离进程组的进程需要更强的平台约束；进程组清理本身不是完整的后代隔离边界。

CLI 检查点要求 Run 已停止，保存文件系统上层，不保存进程内存或冻结所有底层宿主文件。嵌入式 AgentCtl 参与者可以配合静默协议，但这不会使任意子进程变成可检查点恢复的进程。

## 网络边界

host/container 的选择性路由使用显式代理，忽略代理的客户端可以绕过它。宿主 deny-all 和 VM 网络采用不同的强制机制。VM 数据面支持 IPv4 TCP、DHCP 和合成 DNS；不支持的流量失败关闭。支持的协议和连接器限制见 [OverlayNet](overlaynet.md)。

## 后续设计

LiteBox、Firecracker、Virtualization.framework 执行和通用 host/container 透明拦截属于设计方向，不是当前可选择的生产后端。集群准入、证明和租户隔离需要独立实现及验收证据，不能从现有 Run 模型推导出这些能力。

环境配置见[执行环境](../guides/execution.md)，证据解释见[安全与证据](../concepts/security-evidence.md)。
