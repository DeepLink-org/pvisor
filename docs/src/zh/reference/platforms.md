# 平台与执行器支持矩阵

先按你所在的平台选择执行器，再检查该执行器的前置条件。Linux 的宿主任务适合快速开始；需要 Linux 用户空间或更独立的内核时选择 VM。Apple Silicon 上的 VM 使用 Linux aarch64 用户空间。

安装后还需检查所需控制是否生效，以每次运行的 Run Bundle 执行器观察为准。

## 当前机制与准备条件

按下面的条件准备执行环境；需要的功能能否启用，以一次小任务及其 Bundle 验证。

| 平台 | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | 暂存需要 FUSE；sandbox 需要可用 user namespace 与 Landlock | 需要原生 `crun`/`runc` 与匹配的静态 Linux 二进制 | 需要 `/dev/kvm`；musl 发布产物内嵌 guest 内核 |
| Linux arm64 | 有源码路径；先核对构建与测试证据 | 需要匹配 `linux/arm64` 的注入二进制 | 需要该宿主/构建的 KVM 支持及匹配 rootfs；不从 x86_64 wheel 推导支持 |
| macOS Apple Silicon | 暂存需要 macFUSE 5.4.0+ 的 FSKit；文件/网络使用 Seatbelt | 原生 OCI Linux 容器路径不支持 | HVF、Hypervisor 签名与 Linux arm64 rootfs/image；不需要 macFUSE |
| Windows / macOS Intel | 当前发布说明不承诺支持 | 不承诺 | 不承诺 |

此表的 `container` 指原生 Job 的 OCI 执行器（`--executor container`）。[Docker 风格容器命令](cli.md#container-commands)中的 `pvisor run` 使用独立 Linux VM，在 Linux x86_64/KVM 与 Apple Silicon/HVF 上有实现入口；它不采用此列的 OCI runtime，也不实现 Docker Engine API。

发布 wheel 当前面向 Linux x86_64 与 macOS arm64；有源码分支不等于有该架构的发布 wheel 或成熟度保证。先运行 `pvisor --version` 和小任务，再核对 Bundle，安装步骤见[安装指南](../start/installation.md)。

## 单机 daemon {#daemon}

Sandbox API 要求 Linux x86_64/KVM 和委派 cgroup v2；macOS HVF 与 Linux arm64 不在该 API 的支持范围。`prepare-image` 发布共享压缩镜像池中的模板，新模板默认用 Rust guest 引擎；OpenSandbox/E2B 官方 SDK 的核心流程已有真实 VM 验证，完整协议与生产密度不作保证。部署步骤见[daemon 指南](../guides/daemon/index.md)。

同一个可执行文件的缓存和内存池组件支持 Linux 与 Apple Silicon macOS，不需要开启 sandbox API。macOS 可用压缩池或显式 `physical-pages` 物理共享；它们的前提和故障边界见[内存优化](../design/memory-optimization/index.md)。

## 能力证据怎么验证

- 文件读写分别检查 `safety.filesystem_read_non_bypassable` 与 `filesystem_write_non_bypassable`，不要只看是否 staged。
- 网络检查 `safety.network_non_bypassable` 与 driver；Linux host 选择性代理、container host 网络不能因 executor 名称被当成强制边界。
- container 目前不提供 safe 所需的完整控制，`--safe` 会拒绝；所有当前执行器在 `--strict` 下因 Subprocess 缺口拒绝。
- 资源查看 `resources.effective` 与 `limitations`；macOS 不强制 RLIMIT_AS，普通 host rlimit 不是整棵进程树的总额度。

验证入口：`.github/workflows/ci.yml`、`docs/src/zh/cases/06-stage-apply.md`、`crates/pvisor/tests/` 与 `tests/test_vm_*`。尚未验证的矩阵格不作成熟度结论；安全范围见[执行器边界](../security/executor-boundaries.md)。
