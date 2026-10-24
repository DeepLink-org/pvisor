# 工程说明

> 旧 `benchmark/` 运行器已退役。下文旧路径与命令仅用于历史批次溯源；当前入口见 [benchmark](https://github.com/DeepLink-org/pvisor/tree/main/benchmark)。

从仓库根目录运行命令。`mise tasks ls` 展示公开任务；`mise run ci` 执行默认本地检查序列，`mise run ci -- check "test pvisor-core"` 顺序执行指定任务，失败即停止。内部任务仍可调用，但不在默认帮助中展示。

安装 [mise](https://mise.jdx.dev/installing-mise.html) 2026.10.18 或更新版本后，准备开发环境：

```bash
mise trust
mise install --locked
mise run setup
mise run doctor
```

`mise.toml` 固定 Rust（含 rustfmt、Clippy）、Python 3.12、uv、Ruff、actionlint、jq、Zig、nextest 和 cargo-zigbuild 的版本。`setup` 安装当前架构的 Linux musl guest target，并通过 `uv.lock` 同步 `.venv` 的开发依赖；不安装 pVisor 本身，也不触发固件构建。Python 产品仍支持 3.10+，开发工具使用 3.12。

`mise run` 自动使用项目工具和环境，不需要先激活 shell。直接运行 Cargo 或 Python 时，使用 `mise exec -- cargo …` / `mise exec -- python3 …`，或按 mise 的 shell 配置说明激活当前 shell。带任务参数时，先用一个 `--` 分隔 mise 与任务；Rust 测试中的第二个 `--` 分隔包名与 nextest 参数，例如 `mise run test -- core -- --offline`。

系统依赖仍由平台包管理器提供：Linux 的编译库、FUSE、KVM 和 cgroup 权限，以及 macOS 的 Xcode Command Line Tools、LLVM/GNU 工具和 macFUSE。参见下方构建环境与执行器指南。机器私有覆盖写在被忽略的 `mise.local.toml`；不在共享配置中提交个人路径或凭据。更新工具版本修改 `mise.toml`，运行 `mise lock --platform linux-x64,macos-arm64` 并提交 `mise.lock`；更新 Python 依赖后运行 `mise exec -- uv lock` 并提交 `uv.lock`。

## 仓库结构与代码归属

Cargo workspace 包含按产品职责划分的 13 个 crate，默认成员是 `pvisor-cli`。Python `pvisor/` 是可安装的版本标记，不是启动器或运行时实现。
wheel 将原生可执行脚本直接安装到环境的 bin 目录；旧 Python 启动器及其二进制覆盖方式已废弃。

| 目录 | 职责 |
|---|---|
| `crates/pvisor/` | 可嵌入运行时、Session/Attempt 编排、执行器、持久化 Job 服务、镜像准备和缓存机制 |
| `crates/pvisor-cli/` | 三个应用二进制、CLI 命令、终端前端和 replay/TUI 伴随程序分派 |
| `crates/pvisor-vm/` | 原生 VM 运行时、跨平台 API、私有 VMM／平台实现、内嵌 guest 和内核／固件接入 |
| `crates/pvisor-daemon/` | Linux x86_64 sandbox 生命周期 API、独立原生 VM supervisor 和可选 daemon 自有池 |
| `crates/pvisor-core/` | Operation、Placement、策略、对外交互和 Event 契约 |
| `crates/pvisor-journal/` | 共享事实 Journal 存储与读取 |
| `crates/pvisor-gateway/` | Agent 协议转发、转换、采集与投影 |
| `crates/pvisor-overlay-core/` | 不依赖 FUSE 的 OverlayFS 操作和文件访问控制 |
| `crates/pvisor-overlayfs/` | FUSE 适配及挂载 |
| `crates/pvisor-overlaynet/` | 出站策略、HTTP 代理和 VM virtio-net 数据通路 |
| `crates/pvisor-guest/` | Linux PID 1 supervisor，以及 VM 执行器共用的启动契约 |
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
vm ──> overlay-core
daemon ──> pvisor, core
replay ──> core, journal
shim ──> guest, overlay-core
shim --features vm ──> vm
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

```text
crates/pvisor-cli/src/
├── lib.rs                 # 前端模块，不重导出运行时
├── bin/                   # 三个：pvisor、pvisor-tui、pvisor-replay
├── cli/                   # 参数、命令和共享终端工具
│   ├── cache.rs           # 缓存参数解析与展示
│   └── features.rs        # 运行时功能列表前端
├── companions.rs          # 同一安装中的伴随程序查找／分派

└── tui/                   # TUI PTY 运行时、渲染、审查面板和按键映射

crates/pvisor/src/
├── lib.rs                 # 运行时导出与显式前端／嵌入 API
├── session/               # Attempt 生命周期与收尾
├── session.rs             # Session 所有者
├── config.rs              # 运行时与执行器配置
├── trace.rs               # 共享事实 Journal 重导出
├── diagnostics.rs         # 共享宿主日志，前端选择输出位置
├── executor/
│   ├── mod.rs             # RunExecutor 和执行输出契约
│   ├── process.rs         # 宿主进程执行器
│   ├── container.rs       # 容器执行器
│   ├── sandbox.rs         # 宿主 OS 隔离及内部 sandbox 入口
│   ├── artifact.rs        # 适配 guest 的可执行文件解析
│   ├── delegated.rs       # 委派执行的 spec/result 交接
│   └── vm/                # VM 执行器适配与 Run 资源／控制接入
├── image/
│   ├── oci.rs             # Registry、准备记录、blob 和解包
│   └── cache/             # 缓存协议、服务端、客户端及懒加载 FUSE
├── runtime/
│   ├── run.rs             # PVisor API 和运行生命周期
│   ├── job_service.rs     # 持久化 RuntimeJobService
│   ├── job_execution.rs   # Job 执行机制
│   ├── host_transport.rs  # 类型化 Host 传输
│   ├── instance_control.rs # 本地实例控制交互
│   ├── agentctl.rs        # 每次运行的协作控制服务
│   ├── agentctl_client.rs # 同步 AgentCtl 客户端
│   ├── audit.rs           # 审批 socket 传输与缓存
│   ├── event.rs           # 运行事件发布
│   ├── bundle.rs          # 持久化审查摘要
│   ├── checkpoint.rs      # 逻辑检查点与恢复
│   ├── registry.rs        # Run 身份、存活锁和本地控制端点
│   ├── attempt.rs         # 每次尝试的驱动资源与清理
│   ├── supervisor.rs      # 能力检查与驱动协调
│   ├── operation.rs       # 操作与观察构造
│   ├── implant.rs         # 运行环境注入
│   ├── overlay.rs         # 暂存、审查、应用/丢弃和恢复
│   └── zcode.rs           # 进程兼容策略
└── util.rs                # 少量共享文件与时间工具
```

CLI 参数与展示、前端/owner 启动和终端交接归 `pvisor-cli`；
具体执行机制归 `pvisor` 的 `executor/`，Run 资源所有权和持久化 Job 服务归 `runtime/`。
VM 执行器将 Run/Attempt 生命周期适配到 `pvisor_vm::api`；VMM、平台机制、内嵌 guest
和内核／固件接入属于 `pvisor-vm`。OCI 准备属于 `image/`，供直接加载和缓存服务共用。
缓存存储及带认证的服务端留在运行时，缓存命令解析／展示归 `pvisor-cli/src/cli/cache.rs`。
Bundle 和检查点与运行记录放在一起，不归某个执行后端。`pvisor-daemon/src/memory_pool.rs` 拥有池启动／复用和独立 `memory-pool` 组件；通过 `serve --sandbox --memory-pool` 启用。`pvisor/src/node.rs` 与 `node/` 中的 node 协议是运行时设施，没有 daemon acquire/release 适配器。`pvisor cache` 负责准备、发布和读取；`pvisor-daemon serve --cache` 负责服务。

既有公开运行时导入，包括 `PVisor`、`ProcessExecutor`、`cache` 以及内部 `sandbox` 入口，
保留原有路径。显式前端／嵌入 API 导出文件访问类型、`GatewayProfile`、
`DelegatedRunOutput`、`rootless_runtime_available`、Overlay 选择／检查及 Run 查找／控制工具、
Linux Run 租约、`audit`、`checkpoint`、`job_execution` 和启动标记／私有 JSON 工具。
运行时实现模块仍保持私有；这些导出不构成 API 稳定性承诺。

replay 中，`adapter/` 负责原生轨迹规划和 Agent 启动选择；`bridge/` 负责
Claude、Codex、OpenCode 协议桥及 Claude resume transport 校验。
共享执行和 journal 仍在 crate 根目录。

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
| `mise run build` / `mise run build -- release` | 构建 debug/release CLI，并在 macOS 上签署 Hypervisor entitlement |
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
19 个可执行文件／前端集成测试文件（包括混合运行时与命令测试）现归 `crates/pvisor-cli/tests/`；
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
