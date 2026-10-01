# 选择执行环境

按命令需要的内核和用户空间选择 executor。使用 `--safe` 或 `--stage PATH` 暂存工作区改动，运行后再审查；默认写入与清理规则见 [暂存与存储](../reference/cli.md#暂存与存储)。

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

Linux 需要可访问的 `/dev/kvm`。macOS 上用 `just build release` 构建并签署 Hypervisor entitlement。静态 Rust guest supervisor 使用 Rust 自带 linker。VM 网络支持策略控制的 IPv4 TCP、固定 guest 地址和合成 DNS；应用 UDP 流量、IPv6、QUIC 和入站连接不在当前支持范围内。

## 组合底层与提交方式

`--mount SOURCE[:TARGET]:stage` 把宿主目录加入工作区视图的底层。
`--mount SOURCE:read` 授予原绝对路径的只读访问，要求 host executor 和 `--safe/--ask`；
`SOURCE:write` 授予直接写宿主路径的权限。read/write 不支持改写 TARGET，也不经过工作区的审批规则。
重叠限制见 [文件系统参数](../reference/cli.md#文件系统参数)。

`--access PATH-GLOB:deny|ask|warn` 在 OverlayFS 视图中拒绝、询问或放行并警告；
只读共享使用 `--mount …:read`。`--stage PATH` 可指定暂存位置。

宿主进程 Run 在完成或超时后清理进程组，并限制等待输出管道的时间；脱离该组的后代不能仅靠进程组清理来约束。

继续阅读[审查并应用](review-apply.md)或[网络策略](network.md)。
