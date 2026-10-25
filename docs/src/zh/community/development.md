# 工程说明


从仓库根目录运行命令。`mise tasks ls` 展示公开任务；`mise run ci` 执行默认本地检查序列，`mise run ci -- check "test pvisor-core"` 顺序执行指定任务，失败即停止。内部任务仍可调用，但不在默认帮助中展示。

安装 [mise](https://mise.jdx.dev/installing-mise.html) 2026.8.5 或更新版本后，准备开发环境：

```bash
mise trust
mise install --locked
mise run setup
mise run doctor
```

`mise.toml` 固定 Rust（含 rustfmt、Clippy）、Python 3.12、uv、Ruff、actionlint、jq、Zig、nextest 和 cargo-zigbuild 的版本。`setup` 安装当前架构的 Linux musl guest target，并通过 `uv.lock` 同步 `.venv` 的开发依赖；不安装 pVisor 本身，也不触发固件构建。Python 产品仍支持 3.10+，开发工具使用 3.12。

CI 和 wheel 构建使用 mise 2026.10.18、已提交的工具版本和锁文件。复用 `PATH` 中已有的兼容本地工具时，在被忽略的 `mise.local.toml` 的 `[settings]` 表中设置 `disable_tools = ["python", "uv", "jq"]`；这些工具由调用者自行提供。Python 开发任务要求 3.12 或更新版本，禁用 Python 管理后 `UV_PYTHON` 回退到 `python3`。其他 mise 托管版本可在该文件的 `[tools]` 表中选择，通过 `mise ls --current` 检查。正式发布和记录 benchmark 对比时，使用没有个人覆盖或禁用工具的干净 checkout。

`mise run` 自动使用项目工具和环境，不需要先激活 shell。直接运行 Cargo 或 Python 时，使用 `mise exec -- cargo …` / `mise exec -- python3 …`，或按 mise 的 shell 配置说明激活当前 shell。带任务参数时，先用一个 `--` 分隔 mise 与任务；Rust 测试中的第二个 `--` 分隔包名与 nextest 参数，例如 `mise run test -- core -- --offline`。

系统依赖仍由平台包管理器提供：Linux 的编译库、FUSE、KVM 和 cgroup 权限，以及 macOS 的 Xcode Command Line Tools、LLVM/GNU 工具和 macFUSE。参见下方构建环境与执行器指南。机器私有覆盖写在被忽略的 `mise.local.toml`；不在共享配置中提交个人路径或凭据。更新工具版本修改 `mise.toml`，使用 CI 的 mise 2026.10.18 运行 `mise lock --platform linux-x64,macos-arm64` 并提交 `mise.lock`；更新 Python 依赖后运行 `mise exec -- uv lock` 并提交 `uv.lock`。

## 仓库结构与代码归属

Cargo workspace 包含按产品职责划分的 13 个 crate，默认成员是 `pvisor-cli`。Python `pvisor/` 是可安装的版本标记，不是启动器或运行时实现。
wheel 将原生可执行脚本直接安装到环境的 bin 目录；旧 Python 启动器及其二进制覆盖方式已废弃。

| 目录 | 职责 |
|---|---|
| `crates/pvisor/` | 可嵌入运行时、Session/Attempt 编排、执行器、持久化 Job 服务、镜像准备和缓存机制 |
| `crates/pvisor-cli/` | 三个应用二进制、CLI 命令、终端前端和 replay/TUI 伴随程序分派 |
| `crates/pvisor-vm/` | 原生 VM 运行时、跨平台 API、私有 VMM／平台实现、内嵌 guest 和内核／固件接入 |
| `crates/pvisor-daemon/` | Linux x86_64 sandbox API、原生 guest 协议适配，以及跨平台缓存/池组件 |
| `crates/pvisor-core/` | Operation、Placement、策略、对外交互和 Event 契约 |
| `crates/pvisor-journal/` | 共享事实 Journal 存储与读取 |
| `crates/pvisor-gateway/` | Agent 协议转发、转换、采集与投影 |
| `crates/pvisor-overlay-core/` | 不依赖 FUSE 的 OverlayFS 操作和文件访问控制 |
| `crates/pvisor-overlayfs/` | FUSE 适配及挂载 |
| `crates/pvisor-overlaynet/` | 出站策略、HTTP 代理和 VM virtio-net 数据通路 |
| `crates/pvisor-guest/` | Linux PID 1、启动契约与原生进程/文件控制引擎 |
| `crates/pvisor-shim/` | containerd Runtime v2 shim，可选 VM 执行 |
| `crates/pvisor-replay/` | 回放规划、原生 Agent 适配器和续跑协议桥 |
| `pvisor/`、`setup.py`、`scripts/packaging/` | Python 版本标记和原生脚本 wheel 打包 |
| `crates/*/tests/` | Rust 集成测试；单元测试跟随所属模块 |
| `tests/` | Python 打包和仓库工作流测试 |
| `examples/`、`benchmark/` | 可运行的产品场景和性能测量 |
| `scripts/ci/` | CI 检查及冒烟测试入口 |
| `docs/src/zh/`、`docs/src/en/` | 文档源；`docs/site/` 是生成产物 |
| `vendor/` | 有补丁的第三方依赖；产品编排逻辑放在 `crates/` |

workspace 内的直接普通依赖关系（包含目标平台限定的边；以下名称省略 `pvisor-` 前缀）：

```text
cli ──> pvisor, core, journal, replay, overlaynet, overlay-core, vm
pvisor ──> core, journal, overlaynet, overlayfs, overlay-core, guest, vm
cli --features gateway ──> gateway, pvisor/gateway
pvisor --features gateway ──> gateway
vm ──> overlay-core, guest
daemon ──> pvisor, core, guest
replay ──> core, journal
shim ──> guest, overlay-core
shim --features vm ──> vm, overlaynet, core
gateway ──> core, journal, overlaynet
overlaynet ──> core
overlayfs ──> core, overlay-core
overlay-core ──> core, journal
journal ──> core
core, guest ──> 不依赖其他 workspace crate
```

`pvisor` 没有 CLI 或 Clap 普通依赖；Clap 仅作为示例使用的开发依赖。
`pvisor-replay` 引擎没有对 `pvisor` 或 Clap 的普通依赖。
`pvisor-tui` crate 已移除，其可执行文件名称不变。

### pVisor 源码模块

| 实现入口 | 所有权 |
| --- | --- |
| `pvisor-cli/src/cli/mod.rs` | 按选中命令构造参数解析与分派；root help 描述命令组 |
| `pvisor-cli/src/cli/host*.rs`、`run/` | 前端/owner 启动、终端、信号与 Job 命令适配 |
| `pvisor/src/runtime/job_service/` | 持久 Job 生命周期、workspace、fork、capture、视图与策略 |
| `pvisor/src/runtime/attempt_service.rs` | 同一进程内 live Attempt 的 status/pause/resume/termination，供 Job 与 daemon 复用 |
| `pvisor/src/session*`、`runtime/attempt.rs` | 一次 Attempt 的驱动准备、执行与收尾 |
| `pvisor/src/runtime/completion.rs` | Bundle 字节与终态结果绑定的完成提交及读取验证 |
| `pvisor/src/docker.rs`、`image/` | 逐容器 VM、镜像目录与共享压缩镜像池；普通 OCI 执行器仍是独立路径 |
| `pvisor-daemon/src/daemon/`、`runtime.rs` | API、准入、记录、supervisor 与 cgroup/VM 归属 |
| `pvisor-daemon/src/native/`、`prepared_image.rs` | OpenSandbox/E2B guest 协议适配与模板发布 |
| `pvisor-replay/src/adapter/`、`bridge/` | 轨迹规划、Agent 选择与原生续跑协议桥 |

CLI 拥有参数和展示；runtime 拥有执行与持久 Job；VM、文件系统和网络实现分别留在所属 crate。共享策略与 wire DTO 由 Core 定义，普通 Job 与 daemon 复用进程内控制，但保留各自鉴权、准入、存储与确认清理。

公开接口与迁移状态以[核心架构](../design/architecture.md#api-boundaries)为准。当前仅 VM、OverlayFS 与 Journal 采用唯一 `api` 入口；其他 crate 继续使用实际导出，不能按这张逻辑表假定 Rust 可见性或稳定性已统一。

### 核心实现边界

core 定义 Operation、Event 和共享策略；pvisor 实现准入、实际改写、Placement 和调度。
Session 统一拥有 Attempt 的资源与终态；执行器负责执行并返回观察，OverlayCore 负责文件应用与恢复。
AgentCtl 与审批 socket 的实际 I/O 留在 pvisor。完整职责见[核心架构](../design/architecture.md)，
字段和事件顺序见 [Operation 与 Event](../design/operations-events.md)。

## 核心减法预算

CI 先检查默认运行时和应用的依赖边界，再构建带捕获的分发包。`scripts/ci/check_core_budget.py`
拒绝 CLI、Gateway、replay、Clap、TUI 和终端依赖进入 `pvisor` 的普通依赖闭包。
默认 `pvisor-cli` 应用包含 replay 引擎和集成 TUI，Gateway 仍为可选。
脚本记录工具链、运行时／应用依赖数、运行时闭包源码行数、Core 公开声明数和应用二进制字节数。
依赖数、闭包源码行数与 Core 公开声明数对着已提交的基线（`scripts/ci/core-budget.json`）
做棘轮比较：任一指标超过基线即失败，CI 配置里不再手写绝对上限，杜绝数字与注释漂移。
有意扩大预算的改动在同一次提交里运行 `mise run update-core-budget` 重新生成基线，评审直接看到
精确差量；代码缩减后同样建议重新生成，让基线随现实收紧。预算及统计口径由脚本维护；
实测结果保存在 CI 报告中，比较时使用相同平台和工具链。

## 贡献者命令

| 命令 | 作用 |
|---|---|
| `mise run build` / `mise run build -- release` | 构建当前平台支持的全部 workspace 可执行文件；macOS 签署 Hypervisor entitlement |
| `mise run install-cli` | 将已签名的 release CLI 安装到 `CARGO_INSTALL_ROOT` 或 `~/.cargo` |
| `mise run wheel` / `mise run wheel -- debug` | 构建全新 wheel，通过安装验证后再放入 `dist/` |
| `mise run fw -- build` / `mise run fw -- test` | 构建固件或运行 bundle/ABI 回归 |
| `mise run doctor` / `mise run doctor -- test` | 工具与系统能力诊断；mise 会先安装缺失工具 |
| `mise run check` | 检查产品及其依赖能否通过编译检查 |
| `mise run fmt` / `mise run ci -- "fmt-rust --check" "fmt-py --check"` | 格式化 Rust/Python 源码，或仅检查格式 |
| `mise run lint` | 运行 Clippy、Python 和 workflow lint 检查 |
| `mise run test` | 通过 nextest 跑工作区 Rust 测试，再跑 Python 测试 |
| `mise run test -- core pvisor cli` | 测试指定 Rust 包：共享契约、运行时和应用 |
| `mise run test -- cli` / `mise run test -- pvisor-cli` | 可执行文件／前端测试；`mise run test -- pvisor` 选择运行时测试 |
| `mise run test -- pvisor-vm` | VM 所有者测试；macOS 在 nextest 前签署 Hypervisor entitlement |
| `mise run test-py -- -k packaging` | 在仓库及活动 benchmark 测试中筛选 unittest 检查 |
| `mise run test-py -- discover -s benchmark/support` | 用 unittest 单独运行 benchmark 工具测试；默认 Python 测试已包含这些检查 |
| `PVISOR_TEST_VM_BIN=target/release/pvisor mise run test-py -- tests/test_vm_terminal.py` | 启用真实 VM 的普通终端和 TUI 交互回归 |
| `PVISOR_TEST_ZCODE=1 mise run test-py -- tests/test_zcode_integration.py` | 显式运行需要 Linux rootless、FUSE3 和 zcode 的集成测试 |
| `mise run test-isolation` | 运行严格的 Linux rootless/FUSE 回归，不跳过缺失的用户命名空间能力 |
| `mise run smoke` | 构建 debug CLI 并检查主要命令入口 |
| `mise run examples` | 构建 release CLI 并运行全部示例；追加场景名可选择子集 |
| `mise run cases -- --case S-DOC-001,S-DOC-002` | 运行选定的文档场景 |
| `mise run benchmark` / `mise run benchmark -- nightly` | 运行进程与 Run Bundle 基准 |
| `mise run docs-build` | 构建双语文档并检查链接 |
| `mise run docs-serve` / `mise run docs-serve -- en` | Zensical 原生预览与自动刷新；中文端口 3000，英文端口 3001 |
| `mise run ci` | 检查格式、lint、测试并构建，不改写源码 |
| `mise run clean` | 清理构建产物，保留开发环境和本地 Run 记录 |

`mise run test` 和 `mise run test-rust` 支持 Cargo 包名，以及 `pvisor`、`cli`（`pvisor-cli`）、`core`、
`control`／`agentctl`（Core 的兼容别名）、`capture`（Gateway）、`shim`（`pvisor-shim`）这些简称。
带参数的 `mise run test` 只运行指定 Rust 包的测试。CI 分片使用 `mise run test-rust`，
不会额外触发 Python 测试。

仅使用运行时的 Rust 测试留在 `crates/pvisor/tests/`。
可执行文件／前端集成测试（包括混合运行时与命令测试）归 `crates/pvisor-cli/tests/`；
混合文件中的纯运行时用例仍保留在 `pvisor`。
原生 VM 和依赖环境的测试保留原有前置条件及跳过／ignore 门槛；编译检查不代表真实 guest 验证。

默认 unittest 收集 `tests/` 和 `benchmark/`；共享 Operation 和 Overlay 契约由 `pvisor-core` 的 Rust 测试验证。
benchmark 中依赖 `/proc` 和 Linux rootfs 工具的测试仅在 Linux 上运行。
VM 文件系统检查在 Linux guest 内运行，需要 root、Python 和 tar；
在仓库目录执行 `PVISOR_TEST_GUEST_FS_DIRS=/var/tmp:. mise run test-py -- tests/test_vm_filesystem.py`，
分别检查 guest 根文件系统与挂载工作区。未配置目录时跳过，显式启用后检查失败会报错。

需要指定 Rust 集成测试或过滤条件时，在 `--` 后传给 nextest，例如：
`mise run test-rust -- pvisor-gateway -- --test llm_fixtures`。
nextest 不运行 doctest；需要时使用 `cargo test --doc -p <package>`。

## CI 分工

| 工作流 | 触发条件与职责 |
|---|---|
| CI | 面向 `main` 和 `develop` 的 push/PR：格式、Clippy、actionlint、Python 测试、基准工具测试、Rust 测试、文档用例与示例 |
| Documentation | 文档变更：双语构建与链接检查；仅上游仓库的 `main` 部署 Pages |
| pVisor Benchmark | 运行时、构建或基准变更：与 PR 基线或前一提交比较并上传报告 |
| Nightly Build | 每日或在 `main` 手动触发：构建、校验双平台 wheel，更新 nightly release |
| Publish PyPI | 稳定版本 tag：检查版本、lockfile 和 main 祖先关系后构建发布；手动运行只构建校验 |

保留必需状态 `CI`：任一依赖失败、取消或跳过都会使其失败。Linux Rust 测试按
core、Gateway、pVisor 分片，macOS 对同一组包只跑一遍。独立 Linux 隔离任务
必须具备 user namespace 和 FUSE，不允许跳过隔离检查。文件系统示例与文档用例共用该任务的
release 构建和隔离环境。网络/Gateway 示例在单独任务运行。

共享 action 用 mise 安装与本地相同的工具和 Python 开发依赖。`native-firmware` 输入启用 macOS 原生固件工具准备，`static-musl` 启用 Linux 系统编译库；工具版本统一来自 `mise.toml`。双平台 wheel 矩阵集中在一个可复用工作流中，manylinux 容器也安装 mise 并读取同一配置。PR 文档构建不会取消 Pages 部署。

## VM guest 启动

`pvisor-guest` 同时提供共享的 `GuestConfig` 库和 `pvisor-guest` 可执行文件。
构建 `pvisor-vm` 的 `init-blob` feature 时，`crates/pvisor-vm/build.rs` 使用 Rust 自带
`rust-lld`，按 VM 架构把 guest 编译成 release Linux musl ELF。
独立的 `target/pvisor-guest/` 目录避免与外层 Cargo 构建争抢产物锁。
libkrun 内嵌该 ELF，暴露为 `/init.krun`，由它担任 guest PID 1。

CLI 和 shim 注入 `/.pvisor-guest.json`，传递 argv、环境变量、cwd、工作区挂载、
资源限制、可选网络配置和 shim agent 参数。supervisor 初始化 guest 文件系统和
控制台 I/O，挂载工作区、配置网络、直接启动工作负载并回收子进程。
退出时先通过 libkrun 私有的根文件系统 ioctl `0x7602` 上报工作负载退出码，
再 sync/reboot。非零退出码使 Attempt 失败；VM 正常关机但未上报退出码时按 125 失败。

启动性能实测及测量范围见
[Guest init comparison](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/README.md#guest-init-comparison-apple-silicon)。

## 打包与命名

Python 包、安装后的 CLI 和运行时 Rust crate 使用 `pvisor`；应用 crate 为 `pvisor-cli`，伴随 crate 使用 `pvisor-*`，
环境变量使用 `PVISOR_*`。wheel 文件名形如 `pvisor-<version>-py3-none-<platform>.whl`。

## 构建环境

仓库使用 `mise.toml` 固定的 Rust 工具链、默认 LLVM backend 和平台 linker。`mise run setup` 安装当前架构的 guest target；guest supervisor 使用 Rust 自带 linker 构建成静态 Linux musl ELF。macOS VM 构建不需要 Zig。

| 产物 | Linux | Apple Silicon macOS |
|---|---|---|
| 宿主 CLI | 静态 Linux musl ELF | 原生 Darwin 可执行文件，签署 HVF entitlement |
| 内嵌 guest | 静态 Linux musl ELF | 静态 Linux musl ELF |
| `pvisor-vm` | 单一 Rust 运行时 crate | 单一 Rust 运行时 crate |
| guest 内核 | 构建时内嵌 | 运行时加载 `libkrunfw.5.dylib` |

`CARGO_TARGET_DIR` 指定原生构建目录，构建、安装、smoke、示例和场景任务共用此位置。
wheel 使用全新的暂存目录进行验证，避免误把 `dist/` 中的旧包当作本次产物。
Linux CLI 全静态链接 musl 并内嵌 VM 内核。构建所需的 Zig、cargo-zigbuild 和 Linux musl target 由 mise 安装及 `setup` 准备。Linux wheel 保留 manylinux_2_28
标签以支持 glibc Python 安装器。

文档任务通过 uv 隔离环境使用与 CI 相同的锁定版 Zensical，无需单独维护文档虚拟环境。

发布流程见[发布 PolicyVisor](releasing.md)，运行时要求见[可复现示例](examples.md)。

私有运行时模块在 libkrun 1.19.3 基线上选择性回移植上游改进。来源提交、本地适配与验收限制见[上游同步记录](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/provenance/libkrun/UPSTREAM.md)；版本号不代表已完整升级到 1.19.6 或 2.0。

## VM API 边界

`pvisor_vm::api` 是运行时唯一的外部接口。它声明跨平台 struct 和 trait 方法签名，不包含条件编译或方法体。私有模块实现契约；调用方导入需要的 `VmConfiguration`、`VmRuntime`、`VmControl` 以及快照/RAM trait。平台服务由 `VmPlatform` 的 `RuntimeSupport` 提供。

CLI、shim、示例和 init 基准使用这套接口。寄存器/设备测试及硬件探针归属 `pvisor-vm` 内部。静态内核提取与打包由以下文件负责： `crates/pvisor-vm/build_kernel.rs`；既有构建环境变量兼容。固件数据 ABI 和操作系统 FFI 保持私有。历史 trace/receipt 标识和基准证据保留原名。

运行 `mise run test -- pvisor-vm`；macOS 使用既有 Hypervisor entitlement 签署其测试程序。真实 VM 测试需要宿主 HVF/KVM 权限；Linux 创建 VM 的测试需要 `/dev/kvm`。
