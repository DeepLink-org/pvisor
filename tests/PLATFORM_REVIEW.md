# 跨平台 VM 支持审查

Windows 的 `pvisor.exe` 已能构建并通过 WHP 的共同负载，以及下文限定的原生
容器网络/终端验收。完整 Session、TUI、replay 和 Daemon 仍需移植。托管 CI 现在只对 Linux/KVM
和 Windows/WHP 做共同 VM 验证；macOS 保留构建与常规测试，HVF 用例改为手动运行。

审查基线为 `a2f6bb81`，重点检查 Windows 后端 `482d7b25`（#84）和 CI 门禁
`705ba52d`（#85）的 CLI 依赖、VM API、WHP builder、virtio-fs、supervisor 与退出
协议。审查未穷尽四万余行 backport，也未覆盖最近全部文档和镜像分发改动。

## 发现与修复

### [P1，已修复] 目录重命名后，缓存子文件仍访问旧路径

`src/windows/devices/virtio/fs/windows/passthrough.rs::rename_inode_path` 原来
只更新目录自身。先 `lookup before/nested/file`，再把 `before → after`，已有
child inode 的 getattr/open 会访问旧路径并可能报 ENOENT。

修复同步更新缓存子树，保留 inode 和 lookup count；同路径 rename 保持缓存。
原生回归检查嵌套缓存 inode，共同 guest 负载检查实际目录移动。原先只重命名单个
文件的 smoke 无法触发这个问题。

### [P1，已补入口与门禁] Windows 构建未覆盖产品 CLI

`pvisor-cli` 无条件依赖 Unix runtime、OverlayFS 和 replay；原 WHP smoke 直接
调用 Rust API。本次增加 `pvisor vm-run`，共用公共 VM API 和 GuestConfig，
Windows 构建排除 Unix 运行时依赖。该轮仅覆盖 `vm-run`；后续容器移植和验收
见下文，`spawn` 仍不支持。

Linux/macOS 安装包含 `pvisor`、`pvisor-tui`、`pvisor-replay`、`pvisor-daemon`。
Windows 只构建主程序和测试探针。`pvisor-guest` 是 Linux payload，固件 DLL 是
启动依赖。功能与组件的对应关系见[二进制组成](../docs/src/zh/reference/platforms.md#binaries)。

### [P2，已补覆盖] WHP 缺少并发文件负载

原 `whp_guest` 只有一次读取和写回，WHP 验收共五个场景。新增驱动默认启动
12 次 VM，每次运行 8×32 次文件操作，并检查内存触碰、只读拒写、计时器、SMP
和退出码。宿主还检查结果文件，避免 CLI 返回 0 而 guest 没有执行的假阳性。
超时会失败并保留 stdout/stderr、命令、哈希和 rootfs。原 pause/resume、ready
回调失败清理及退出防伪 smoke 保留。

## 验证结果

[首轮 CI](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)对应提交
`5b320104`。后续提交需重新检查。

| 环境 | 结果 |
| --- | --- |
| Windows 原生 CI | MSVC 构建/契约、WHP API smoke、共同 CLI 负载通过 |
| Linux/KVM CI | 共同 CLI 负载通过 |
| macOS CI | 四程序构建和独立 CLI 测试通过；共同负载在 guest 启动前报 `build hvf aarch64 VM: Internal(Vm(VmSetup(VmCreate)))`；core shard 的 `opencode_continuation_preserves_failure_and_step_budget` 另有超时 |
| 本机 Apple Silicon | HVF 默认 12/12 次启动及追加 30 轮、120/120 次启动通过；129 项 CLI、14 项 VM API/boundary、4 项 Python 驱动回归通过 |
| Windows 交叉检查 | x86_64 MSVC `cargo check` 通过，已用固定 1.98.0 工具链复核；另用 stable 检查 VM 原生测试的编译 |
| 文档与格式 | 严格双语构建通过，134 对文章、270 个 HTML 页面；workflow lint、格式检查通过 |
| 本机 macOS lint | `cargo clippy ... -D warnings` 被 `pvisor-overlay-core/src/apply.rs:1698–1699` 的既有未使用变量阻断，该文件未改动 |

本机 HVF 通过和 Windows 交叉检查分别说明本机运行与目标编译的结果；macOS
runner 的 VM 创建失败原因仍待确认。该轮主 CI 未全部通过，用例未放宽或跳过。

### Win11 / VirtualBox 本机复测（2026-11-14）

源码基线为 `76cb554f`，使用归档快照在 Win11 Enterprise Evaluation
`10.0.26200` 中原生构建。外层为 Fedora / VirtualBox `7.2.22`，AMD Ryzen 7
9700X；Windows 分配 4 vCPU、8 GiB。构建工具为 VS 2022 Build Tools 17.14、
Rust 1.98.0、mise 2026.10.18、nextest 0.9.137、Python 3.12.12 和 uv 0.11.19。

干净环境首先暴露两类安装问题：

- 缺少 `VCRUNTIME140.dll` 时，CI 的 MSVC CLI 和 mise 都在启动前以
  `0xC0000135` 退出。安装 C++ Build Tools 带入运行库后，help/version 正常。
- 为 Windows 选定的 Rust、nextest、Python 和 uv 执行 `mise install --locked`
  时，缺少 Python/uv 的 `windows-x64` 下载记录；补齐后安装通过。随后
  `test-py` 使用不存在的 Unix `bin/python3` 路径，已改为
  Windows 安装根目录的 `python.exe`，保留 Unix 和未安装工具时的原有行为。
  Windows CI 增加这两个锁定工具和 VM 驱动回归，覆盖实际 Python 任务入口。

| 本机命令 | 结果 |
| --- | --- |
| `mise exec -- cargo build --locked -p pvisor-vm --lib --no-default-features` | 通过 |
| `mise exec -- cargo build --locked -p pvisor-vm --lib --example whp_smoke` | 通过 |
| `mise exec -- cargo build --locked -p pvisor-cli --bin pvisor` | 通过，源码构建 CLI 的 version 正常 |
| `mise run test -- pvisor-cli -- --lib --bin pvisor` | 2/2 通过，0 skipped |
| `mise run test -- pvisor-core pvisor-guest pvisor-vm -- --lib --tests --no-fail-fast` | 309/309 通过，0 skipped |
| `mise run test -- pvisor-vm -- --no-default-features --lib --tests --no-fail-fast` | 209/209 通过，0 skipped；与默认配置有重叠用例 |
| `RUSTDOCFLAGS='-D warnings' mise exec -- cargo doc --locked -p pvisor-vm --no-deps` | 通过 |
| `mise run test-py -- tests/test_vm_cli_contract.py` | 5/5 通过，含 Windows 超时进程树回收 |

真实 guest 启动未通过。启用 WHP 并重启后，`WHvGetCapability` 仍返回
`HypervisorPresent=0`，`WHvCreatePartition` 返回 `0xC0351000`。Windows System
日志的 Hyper-V-Hypervisor 事件 59 明确报告缺少 SLAT；boot loader 已是
`hypervisorlaunchtype Auto`。外层 VM 的 `nested-hw-virt`、`nestedpaging` 均为
on，但 VirtualBox 日志显示 guest `NP=0`、宿主 `NP=1`。

共同 CLI 驱动使用本机源码构建的 `pvisor.exe`，guest 和固件来自同基线的
[CI 制品](https://github.com/DeepLink-org/pvisor/actions/runs/43332712130)，以
`--rounds 1` 尝试。首个 1 CPU / exit 0 场景即以宿主退出码 1 和
`WHP boot: Internal(Vm(HypervisorNotPresent))` 失败，guest 未启动，后续场景
未执行。CI 的 `whp_smoke` 也在 ready 回调前失败。没有放宽断言或将环境失败
标为 skip；这些原生测试结果不构成该宿主的 WHP 验收。

安装和命令状态记录保留于 Windows `%USERPROFILE%\pvisor-debug\logs`，
共同驱动的命令、哈希和失败 rootfs 保留于
`%USERPROFILE%\pvisor-debug\evidence\vm-cli-virtualbox-20261114`。
该次复测未转换原虚拟磁盘；后续独立 QEMU 副本的验证见下一节。

### QEMU/KVM 嵌套 WHP 复测（2026-11-14）

同一台 Fedora / Ryzen 7 9700X 宿主改用 QEMU 11.1.3、libvirt 12.6.0、
Linux 7.2.15。先正常关闭 VirtualBox guest，再将 VDI 转换为独立 qcow2；
`qemu-img check` 无错误，启动前 `qemu-img compare` 确认磁盘字节相同。
原 VirtualBox VM 保留并保持关机，QEMU 只挂载副本，无原盘 backing 依赖。

新域为 `qemu:///session` 下的 `pvisor-win11-qemu`，4 vCPU、8 GiB、
Q35/UEFI、SATA、TPM 2.0，CPU 使用 host-passthrough 并要求 SVM/NPT。
KVM 的 `nested=1`、`npt=Y`，QMP 确认 KVM enabled。用户态 passt 网络只将
SSH 转发到宿主 `127.0.0.1:2222`，VNC 绑定 `127.0.0.1:5901`，未修改宿主
桥接或防火墙。域为持久配置，未启用自动启动；磁盘和 XML 副本位于宿主
`~/VirtualMachines/pvisor-win11-qemu`。

克隆保留 Windows 的 `Pacific Standard Time`，最初 RTC 按宿主中国时区初始化，
导致 guest UTC 快 16 小时。重复负载结束后将持久 RTC 改为
`offset=timezone`、`America/Los_Angeles`，正常关机并重新启动；guest UTC 与
宿主相差小于 5 秒，WHP 仍可用。初始和 120 次重复负载在校时前运行，
冷启动后的复验在校时后运行；驱动的超时和计时器 oracle 未改动。

Windows 仍为 `10.0.26200`。此次 System 事件 1 报告
`Hypervisor successfully started`，`WHvGetCapability` 返回
`HypervisorPresent=1`，WHP 分区创建和删除均返回 `0x00000000`。

继续使用上述源码构建的 CLI 和同基线 CI 的 guest/固件，未修改 VM 实现或
测试断言。CLI SHA-256 为
`73cd6dce3c77cd50a0cf324b1fd8d0ae7f9b7f7fd2be6b36efb5db4e2d252c2e`，
与 VirtualBox 失败记录相同。

| QEMU 副本中的检查 | 结果 |
| --- | --- |
| `whp_smoke.exe`，使用同基线的固件和 `whp-guest` | 5/5 通过：ready 拒绝清理、暂停/继续、单/双核、文件/只读共享、退出码 0/7/255 |
| `tests/vm_cli_contract.py` 默认参数 | 3 轮，12/12 次真实启动通过；guest 退出码和宿主文件 oracle 均通过 |
| 同驱动追加 `--rounds 30 --seed 20261114` | 120/120 次真实启动通过，保持默认 90 秒单次超时 |
| 修正 RTC 后正常关机、冷启动并复跑 smoke 和默认 CLI 驱动 | smoke 5/5、CLI 12/12 通过 |
| 上节三组 `mise run test` 命令 | 2/2、309/309、209/209 通过，均为 0 skipped |
| `mise run test-py -- tests/test_vm_cli_contract.py` | 5/5 通过 |
| warnings-denied `cargo doc`，命令同上 | 通过 |

三批共同 CLI 负载合计 144/144 次启动通过。Windows 证据分别位于
`%USERPROFILE%\pvisor-debug\evidence` 下的 `vm-cli-qemu-default`、
`vm-cli-qemu-repeat-120`、`vm-cli-qemu-after-coldboot`，包含逐场景
stdout/stderr、rootfs 和带输入哈希的 `report.json`。宿主
`~/VirtualMachines/pvisor-win11-qemu/logs` 保留 smoke、原生测试、冷启动复验
完整输出、校时后环境记录及三份共同负载报告副本。原生测试和短期 guest 验证
不代表完整 Session、TUI、replay、Daemon、小时级稳定性或生产密度已验收。

### 原生容器默认网络与终端验收（2026-11-14）

在同一 Win11 / QEMU/KVM 嵌套 WHP 环境原生构建当前工作区，使用真实 Alpine
镜像和匹配的固件、ext4 模板。QEMU 只提供测试用的 Windows 宿主；容器由
Windows 上的 `pvisor.exe` 通过 WHP 执行，不依赖 WSL 或 Docker。

测试发现并修复两项实现问题：省略 `-w` 时启动预检使用空路径；ext4 镜像未安装
指向虚拟网关的 resolver。增加默认 cwd 与 resolver 创建、刷新、符号链接替换的
回归。另修复测试夹具的 Winsock accept 继承非阻塞模式、SSH 父进程重定向
stdio 被 ConPTY 子进程继承的问题。离线网络检查允许内核自带的虚拟 `dummy0`，
仍要求无附加设备和默认路由；peer TCP 使用标准 Alpine 自带的 `nc`，不依赖
未安装的 `httpd` applet。失败记录在清理实例前保留状态和 workload 日志。

| 原生命令或检查 | 结果 |
| --- | --- |
| `cargo build --locked -p pvisor-cli --bin pvisor`，通过 `mise exec` | 通过 |
| `mise run test -- pvisor-cli -- --lib --bin pvisor` | 8/8 通过，0 skipped |
| `mise run test -- pvisor-containers pvisor-guest -- --lib --tests` | 32/32 通过，0 skipped |
| `mise run test -- pvisor-overlaynet -- --lib` | 74/74 通过，0 skipped |
| VM 的 `windows_contract` 与 named-pipe 单测（`--features net,blk`，通过 `mise run test`） | 所选 11/11 通过，205 项未运行 |
| `python tests/windows_container_contract.py --binary target/debug/pvisor.exe --image alpine --output ../evidence/windows-network-terminal-6`，通过 `mise exec` | 77/77 次 CLI 调用通过 |
| 同驱动重复执行，输出到 `windows-network-terminal-7` | 再次 77/77 通过，并核对两个 ConPTY 子命令返回后的控制台模式和代码页恢复 |
| 结束后查询 Windows `Win32_Process` | 无遗留 `pvisor.exe` 进程 |

两轮验收合计 154 次 CLI 调用，覆盖默认 bridge 的 DNS/IPv4 TCP 出网、
同 store 两个 guest 的真实 TCP 回显、显式 `none`、二进制 stdin/EOF、
主进程提前退出时取消空闲输入，以及 `run -it`/`exec -it` 的 37×91 初始尺寸、
中文输入、Ctrl+C 和退出码 7。原有镜像、别名、ps、日志分流/跟随、并发 exec、
stop 信号/无限等待、强制删除和保留磁盘重启检查也全部通过。

两轮使用相同的 CLI SHA-256：
`6eb94cb0f105cedf822d46a20219b29ba13c05fab7baec286014e0a92d8c58c9`。
Windows 的 `%USERPROFILE%\pvisor-debug\evidence\windows-network-terminal-6`
和 `windows-network-terminal-7` 保留逐命令 stdout/stderr、ConPTY 原始输出和
`report.json`。归档 `windows-network-terminal-evidence.zip` 已复制到开发工作区
`target/`，校验 SHA-256 为
`d5b604ff4fe4b6534bbe757e8467b1445e1e9edec37ee4c54b4ca1e50029e40e`。
测试结束后使用 Windows 正常关机，确认 QEMU 状态为 `shut off`，未强制断电。

Linux 定向 `mise run test` 回归 860/860 通过（388 项未运行），VM/containers
warnings-denied rustdoc 与格式检查通过。以上是本机验收，未触发远端 CI，
不覆盖实时终端缩放、guest UDP/ICMP 互联、端口发布、挂载、自定义网络、
小时级泄漏或完整 Session/companion 工作流。

## 未覆盖的行为

尚未验证小时级稳定性、持续启停后的 handle/线程/内存泄漏、持续并发多个 VM、
极低内存或 64 CPU、休眠恢复及宿主时钟跳变。Windows 的大小写冲突、长路径、
ACL/reparse point、mmap、SQLite、锁、打开后删除和文件替换还需独立用例。
通用 UDP/ICMP 出网、IPv6、端口发布、快照、stage/apply 和模型调用不在本轮容器验收范围内。

测试固定输入和最终结果，允许线程调度变化。共享树仍要求调用方管理并防止
不可信宿主改写，新增 CLI 没有加强这项边界。后续优先补持续启停的资源基线和
上述文件语义测试；平台实现及验收条件见平台文档。
