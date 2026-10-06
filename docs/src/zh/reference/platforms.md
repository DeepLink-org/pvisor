# 平台与执行器支持矩阵

先按你所在的平台选择执行器，再检查该执行器的前置条件。Linux 的宿主任务适合快速开始；需要 Linux 用户空间或更独立的内核时选择 VM。Apple Silicon 上的 VM 使用 Linux aarch64 用户空间。

安装包可用与某项控制生效是两件需要分别检查的事。下面列出当前实现入口；一次运行的有效边界以 Run Bundle 的执行器观察为准。

## 当前机制与准备条件

按下面的条件准备执行环境；需要的功能能否启用，以一次小任务及其 Bundle 验证。

| 平台 | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | 暂存需要 FUSE；sandbox 需要可用 user namespace 与 Landlock | 需要原生 `crun`/`runc` 与匹配的静态 Linux 二进制 | 需要 `/dev/kvm`；musl 发布产物内嵌 guest 内核 |
| Linux arm64 | 有源码路径；先核对构建与测试证据 | 需要匹配 `linux/arm64` 的注入二进制 | 需要该宿主/构建的 KVM 支持及匹配 rootfs；不从 x86_64 wheel 推导支持 |
| macOS Apple Silicon | 暂存需要 macFUSE 5.4.0+ 的 FSKit；文件/网络使用 Seatbelt | 原生 OCI Linux 容器路径不支持 | HVF、Hypervisor 签名与 Linux arm64 rootfs/image；不需要 macFUSE |
| Windows / macOS Intel | 当前发布说明不承诺支持 | 不承诺 | 不承诺 |

发布 wheel 当前面向 Linux x86_64 与 macOS arm64；有源码分支不等于有该架构的发布 wheel 或成熟度保证。先运行 `pvisor --version` 和小任务，再核对 Bundle，安装步骤见[安装指南](../start/installation.md)。

## 单机 daemon {#daemon}

独立 daemon 当前后端要求 Linux、可信 rootless Podman 可执行文件绝对路径、cgroup v2 和委派 CPU/memory/PID controllers。它与原生执行器矩阵独立，没有 macOS HVF 或原生 KVM 集成。镜像须预先准备在本机，包含真实 OpenSandbox 1.1.0 execd 与无 capability egress；默认 upstream egress 与 `cap-drop=ALL` 冲突，目前没有经过端到端验证的镜像配方。见 [daemon 安装](../guides/daemon/index.md)与[运行时边界](../guides/daemon/boundaries.md)，不要从原生 VM 或 wheel 支持矩阵推导 SDK 就绪。

## 能力证据怎么验证

- 文件读写分别检查 `safety.filesystem_read_non_bypassable` 与 `filesystem_write_non_bypassable`，不要只看是否 staged。
- 网络检查 `safety.network_non_bypassable` 与 driver；Linux host 选择性代理、container host 网络不能因 executor 名称被当成强制边界。
- container 目前不提供 safe 所需的完整控制，`--safe` 会拒绝；所有当前执行器在 `--strict` 下因 Subprocess 缺口拒绝。
- 资源查看 `resources.effective` 与 `limitations`；macOS 不强制 RLIMIT_AS，普通 host rlimit 不是整棵进程树的总额度。

验证入口：`.github/workflows/ci.yml`、`tests/semantics/stage-apply.md`、`crates/pvisor/tests/` 与 `tests/test_vm_*`。尚未验证的矩阵格不作成熟度结论；安全范围见[执行器边界](../security/executor-boundaries.md)。
