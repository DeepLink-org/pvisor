# 平台与执行器支持矩阵

Linux x86_64 和 macOS Apple Silicon 提供完整 CLI 安装。Windows x86_64 目前只支持
源码构建的 `pvisor vm-run`，还不能运行 Job、TUI、replay 或本地 Daemon。

一个 `pvisor.exe` 足够运行这个 VM 入口，但仍需 `libkrunfw.dll`、Linux rootfs 和
其中的工作负载。三平台验收尚未全部通过：提交 `5b320104` 的 Windows/WHP 和
Linux/KVM 负载通过，macOS CI 在创建 HVF VM 时失败。

## 安装组件 {#binaries}

Linux/macOS 的完整 wheel 和 native archive 包含四个同版本、同目录的宿主程序。
普通非交互 Job 不要求常驻 Daemon；其他组件按所用功能调用。

| 程序与归属 | 何时使用 | Windows 状态 |
| --- | --- | --- |
| `pvisor` / `pvisor-cli` | 命令入口；自行启动普通 Job owner，`vm-run` 在当前进程执行 VM | 仅 `pvisor.exe` 的 `vm-run`、help/version |
| `pvisor-tui` / `pvisor-cli` | `pvisor tui`、`spawn --tui/--ask`、交互 resume；TUI 启动同目录 `pvisor`，管理 PTY、终端和审查面板 | 未移植 |
| `pvisor-replay` / `pvisor-cli` | `pvisor replay`；managed replay 再调用同目录 `pvisor`，沿用 Job/轨迹契约 | 未移植 |
| `pvisor-daemon` / 独立 crate | `pvisor daemon`、cache/sandbox 服务；`--vm-memory-pool auto` 通过它发现或启动共享内存池 | 未移植 |

只复制主程序会使依赖 companion 的命令不可用。Windows CI 使用 `--bin pvisor`；
`cargo build -p pvisor-cli` 的全部目标和完整 Windows 安装包尚未支持。
wheel 目标为 Linux x86_64 musl 和 macOS arm64，安装步骤见[安装指南](../start/installation.md)。

`pvisor-guest` 是 `pvisor-vm` 内嵌的 Linux PID 1 supervisor，在 Windows 上交叉编译后仍是
Linux payload，不需要安装宿主 `pvisor-guest.exe`。`whp_smoke.exe` 是 CI 探针。
`containerd-shim-pvisor-v2` 用于 Linux containerd 集成，单独交付。guest 工作负载
须为 Linux 程序。

构建清单见[程序清单与构建目标](https://github.com/DeepLink-org/pvisor/blob/main/scripts/packaging/stage_wheel_binaries.py)。
同目录程序的权限检查和派发见[companion 查找与信任检查](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/companions.rs)，
内存池调用见[自动池发现](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/cli/daemon.rs)。

## 当前机制与准备条件

| 功能 | Linux x86_64 | macOS Apple Silicon | Windows x86_64 |
| --- | --- | --- | --- |
| `vm-run` | KVM、可读写 `/dev/kvm`；Linux x86_64 guest | HVF、Hypervisor 签名；Linux aarch64 guest，不需要 macFUSE | WHP；启用功能并重启，嵌套 VM 需虚拟化透传；Linux x86_64 guest |
| Host Job（`spawn`）、OverlayFS、review/apply/drop | 暂存需要 FUSE；sandbox 需要 user namespace 和 Landlock | 暂存需要 macFUSE 5.4.0+ FSKit；文件/网络控制使用 Seatbelt | 未移植 |
| 原生 OCI Job（`container` / `--executor container`） | `crun`/`runc` 和匹配的静态 Linux 注入程序 | 不支持 | 不支持 |
| Docker 风格 VM 容器及网络策略 | 有实现和独立契约 | 有实现，各能力需 HVF 验证 | 未移植 |
| Linux host-root `stage`、`:stage` bind | Linux 专用路径 | 不支持 Linux host-root；staged bind 不推定与 Linux 等价 | 不支持 |
| VM 快照、恢复、冷 RAM | 有 API 和测试，受 backing/profile 限制 | 有 API 和测试，受 HVF/backing 限制 | Unsupported；仅有暂停/继续 |
| Gateway | 按所选执行器和配置启用 | 有宿主执行路径 | 此次 CLI 不包含 |

[Docker 风格容器命令](cli.md#container-commands)中的 `pvisor run` 每个容器使用
独立 Linux VM，与原生 OCI Job 分开，也不实现 Docker Engine API。
Linux arm64 有源码路径，需核对该宿主的构建/KVM 测试及匹配 rootfs；OCI 还需
`linux/arm64` 注入程序。x86_64 wheel 不覆盖 arm64。Intel macOS 无当前发布支持
承诺，也不在 `vm-run` 范围内。

安装后先运行 `pvisor --version`。Job 的有效控制查看 Run Bundle；`vm-run` 的
结果通过退出码和宿主文件检查。

## 平台实现差异 {#implementation-parity}

三平台共用 `vm-run` 的参数、GuestConfig 和认证退出协议。执行链为
`pvisor vm-run → pvisor_vm::api::VmBuilder → 平台 VMM →
virtio-fs → Rust PID 1 supervisor → Linux workload → token 认证退出`。

| 实现层 | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CLI 与进程 | 完整命令树、Job owner、信号和后代清理 | 相同 Job/companion 结构，终端与资源控制受 macOS 限制 | 最小命令树调用共享 `vm-run`；无 Job owner、控制 socket 或 Job 状态记录 |
| companion | 同目录发现、Unix uid/mode 检查、`exec` 派发 | 相同检查；解析 Homebrew 入口的真实安装目录 | 无 companion 派发；完整移植需定义 `.exe` 查找、ACL 信任及进程启动/退出传播 |
| 终端 | PTY、终端前台归属和信号 | PTY、终端恢复；TUI 为独立程序 | 未接入 Windows 控制台/PTY 生命周期 |
| 共享服务 | Unix socket、同用户认证、锁和 readiness；sandbox 另需 cgroup/KVM | 使用 Unix IPC；支持缓存/池，sandbox API 不支持 | IPC、端点 ACL、锁、取消和服务回收尚未移植；连接远端 Linux API 不等于本地服务可用 |
| VM 与文件系统 | KVM ioctl、Unix virtio-fs；完整运行时还有 overlay/network 配置 | HVF、Unix virtio-fs；受签名、页大小和宿主权限影响 | WHP 分区、APIC/PIT/TSC、Win32 waitable handle 和目录共享；Linux wire errno、标志位及 ABI 宽度需转换 |

共同目录共享路径关闭 DAX。Windows 合成 Linux mode/uid/gid，宿主 ACL 仍生效，
不提供完整 POSIX 元数据语义。调用方须独占管理 rootfs，避免不可信宿主进程改写
共享树。

完整 Windows 移植还需验证同目录安装、版本/权限检查，以及 CLI→TUI→CLI、
CLI→replay→CLI、CLI→Daemon 的实际调用。测试须覆盖终端恢复、取消/超时、
后代回收、服务就绪和失败恢复，并分别核对 Job JSON/Bundle、review/apply 和
服务协议。只有 `--help` 或 VM 负载通过，尚不足以验收这些调用。

## 三平台的特性集合与关键路径

`vm-run` 直接修改调用方提供的 rootfs，不创建 Job、Bundle、stage 或容器实例。
它不提供网络/vsock、交互 stdin、独立 task stdio、容器生命周期、快照或 Job 策略。
输出走 VM 控制台，可能含内核日志。workload 退出码直接返回；参数错误为 2、
准备/运行错误为 1，退出认证失败或 VM 异常终止为 125。超时和进程终止由外部调用方负责。

| 输入/变量 | 影响与要求 |
| --- | --- |
| `--cpus`、`--memory` | vCPU 启动、中断及内存压力；允许 1 至 64 vCPU、1 至 65536 MiB，固件/宿主可能进一步限制；本组测试只覆盖 1/2 CPU、256 MiB |
| `--firmware` | 加载可信、匹配 guest 架构的宿主 `.so`/`.dylib`/`.dll`；省略时必须有内嵌 kernel。musl 发布产物内嵌内核，Windows 目前需显式 DLL |
| `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` | 产品构建选择的固件/内核输入，区别于运行期 `--firmware` |
| `--rootfs`、`--readonly` | 非重叠的普通目录、绝对 UTF-8 宿主路径；只读目录挂到 `/readonly`，guest 命令须为绝对 Linux 路径 |
| 宿主/guest 页大小、文件系统、大小写、路径长度、ACL | 影响 mmap、rename、inode 缓存和 errno；本组用例未覆盖全部 POSIX 差异 |
| `--seed`、`--rounds`、`--timeout`（测试驱动） | 控制数据序列、重复启停次数和单次截止时间；线程调度和耗时不固定 |

## 统一 VM 用例与三平台 CI

### 运行记录

[PR #92 的首轮 CI](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)对应
提交 `5b320104`，主 CI 尚未全部通过。

| 平台 | 构建及单元测试 | 共同 VM 负载 |
| --- | --- | --- |
| Windows | MSVC 原生构建/契约、WHP API smoke 通过 | WHP 通过 |
| Linux | 该组负载使用产品构建 | KVM 通过 |
| macOS | 四程序产品构建和独立 CLI 测试通过；core shard 另有 replay 测试超时 | HVF `VmCreate` 失败，guest 未启动 |

本机 Apple Silicon 另有 12 次加 120 次 HVF 启动通过，记录属于另一台宿主。
后续提交需重新检查 CI。审查发现及未测项目见[平台审查记录](https://github.com/DeepLink-org/pvisor/blob/main/tests/PLATFORM_REVIEW.md)。

### 用例和结果检查

[CLI 驱动](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_cli_contract.py)通过
真实 `pvisor` 执行同一份[guest 负载](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_guest.rs)。
默认 3 轮，每轮检查 `(1 CPU, exit 0)`、`(2, 0)`、`(2, 7)`、`(1, 255)`，共 12 次启动。
每次运行 8 个线程，每个线程做 32 次 4 至 8 KiB 写入、fsync、rename、读取和删除，
并触碰共 16 MiB 内存。还检查在线 CPU、100 ms 计时器、只读拒写和缓存子文件后的目录重命名。

宿主检查退出码、与 seed/CPU/退出码精确匹配的结果、未改动的只读 fixture 和空工作目录。
缺少虚拟化、固件错误或超时均失败。驱动超时时终止并回收进程树，保留逐场景日志、
命令、哈希、rootfs 和 JSON。定时器上界仅检查活性；确定性指相同输入得到相同退出
状态和文件结果，不要求调度/耗时相同。小时级稳定性和生产密度尚未验证。

### CI 配置和手动运行

| CI 路径 | 准备与执行 | 制品 |
| --- | --- | --- |
| `ci.yml` / Rust Test / Linux / pvisor | 构建产品、x86_64 musl guest，检查 `/dev/kvm`，执行共同驱动 | `vm-cli-Linux` |
| `ci.yml` / Rust Test / macOS / pvisor | 构建并签名产品、aarch64 musl guest，执行相同驱动 | `vm-cli-macOS` |
| `windows-vm.yml` / native + firmware + whp | MSVC 构建 `pvisor --bin` 并测 parser；Linux 构建 DLL 和 x86_64 guest；Windows 执行相同驱动 | `windows-vm-whp-log` |

Windows 原有 `whp_smoke` 继续检查 pause/resume、ready 回调失败清理和旧退出 ioctl
拒绝。Windows CLI 构建使用 `cargo build --locked -p pvisor-cli --bin pvisor`，
测试使用 `mise run test -- pvisor-cli -- --lib --bin pvisor`；现有 Unix 集成测试不在此选择中。
CI 用例失败会使主 CI 失败；WHP 不可用或需要重启时也会失败。

手动运行前准备可执行的 `pvisor`、匹配架构的静态 guest 和固件；macOS 还需签名。

```bash
# Linux: x86_64-unknown-linux-musl; Apple Silicon: aarch64-unknown-linux-musl.
mise exec -- rustc --edition=2021 --target aarch64-unknown-linux-musl -C linker=rust-lld tests/vm_guest.rs -o /tmp/vm-guest
python3 tests/vm_cli_contract.py --binary target/debug/pvisor --guest /tmp/vm-guest --output target/vm-cli-evidence
# Windows uses the same driver with --firmware C:/firmware/libkrunfw.dll.
# --output must be new; --rounds 30 expands coverage to 120 actual boots.
```

## 单机 daemon {#daemon}

Sandbox API 要求 Linux x86_64/KVM 和委派 cgroup v2；macOS HVF 与 Linux arm64 不在该 API 的支持范围。`prepare-image` 发布共享压缩镜像池中的模板，新模板默认用 Rust guest 引擎；OpenSandbox/E2B 官方 SDK 的核心流程已有真实 VM 验证，完整协议与生产密度不作保证。部署步骤见[daemon 指南](../guides/daemon/index.md)。

同一个可执行文件的缓存和内存池组件支持 Linux 与 Apple Silicon macOS，不需要开启 sandbox API。macOS 可用压缩池或显式 `physical-pages` 物理共享；它们的前提和故障边界见[内存优化](../design/memory-optimization/index.md)。

## 能力证据怎么验证

- 文件读写分别检查 `safety.filesystem_read_non_bypassable` 与 `filesystem_write_non_bypassable`，不要只看是否 staged。
- 网络检查 `safety.network_non_bypassable` 与 driver；Linux host 选择性代理、container host 网络不能因 executor 名称被当成强制边界。
- container 目前不提供 safe 所需的完整控制，`--safe` 会拒绝；所有当前执行器在 `--strict` 下因 Subprocess 缺口拒绝。
- 资源查看 `resources.effective` 与 `limitations`；macOS 不强制 RLIMIT_AS，普通 host rlimit 不是整棵进程树的总额度。

验证入口：`.github/workflows/ci.yml`、`docs/src/zh/cases/06-stage-apply.md`、`crates/pvisor/tests/` 与 `tests/test_vm_*`。尚未验证的矩阵格不作成熟度结论；安全范围见[执行器边界](../security/executor-boundaries.md)。
