# 选择执行环境

按命令需要的内核和用户空间选择 executor。使用 `--safe` 或 `--stage PATH` 暂存工作区改动，运行后再审查；默认写入与清理规则见 [暂存与存储](../../reference/cli.md#暂存与存储)。

| Executor | 执行环境 | 前提 | 指南 |
| --- | --- | --- | --- |
| `host`（默认） | 宿主内核和已安装工具 | 暂存运行需要平台文件系统挂载支持；macOS 使用 macFUSE | [host](host.md) |
| `container` | Linux 宿主内核上的 OCI 镜像用户空间 | 原生 OCI runtime（`crun` 或 `runc`）及匹配的 Linux pVisor 二进制 | [container](container.md) |
| `vm` | libkrun 提供的 Linux 客户机内核 | Linux KVM 或 Apple Silicon HVF，以及匹配宿主架构的 Linux rootfs | [VM](vm.md) |

命令必须安装在选定环境中。宿主装了 Agent，不代表 Ubuntu 镜像也有它。实际安装的控制、警告和平台限制以 Run Bundle 为准；各执行器在每个能力维度上的边界见[执行器边界](../../security/executor-boundaries.md)。

## 怎么选

- **只想审查文件改动、使用本机工具链**：host 加 `--safe`。
- **需要强制网络边界**：VM（`--overlaynet auto`），或 host 加 `--overlaynet-deny-all`。
- **需要固定的 Linux 用户空间**：container 或 VM 加 OCI 镜像。
- **需要独立内核**：VM。

`--safe` 不会替你选择执行器；它要求所选执行器落实文件和网络隔离，做不到时拒绝启动，而不是静默降级。

## 各执行器通用的参数

`--mount SOURCE[:TARGET]:stage` 把宿主目录加入工作区视图的底层。
`--mount SOURCE:read` 授予原绝对路径的只读访问，要求 host executor 和 `--safe/--ask`；
`SOURCE:write` 授予直接写宿主路径的权限。read/write 不支持改写 TARGET，也不经过工作区的审批规则。
重叠限制见 [文件系统参数](../../reference/cli.md#文件系统参数)。

`--access PATH-GLOB:deny|ask|warn` 在 OverlayFS 视图中拒绝、询问或放行并警告，见[文件策略](../policies/files.md)。`--stage PATH` 可指定暂存位置。
