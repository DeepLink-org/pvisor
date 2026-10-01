# libkrun VM

提供独立 Linux 客户机内核、virtio-fs 工作区与 smoltcp 网络路径。Linux 需要 KVM，Apple Silicon macOS 使用 HVF。VM 网络支持策略控制的 IPv4 TCP、固定 guest 地址与合成 DNS。

!!! note "TODO"
    补 rootfs 准备、镜像缓存（见 reference/shared-image-cache）、资源限制与已知缺口。

