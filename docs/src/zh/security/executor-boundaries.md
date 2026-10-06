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

上表描述原生 `pvisor run` 执行器，不描述 `pvisor-daemon`。daemon 当前的外部 rootless Podman 后端共享宿主内核，安装私有 namespace/no-new-privileges/`cap-drop=ALL` 和资源限制，要求 prepared execd/egress 服务。它未接入原生 VM、stage/apply、checkpoint/fork 或 pVisor 网络策略。不支持的网络策略请求被拒绝；rootless slirp4netns 不等于 deny-all egress。

宿主账户、Podman 配置/hooks 与 prepared image 属于可信输入。其他本机用户可能访问原生发布的 loopback 端口，单靠 daemon 端点认证不保护这些端口；需要真实 upstream 服务认证与宿主网络控制。没有敌对多用户隔离或安全审计承诺。镜像契约及当前 upstream egress capability 冲突见 [daemon 运行时边界](../guides/daemon/boundaries.md)。

## 读表时要注意

- **一个维度的控制不提升其他维度。** 暂存文件不能证明网络已隔离；捕获到模型请求也不能证明没有其他连接。
- **子进程维度目前没有执行器声称完整强制。** 因此 `--strict`（要求每个请求维度都有不可绕过的强制证据）在 host、container、VM 上都会以 `UnsupportedPolicy` 退出。它用于验证失败关闭的行为，不是一个现成的更强沙箱预设。
- **macOS VM 不是敌对多租户边界**：VMM 仍拥有调用用户的宿主权限。
- **主动脱离进程组的后代**不受 host 进程组清理约束。

## 在 Run Bundle 中核对

`pvisor status --review` 的 Safety boundary 一节列出每个维度实际的控制和警告；`--json` 中 `executor_observations` 是强制力证据，`safety.*` 摘要从它派生。等级含义（`Unenforced`、`Cooperative`、`Enforced`）见[能力、证据与保证边界](../concepts/capabilities-and-evidence.md)。
