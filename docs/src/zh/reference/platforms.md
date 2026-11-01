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
| Windows x86_64 | 未移植 | 未移植 | 源码构建的 `vm-run` / WHP，见下文限定范围 |
| macOS Intel | 当前发布说明不承诺支持 | 不承诺 | 不承诺 |

此表的 `container` 指原生 Job 的 OCI 执行器（`--executor container`）。[Docker 风格容器命令](cli.md#container-commands)中的 `pvisor run` 使用独立 Linux VM，在 Linux x86_64/KVM 与 Apple Silicon/HVF 上有实现入口；它不采用此列的 OCI runtime，也不实现 Docker Engine API。

发布 wheel 当前面向 Linux x86_64 与 macOS arm64；有源码分支不等于有该架构的发布 wheel 或成熟度保证。先运行 `pvisor --version` 和小任务，再核对 Bundle，安装步骤见[安装指南](../start/installation.md)。

## 二进制组成：一个 exe 是否够用？ {#binaries}

**只运行 `vm-run`，一个 `pvisor.exe` 足够承担宿主程序的职责；使用完整 pVisor
工作流则不够。** Windows 还需要匹配的 `libkrunfw.dll`、调用方准备的 Linux
rootfs 和其中的工作负载。当前没有把完整 Linux/macOS 产品合并成单 exe，也没有
移植 Windows Daemon/TUI/replay。Linux/macOS 的完整安装包含下面四个宿主程序。

| 程序与归属 | 实际职责及调用关系 | Linux x86_64 | macOS Apple Silicon | Windows x86_64 |
| --- | --- | --- | --- | --- |
| `pvisor` / `pvisor-cli` | 命令入口；普通 Job 拥有者由自身启动，`vm-run` 在当前进程直接运行 VM | 构建并打包完整 CLI | 构建并打包完整 CLI，VM 程序需 Hypervisor 签名 | 仅构建 `pvisor.exe` 的 `vm-run`、help/version |
| `pvisor-tui` / `pvisor-cli` | `pvisor tui`、Job 的 `--tui/--ask` 和交互 resume；TUI 启动同目录 `pvisor`，持有 PTY、终端与审查面板 | 构建并打包 | 构建并打包 | 未移植，未构建；`vm-run` 不提供这些选项 |
| `pvisor-replay` / `pvisor-cli` | `pvisor replay` 派发；managed replay 再调用同目录 `pvisor`，使用原有 Job/轨迹契约 | 构建并打包 | 构建并打包 | 未移植，未构建 |
| `pvisor-daemon` / 独立 crate | `pvisor daemon` 派发；管理共享服务，`--vm-memory-pool auto` 通过它发现/启动内存池；也提供显式 cache/sandbox 服务 | 构建并打包；sandbox API 需要 KVM/cgroup v2 | 构建并打包；仅缓存/池等组件，sandbox API 不支持 | 未移植，未构建；没有 Windows 本地服务契约 |

另外，`pvisor-guest` 是 Linux guest 内的 Rust PID 1 supervisor，通常作为
`pvisor-vm` 的内嵌 payload；在 Windows 上交叉编译它，不会生成需要在宿主安装的
`pvisor-guest.exe`。`whp_smoke.exe` 是 CI 的 VM API 探针，不是 Daemon 或产品
companion。`containerd-shim-pvisor-v2` 属于 Linux containerd 集成，不属于共同
四程序安装。固件和 Linux workload 也不能用 Windows 可执行文件替代。

按所用功能选择组件，而不是按可执行文件数量判断支持：

| 使用方式 | 所需宿主组件 | Windows 当前结论 |
| --- | --- | --- |
| 准备好 rootfs 后执行 `vm-run`，检查退出码与文件 | `pvisor`；Windows 另带可信固件 DLL | 本 PR 的范围；无需 Daemon/TUI/replay |
| 非交互 `spawn`、review/apply/drop | 完整 `pvisor`、对应 executor/driver；普通 Job 不要求常驻 Daemon | 尚不支持；单 exe 中缺少该运行时，增加几个空壳 exe 不能补齐 |
| `spawn --tui/--ask`、`pvisor tui`、交互 resume | 完整 `pvisor` + `pvisor-tui` + 可用 PTY | 尚不支持 |
| `pvisor replay` | `pvisor-replay`；managed 路径还需要完整 `pvisor` | 尚不支持 |
| 自动共享内存池、本地 cache 服务或 sandbox API | 对应 CLI/客户端 + `pvisor-daemon`；按服务分别满足平台条件 | 尚不支持；远端 Linux daemon 的 API 可达性不代表 Windows 本地 daemon 已移植 |

当前 wheel/native archive 的完整产品构建会选择四个宿主程序，wheel 目标为 Linux
x86_64 musl 和 macOS arm64。Windows CI 特意使用 `--bin pvisor`，验证的是 VM
CLI profile；不能改成发布整个 Windows 安装包或声称 `cargo build -p pvisor-cli`
全部目标通过。Unix 安装应保留四个程序的同版本、同目录关系；仅复制主程序会使
相应 companion 命令不可用。源码依据：
[程序清单与构建目标](https://github.com/DeepLink-org/pvisor/blob/main/scripts/packaging/stage_wheel_binaries.py)、
[companion 查找与信任检查](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/companions.rs)、
[自动池发现](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/src/cli/daemon.rs)。

## 实现差异与完整功能验收边界 {#implementation-parity}

| 实现层 | Linux | macOS | Windows 当前实现/缺口 |
| --- | --- | --- | --- |
| CLI 与进程归属 | 完整命令树；Job 自身进程、信号与后代清理 | 相同 Job/companion 结构，终端和资源约束受 macOS 限制 | 独立的最小命令树，仅进入共享 `vm-run` 实现；没有 Job owner、控制 socket 或 Job 状态记录 |
| companion 安装与调度 | 同目录发现、Unix uid/mode 检查、`exec` 派发 | 相同检查；先解析 Homebrew 入口的真实安装目录 | 没有 companion 发现/派发；为完整移植需要定义 `.exe` 查找、ACL 信任、进程启动与退出传播 |
| 终端和交互 | PTY、终端前台归属、信号 | PTY 与终端恢复；TUI 为独立程序 | 未接入 Windows 控制台/PTY 生命周期；`vm-run` 控制台输出不是 TUI 兼容性证据 |
| 共享服务与 IPC | Unix socket、同用户身份检查、锁与 readiness；sandbox 另有 cgroup/KVM 约束 | 共享服务仍用 Unix IPC；缓存/池可用，sandbox API 不支持 | 尚未定义等价 IPC、端点 ACL、锁、取消与服务回收语义；WHP 能启动 VM 不能证明这些已实现 |
| VM/文件系统 | KVM + Unix virtio-fs；完整运行时还可选择 overlay/network 等配置 | HVF + Unix virtio-fs；有签名、页大小和宿主权限差异 | WHP + Win32 普通目录共享；Linux wire ABI 转换、合成元数据；overlay/network/快照等仍 Unsupported |
| 交付 | 四程序产品包与独立集成制品 | 四程序产品包；Hypervisor 签名 | 源码/CI 的限定 profile，未提供完整 Windows wheel 或四程序产品包 |

若后续要让 Windows 支撑完整 pVisor，完成标准必须包含：各程序真实构建与安装，
同目录发现和版本/权限检查，CLI→TUI→CLI、CLI→replay→CLI、CLI→Daemon 的实际
调用，取消/超时/子进程回收，以及所用服务的就绪/失败恢复测试。再分别验证 Job
JSON/Bundle、review/apply、交互终端和服务协议，才可扩展支持矩阵。创建同名 stub、
只检查 `--help`，或只通过这组 VM 负载，都不能作为上述功能通过的证据。

## 三平台的特性集合与关键路径

Windows x86_64 现在有一个限定范围的源码构建入口：`pvisor vm-run`。
Linux x86_64/KVM、macOS Apple Silicon/HVF 和 Windows x86_64/WHP 共用
此命令的参数、guest 配置和退出认证协议。它直接启动 Linux guest，不创建 Job、
Bundle 或容器实例；普通目录共享会直接修改 rootfs，不经过 stage/review/apply。
Windows 的其他产品命令尚未移植，也没有 Windows wheel；Intel macOS 不在此入口范围内。

| 特性 | Linux x86_64 | macOS Apple Silicon | Windows x86_64 |
| --- | --- | --- | --- |
| `vm-run`、1/2 vCPU、内存配置、认证退出 | KVM | HVF，需要签名 | WHP，需要启用功能并重启；嵌套 VM 需虚拟化透传 |
| guest 架构 | Linux x86_64 | Linux aarch64 | Linux x86_64 |
| 普通目录 rootfs、只读附加目录 | 支持，无 DAX 的共同路径 | 支持，无 DAX 的共同路径 | 支持；合成 Linux mode/uid/gid，宿主 ACL 仍生效 |
| Host Job、OverlayFS、review/apply/drop | FUSE、namespace/Landlock | macFUSE FSKit、Seatbelt | 未移植；拒绝相应 CLI 命令 |
| Docker 风格 VM 容器及网络策略 | 有实现和独立契约 | 有实现；各能力需 HVF 证据 | 未移植；VM 网络/vsock 返回 Unsupported |
| Linux host-root `stage`、`:stage` bind、原生 OCI | Linux 专用路径 | 不支持 Linux host-root/原生 OCI；不推定 staged bind 等价 | 不支持 |
| VM 快照、恢复、冷 RAM | 有专门 API 与测试，受 backing/profile 限制 | 有专门 API 与测试，受 HVF/backing 限制 | Unsupported；不是暂停/继续 |
| Gateway、replay、daemon | 按各接口契约；sandbox daemon 要求 KVM/cgroup v2 | 宿主/Gateway/replay 路径；daemon 仅缓存/内存组件 | 此次 CLI 不包含这些接口 |

共同执行链为 `pvisor vm-run → pvisor_vm::api::VmBuilder → 平台 VMM →
virtio-fs → Rust PID 1 supervisor → Linux workload → token 认证退出`。
Linux 使用 KVM ioctl，macOS 使用 HVF 和 Hypervisor entitlement，Windows 使用 WHP
分区、APIC/PIT/TSC 与 Win32 waitable handle。WHP 共享路径的 Linux errno、标志位
和 ABI 宽度不能使用宿主 CRT 的编号/类型替代。rootfs 必须由调用方独占管理；尤其
Windows 不提供与 Linux 等价的完整 POSIX 元数据或对恶意宿主目录改写的安全承诺。

主要变量及故障定位：

| 输入/变量 | 改变的关键路径与检查点 |
| --- | --- |
| `--cpus`、`--memory` | vCPU 启动/中断、内存压力；允许 1–64 和 1–65536 MiB，但固件/宿主可能进一步限制；本组验收只覆盖 1/2 CPU、256 MiB |
| `--firmware` | 显式加载宿主 `.so`/`.dylib`/`.dll`；不传则必须有内嵌 kernel；Windows 目前需显式 DLL；必须使用匹配 guest 架构的可信本仓库固件 |
| `PVISOR_KRUNFW_PATH` / `PVISOR_KRUNFW_KERNEL_BUNDLE` | 构建期固件/内核输入，区别于运行期 `--firmware`；产品构建流程负责选择输入 |
| `--rootfs`、`--readonly` | 独立、非重叠的普通目录；rootfs 写入直接持久化，只读目录固定挂到 `/readonly`；绝对 UTF-8 宿主路径，guest 命令为绝对 Linux 路径 |
| 宿主/guest 页大小、文件系统、大小写、路径长度、ACL | mmap、rename、缓存 inode、errno；本套用例只证明列出的共同契约，不能推出所有 POSIX 行为等价 |
| `--seed`、`--rounds`、`--timeout`（测试驱动） | 固定数据序列、重复 VM 启停次数、单次超时；线程调度和墙钟耗时不固定 |

`vm-run` 不连接网络，不提供交互 stdin、独立 task stdio、容器生命周期、快照或
Job 策略。输出走 VM 控制台，可能含内核日志。正常返回 workload 退出码；参数错误
为 2、准备/运行错误为 1，认证失败/异常 VM 退出为 125。外部监督者负责截止时间与
进程终止；下面的驱动在超时时杀死进程树，保留失败证据。

## 统一 VM 用例与三平台 CI

同一个 [guest 负载](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_guest.rs) 与
[CLI 驱动](https://github.com/DeepLink-org/pvisor/blob/main/tests/vm_cli_contract.py) 验证三种后端，不使用 SDK 或用模拟
结果代替 guest。默认 3 轮，每轮检查 `(1 CPU, exit 0)`、`(2, 0)`、`(2, 7)`、
`(1, 255)`，共 12 次真实启动。每次有 8 个线程，各自执行 32 次 4–8 KiB 写入、
fsync、rename、读取和删除，触碰共 16 MiB 内存；另检查在线 CPU、100 ms 计时器、
只读拒写、缓存子文件后的目录重命名和宿主最终结果。结果必须与 seed/CPU/退出码
精确匹配，只检查进程退出码不能通过。定时器上界是宽松活性检查，不是性能指标。

| CI 路径 | 平台准备与执行 | 失败及证据 |
| --- | --- | --- |
| `ci.yml` / Rust Test / Linux / pvisor | 构建产品与 x86_64 musl guest，检查 `/dev/kvm` 权限，执行统一驱动 | `vm-cli-Linux` 保存逐场景日志、rootfs 和 JSON；失败阻断主 CI |
| `ci.yml` / Rust Test / macOS / pvisor | 构建并签名产品、aarch64 musl guest，使用真实 HVF 执行相同驱动 | `vm-cli-macOS`，相同断言与门禁 |
| `windows-vm.yml` / native + firmware + whp | 原生 MSVC 构建 `pvisor --bin` 与 parser 测试；Linux 构建 DLL 和相同 x86_64 guest；Windows 执行同一驱动 | `windows-vm-whp-log` 包含统一用例证据；无 WHP、需重启、超时均失败，不跳过 |

Windows 的既有 `whp_smoke` 继续验证 pause/resume、ready 回调失败清理及旧退出
ioctl 拒绝。原生契约验证与真实 VM 运行分别记录；编译成功不能替代后者。
CLI 的 Windows 构建选择 `cargo build --locked -p pvisor-cli --bin pvisor`，
测试选择 `mise run test -- pvisor-cli -- --lib --bin pvisor`；TUI/replay 和现有 Unix
集成测试不在 Windows 构建集合。

手动运行（先准备已签名/可执行的 `pvisor`、匹配架构的静态 guest 与固件）：

```bash
# Linux: x86_64-unknown-linux-musl; Apple Silicon: aarch64-unknown-linux-musl.
mise exec -- rustc --edition=2021 --target aarch64-unknown-linux-musl -C linker=rust-lld tests/vm_guest.rs -o /tmp/vm-guest
python3 tests/vm_cli_contract.py --binary target/debug/pvisor --guest /tmp/vm-guest --output target/vm-cli-evidence
# Windows uses the same driver with --firmware C:/firmware/libkrunfw.dll.
# --output must be new; --rounds 30 expands coverage to 120 actual boots.
```

确定性在此指固定输入下的退出状态、文件结果、只读边界和清理结果一致，不承诺相同
调度/耗时。这是有界正确性负载，不是长时间稳定性或生产密度认证。CI 配置本身也
不是通过记录，必须以对应提交的真实运行结果为准。代码审查和待补覆盖见
[平台审查记录](https://github.com/DeepLink-org/pvisor/blob/main/tests/PLATFORM_REVIEW.md)。

### 已有运行结果的范围

[PR #92 的首轮 CI](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)
对应提交 `5b320104`：Windows MSVC 原生构建/契约、WHP API smoke 和共同 CLI
负载均通过；Linux/KVM 的共同 CLI 负载通过。macOS 的四程序产品构建和独立 CLI
测试通过，但共同 VM 负载在 HVF `VmCreate` 阶段失败，尚未执行 guest；它不能被
记录为三平台全部通过。本机 Apple Silicon 的 12 次加 120 次 HVF 启动通过，只是
另一台宿主的证据，不能替代该 runner。该轮 macOS core shard 另有 replay 测试
超时，主 CI 不能据此视为绿色。以上是该提交的结果快照，后续提交需重新检查。

## 单机 daemon {#daemon}

Sandbox API 要求 Linux x86_64/KVM 和委派 cgroup v2；macOS HVF 与 Linux arm64 不在该 API 的支持范围。`prepare-image` 发布共享压缩镜像池中的模板，新模板默认用 Rust guest 引擎；OpenSandbox/E2B 官方 SDK 的核心流程已有真实 VM 验证，完整协议与生产密度不作保证。部署步骤见[daemon 指南](../guides/daemon/index.md)。

同一个可执行文件的缓存和内存池组件支持 Linux 与 Apple Silicon macOS，不需要开启 sandbox API。macOS 可用压缩池或显式 `physical-pages` 物理共享；它们的前提和故障边界见[内存优化](../design/memory-optimization/index.md)。

## 能力证据怎么验证

- 文件读写分别检查 `safety.filesystem_read_non_bypassable` 与 `filesystem_write_non_bypassable`，不要只看是否 staged。
- 网络检查 `safety.network_non_bypassable` 与 driver；Linux host 选择性代理、container host 网络不能因 executor 名称被当成强制边界。
- container 目前不提供 safe 所需的完整控制，`--safe` 会拒绝；所有当前执行器在 `--strict` 下因 Subprocess 缺口拒绝。
- 资源查看 `resources.effective` 与 `limitations`；macOS 不强制 RLIMIT_AS，普通 host rlimit 不是整棵进程树的总额度。

验证入口：`.github/workflows/ci.yml`、`docs/src/zh/cases/06-stage-apply.md`、`crates/pvisor/tests/` 与 `tests/test_vm_*`。尚未验证的矩阵格不作成熟度结论；安全范围见[执行器边界](../security/executor-boundaries.md)。
