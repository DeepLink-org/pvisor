# 选择执行环境

两个快速开始示例分别使用 Host 和 VM。需要更换执行方式时，可按命令所需的内核和用户空间选择。

| Executor | 执行环境 | 前提 | 指南 |
| --- | --- | --- | --- |
| `host`（默认） | 宿主内核和已安装工具 | 暂存运行需要平台文件系统挂载支持；macOS 使用 macFUSE | [host](host.md) |
| `container` | Linux 宿主内核上的 OCI 镜像用户空间 | 原生 OCI runtime（`crun` 或 `runc`）及匹配的 Linux pVisor 二进制 | [container](container.md) |
| `vm` | libkrun 提供的 Linux 客户机内核 | Linux KVM 或 Apple Silicon HVF，以及匹配宿主架构的 Linux rootfs | [VM](vm.md) |

命令必须安装在选定环境中；使用容器或 VM 时，在其镜像内安装所需 Agent。

## 怎么选

- **只想审查文件改动、使用本机工具链**：host 加 `--safe`。
- **需要强制网络边界**：VM（`--overlaynet auto`），或 host 加 `--overlaynet-deny-all`。
- **需要固定的 Linux 用户空间**：container 或 VM 加 OCI 镜像。
- **需要独立内核**：VM。

用 `--executor` 选择执行器。`--safe` 要求文件和网络隔离，无法落实时拒绝启动。当前 container 不支持 `--safe`；暂存与网络配置见[容器执行器指南](container.md)。

<a id="_3"></a>

## 配置与验证 {#configuration-and-verification}

确定执行器后，按上表中的指南准备环境。挂载、路径访问与暂存参数见 [CLI 文件系统参数](../../reference/cli.md#文件系统参数)；需要限定具体文件时，按[文件策略](../policies/files.md)配置。

运行后查看 `pvisor status --review`，核对 Run Bundle 中实际安装的控制与警告。各执行器的隔离范围和平台限制见[执行器边界](../../security/executor-boundaries.md)。
