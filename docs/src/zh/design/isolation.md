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

## 工作区与生命周期 {#workspace-and-lifecycle}

受 pVisor 管理的 stage 默认使用 `checkpoint` 持久化策略。执行期间追加首次观察记录，不在每次首次修改时同步落盘；workspace checkpoint 持久化自己的副本。所有写入者停止后，任务完成依次同步 preimage 日志、upper 文件和目录，最后发布持久化完成标记。中断后没有完成标记的 stage 不能 apply，也不能重新打开并冒充完整结果；应恢复已提交的 checkpoint 或丢弃。使用 `--stage-durability strict`（配置为 `overlayfs.durability = "strict"`）保留首次修改前同步日志的策略。两种模式都会在程序显式 `fsync` 时先同步观察记录，再同步数据。没有策略文件的旧 stage 保留原来的严格语义。

两种模式都保留暂存隔离和内容冲突检测。冻结 baseline 复用已有、经过验证的 content receipt；可变 lower 仍记录原内容指纹，持久化策略不会将其降级为仅检查 metadata。运行中的 execution checkpoint 还保存 guest RAM 和设备状态；stage 完成标记本身不意味着能够恢复进程。

文件系统访问、网络隔离和改动暂存是独立设置。Host 默认保留宿主文件系统视图。需要限制路径访问时使用 `--filesystem sandbox`，需要审查改动时显式启用暂存：

```bash
pvisor run --stage ../stage-001 -- codex
pvisor run --filesystem sandbox --overlaynet-deny-all -- codex
```

默认写入和清理规则见 [暂存与存储](../reference/cli.md#暂存与存储)。暂存的覆盖范围见[能力与证据](../concepts/capabilities-and-evidence.md)。ZCode 的 Linux host 适配会向应用状态目录授予持久写权限；详见 [CLI 参考](../reference/cli.md)。

宿主进程 executor 创建进程组，在完成或取消后向整组发送终止信号，并在宽限期后升级终止。后代持有输出管道时，读取等待也有时限。主动脱离进程组的进程需要更强的平台约束；进程组清理本身不是完整的后代隔离边界。

默认 workspace `fork` 要求 Run 已停止；原生 execution fork 可以捕获并继续运行中的 VM，或使用历史执行检查点。快照范围见[执行模型](../design/execution-model.md)。嵌入式 AgentCtl 参与者可以配合静默协议，但这不会使任意子进程变成可检查点恢复的进程。

## 网络边界

各执行路径的控制范围、能否绕过以及 VM 数据面的协议覆盖见[网络边界](../guides/policies/network.md#网络边界)，设计机制见 [OverlayNet](overlaynet.md)。

环境配置见[执行环境](../guides/executors/index.md)，证据解释见[能力与证据](../concepts/capabilities-and-evidence.md)。
