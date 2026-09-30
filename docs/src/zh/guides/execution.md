# 选择执行环境

按命令需要的内核和用户空间选择 executor。普通 host Run 默认直接写入工作区；`--safe` 暂存工作区写入。需要在运行后审查时用 `--stage PATH` 保留改动。

| Executor | 执行环境 | 前提 |
| --- | --- | --- |
| `host`（默认） | 宿主内核和已安装工具 | 暂存运行需要平台文件系统挂载支持；macOS 使用 macFUSE |
| `container` | Linux 宿主内核上的 OCI 镜像用户空间 | 原生 OCI runtime（`crun` 或 `runc`）及匹配的 Linux pVisor 二进制 |
| `vm` | libkrun 提供的 Linux 客户机内核 | Linux KVM 或 Apple Silicon HVF，以及匹配宿主架构的 Linux rootfs |

命令必须安装在选定环境中。宿主装了 Agent，不代表 Ubuntu 镜像也有它。实际安装的控制、警告和平台限制以 Run Bundle 为准。

## 宿主命令

```bash
pvisor run --executor host --stage ../stage-host -- /bin/sh
```

Host 默认保留宿主文件系统视图。需要限制路径访问时使用 `--filesystem sandbox`，需要可审查的写时复制工作区时使用 `--stage PATH`，需要拒绝所有网络时使用 `--overlaynet-deny-all`。这些设置相互独立，网络参数不会启用文件系统限制。

## Linux 容器

```bash
pvisor run --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

此路径使用原生 OCI executor。额外挂载和注入的 pVisor 二进制配置见 [CLI 参考](../reference/cli.md)。Gateway 和显式 OverlayNet 代理当前要求容器使用 host 网络，因为注入的地址是宿主回环地址。

## 使用 OCI 镜像的 VM

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 \
  --stage ../stage-vm --mount "$PWD:stage" -- /bin/sh
```

也可用 `--rootfs DIR` 指定准备好的 Linux rootfs。镜像直接拉取，VM executor 不需要 Docker。需要可复现时固定镜像摘要。rootfs 写入是临时的；工作区暂存内容会保留供审查。

Linux 上的 `--rootfs host` 可将宿主根目录作为只读底层提供给独立客户机内核。这会暴露宿主内容供读取。应通过 `--mount` 显式声明需要暴露的路径。

Linux 需要可访问的 `/dev/kvm`。macOS 上用 `just build release` 构建并签署 Hypervisor entitlement；源码构建还需要 Zig。VM 网络支持策略控制的 IPv4 TCP、DHCP 和合成 DNS；应用 UDP 流量、IPv6、QUIC 和入站连接不在当前支持范围内。

## 组合底层与提交方式

`--mount SOURCE[:TARGET]:read|stage|write` 声明宿主路径；省略 target 时 target 等于 source。`read` 和 `stage` 当前都作为工作区视图的底层，`write` 授予直接写宿主路径的权限。`--access PATH-GLOB:deny|read` 在视图内应用规则：`deny` 隐藏匹配路径，`read` 当前仅记录访问警告，尚未强制只读。`--stage PATH` 指定持久 stage；`--safe` 未指定 stage 时使用临时 stage。

| Stage 生命周期 | 行为 |
| --- | --- |
| `--stage PATH` | 保留改动，由 `status --review`、`apply` 或 `drop` 决定 |
| 仅 `--safe`，未指定 `--stage` | 使用临时目录，Run 结束后自动丢弃 |
| 普通 host Run，未指定 `--stage` | 直接写入 lower |

需要根据审查结果决定是否接受改动时，显式指定 `--stage PATH`。宿主进程 Run 在完成或超时后清理进程组，并限制等待输出管道的时间；脱离该组的后代不能仅靠进程组清理来约束。

继续阅读[审查并应用](review-apply.md)或[网络策略](network.md)。
