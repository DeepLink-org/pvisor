# 执行器边界

各执行器按能力维度提供的保护范围如下。所有结论都描述**请求了对应控制时**的情况；某次 Run 是否真正安装了控制，以它的 Run Bundle 为准。

网络维度的细节（每种执行路径能否被绕过、VM 数据面的协议覆盖）以[网络边界](../guides/policies/network.md#网络边界)为准；机制见[隔离设计](../design/isolation.md)。

## 总览

| 维度 | host（Linux） | host（macOS） | container | VM |
| --- | --- | --- | --- | --- |
| 工作区写入 | 暂存时经 OverlayFS（FUSE），apply 前不落地 | 暂存时经 OverlayFS（macFUSE） | 暂存时经 OverlayFS | 经 virtio-fs 服务的写时复制工作区 |
| 宿主其他路径的写入 | `--filesystem sandbox`：namespace、投影根目录与 Landlock | `--filesystem sandbox`：Seatbelt 写入范围 | 镜像用户空间与配置的挂载 | 独立 guest；根目录写入进临时 upper，退出丢弃 |
| 读取 | 默认 ambient；`--filesystem sandbox` 下受投影根目录与 Landlock 约束 | 默认 ambient；`--safe` 申请 Seatbelt 读取范围，但当前 Run 证据仍把读取报告为 ambient（见[已知限制](known-limitations.md)） | 镜像内容与配置的挂载 | 只有 rootfs 与声明的挂载；`--rootfs host` 会暴露宿主内容 |
| 视图内敏感路径 | `--access` 规则与 `--safe` 预设，经 OverlayFS 执行 | 同左 | 同左 | 同左，并作用于根视图 |
| 网络（选择性策略） | 协作式代理，可被绕过 | `--safe` 下阻断直接连接，代理执行选择性规则 | 协作式代理，需 host 网络 | 不可绕过的 smoltcp 数据面 |
| 网络（全部拒绝） | 私有 network namespace | Seatbelt 阻断非 loopback IP 与 ambient Unix socket | `--container-network none` | `--overlaynet off` |
| HOME 与凭据 | `--safe`：HOME 写时复制视图；凭据需显式 `--pass-env` | `--safe`：临时 HOME，不能读取原主目录 | 只有显式挂载与传入的变量 | 取决于 rootfs；`--rootfs host` 保留宿主 HOME |
| 子进程 | user/mount/PID namespace；进程组清理 | 进程组清理 | 容器进程树 | guest 内核内 |
| `--safe` 是否可用 | 可用 | 可用 | **拒绝启动**（缺少完整强制边界） | 可用，要求 `auto` 网络 |

## 单机 daemon 边界 {#daemon}

上表描述原生 Job 请求的控制，不是 daemon API。VM-only NativeRuntime 在独立 supervisor 中嵌入 pVisor，使用独立不可变 guest rootfs 与私有写入；委派 cgroup v2 限制 supervisor/VM 树。Stage/apply 与 checkpoint/fork API 未实现，也不自动获取 node 共享。OpenSandbox 网络策略选项被拒绝；原生 OverlayNet 出口网络不构成 deny-all 声明。

宿主账户、daemon/firmware 与预制镜像属于可信输入。私有状态与同 UID IPC 不防御敌对宿主 UID/root 代码。其他本机用户可能访问 loopback 发布：使用真实服务鉴权与宿主控制。不承诺敌对多用户隔离或安全审计。未提供／验证的 bootstrap 与真实服务检查见 [daemon 运行时边界](../guides/daemon/boundaries.md)。

## 读表时要注意

- **一个维度的控制不提升其他维度。** 暂存文件不能证明网络已隔离；捕获到模型请求也不能证明没有其他连接。
- **子进程维度目前没有执行器声称完整强制。** 因此 `--strict`（要求每个请求维度都有不可绕过的强制证据）在 host、container、VM 上都会以 `UnsupportedPolicy` 退出。它用于验证失败关闭的行为，不是一个现成的更强沙箱预设。
- **macOS VM 不是敌对多租户边界**：VMM 仍拥有调用用户的宿主权限。
- **主动脱离进程组的后代**不受 host 进程组清理约束。

## 在 Run Bundle 中核对

`pvisor status --review` 的 Safety boundary 一节列出每个维度实际的控制和警告；`--json` 中 `executor_observations` 是强制力证据，`safety.*` 摘要从它派生。等级含义（`Unenforced`、`Cooperative`、`Enforced`）见[能力、证据与保证边界](../concepts/capabilities-and-evidence.md)。
