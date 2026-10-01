# libkrun VM

VM 执行器用 libkrun 启动最小的 Linux 客户机，提供独立的 guest 内核、通过 virtio-fs 服务的工作区，以及由 pVisor smoltcp 数据面处理的网络。Linux 使用 KVM，Apple Silicon macOS 使用 HVF。

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 \
  --stage ../stage-vm --mount "$PWD:stage" -- /bin/sh
```

## 准备 rootfs

| 方式 | 参数 | 说明 |
| --- | --- | --- |
| OCI 镜像 | `--rootfs image=<IMAGE>` | 直接拉取镜像，不调用 Docker、Podman 或 Buildah；校验 manifest 和 layer digest，按宿主架构选择 `linux/arm64` 或 `linux/amd64` |
| 准备好的目录 | `--rootfs <PATH>` | 使用已有 Linux rootfs |
| 宿主根目录（仅 Linux） | `--rootfs host` | 把宿主 `/` 作为只读底层，保留宿主 PATH 和 HOME；会暴露宿主内容供读取 |

需要可复现时固定镜像摘要。`--image-store` 可覆盖镜像缓存目录；多个 VM 共享镜像文件时，可以使用[共享镜像缓存](../../reference/shared-image-cache.md)。macOS 上必须显式提供 Linux rootfs 或镜像。

## 写入去向

- 合并后的 rootfs 是 guest `/`，`/workspace` 是 guest 的工作目录；
- 工作区改动进入指定 stage 或默认 Job 存储，退出后保留，供审查和 apply；
- VM 根目录的其他写入使用临时 upper，VM 退出时丢弃；
- 镜像缓存被标为不可变，`apply` 不能改写被其他 Run 共享的 rootfs。

## 网络

| 模式 | 行为 |
| --- | --- |
| `--overlaynet auto`（默认） | 静态 guest IPv4 地址、合成 DNS、受策略控制的 IPv4 TCP；guest 没有直接网络旁路 |
| `--overlaynet off` | 不配置 guest 网络，离线 |

不支持的 UDP、IPv6、ICMP、QUIC 和入站转发会失败关闭。`deny-all` 仍允许已配置的内部 Gateway 路由；需要完全离线时关闭 Gateway 并使用 `off`。VM 不支持 `proxy` 模式。

## 平台准备

- **Linux**：需要可访问的 `/dev/kvm`；静态 musl 构建内嵌 guest 内核，运行时不需要固件共享库；
- **macOS**：用 `just build release` 构建并签署 Hypervisor entitlement；wheel 自带 `libkrunfw`，源码运行时会下载经 SHA-256 校验的固定版本。

## 已知缺口

- Linux 另外用 namespace 和 Landlock 约束 VMM；**macOS 上 VMM 仍拥有调用用户的宿主权限**，因此尽管有 guest 内核隔离，也不应把它当作敌对多租户边界；
- 宿主连接器和共享文件仍是边界的一部分；
- `--safe` 要求 VM 使用现有的 `auto` 网络边界，但不会自动选择 VM。
