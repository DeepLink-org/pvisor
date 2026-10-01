# 工程说明

从仓库根目录运行命令。`just` 列出支持的任务，每种工作流保留一个入口。

## 仓库结构与代码归属

Cargo workspace 按产品职责划分。Python `pvisor/` 只负责启动随包分发的 Rust
二进制，不是另一套运行时实现。

| 目录 | 职责 |
|---|---|
| `crates/persisting-pvisor/` | CLI、运行编排、执行器、镜像准备和缓存服务 |
| `crates/persisting-control/` | 共享契约、策略、AgentCtl 消息、IR 和事件 schema |
| `crates/persisting-gateway/` | Agent 协议转发、转换、采集与投影 |
| `crates/persisting-overlay-core/` | 不依赖 FUSE 的 OverlayFS 操作和文件访问控制 |
| `crates/persisting-overlayfs/` | FUSE 适配及挂载 |
| `crates/persisting-overlaynet/` | 出站策略、HTTP 代理和 VM virtio-net 数据通路 |
| `crates/persisting-replay/` | 回放规划、原生 Agent 适配器和续跑协议桥 |
| `pvisor/`、`setup.py`、`scripts/packaging/` | Python 启动器和 wheel 打包 |
| `crates/*/tests/` | Rust 集成测试；单元测试跟随所属模块 |
| `tests/` | Python 打包和仓库工作流测试 |
| `examples/`、`benchmark/` | 可运行的产品场景和性能测量 |
| `scripts/ci/` | CI 检查及冒烟测试入口 |
| `docs/src/en/`、`docs/src/zh/` | 成对维护的文档；`docs/site/` 是生成产物 |
| `vendor/` | 有补丁的第三方依赖；产品编排逻辑放在 `crates/` |

workspace 内的实际依赖关系：

```text
pvisor ──> control, gateway, overlaynet, overlayfs, overlay-core, replay
gateway ──> control, overlaynet
overlaynet ──> control
overlayfs ──> control, overlay-core
overlay-core ──> control
control, replay ──> 不依赖其他 workspace crate
```

### pVisor 源码模块

```text
src/
├── lib.rs                 # 稳定的嵌入接口导出
├── bin/pvisor.rs          # 二进制入口
├── cli/                   # 参数、命令、Agent 预设和终端 UI
├── config.rs              # 运行时与执行器配置
├── trace.rs               # 共享事实 Journal 重导出
├── diagnostics.rs         # 共享宿主日志，前端选择输出位置
├── executor/
│   ├── mod.rs             # RunExecutor 和 AttemptContext
│   ├── process.rs         # 宿主进程执行器
│   ├── container.rs       # 容器执行器
│   ├── sandbox.rs         # 宿主 OS 隔离及内部 sandbox 入口
│   ├── artifact.rs        # 适配 guest 的可执行文件解析
│   ├── delegated.rs       # 委派执行的 spec/result 交接
│   └── vm/                # libkrun 执行器和固件获取
├── image/
│   ├── oci.rs             # Registry、准备记录、blob 和解包
│   └── cache/             # 缓存 CLI、协议、服务端、客户端及懒加载 FUSE
├── runtime/
│   ├── run.rs             # PVisor API 和运行生命周期
│   ├── agentctl.rs        # 每次运行的协作控制服务
│   ├── event.rs           # 运行事件发布
│   ├── bundle.rs          # 持久化审查摘要
│   ├── checkpoint.rs      # 逻辑检查点与恢复
│   ├── registry.rs        # Run 身份、租约和本地控制端点
│   ├── attempt.rs         # 每次尝试的驱动资源与清理
│   ├── supervisor.rs      # 能力检查与驱动协调
│   ├── plan.rs            # 类型化运行计划构造
│   ├── implant.rs         # 运行环境注入
│   ├── overlay.rs         # 暂存、审查、应用/丢弃和恢复
│   └── zcode.rs           # 进程兼容策略
└── util.rs                # 少量共享文件与时间工具
```

CLI 参数与展示留在 `cli/`，具体执行机制归 `executor/`，Run 资源所有权归
`runtime/`。固件属于 VM 执行器；OCI 准备属于 `image/`，供直接加载和缓存
服务共用。Bundle 和检查点与运行记录放在一起，不归某个执行后端。
`PVisor`、`ProcessExecutor`、`cache` 以及内部 `sandbox` 入口等根级导出保留
原有导入路径。

replay 中，`adapter/` 负责原生轨迹规划和 Agent 启动选择；`bridge/` 负责
Claude、Codex、OpenCode 协议桥及 Claude resume transport 校验。
共享执行和 journal 仍在 crate 根目录。

### 仍需逐步改善的边界

目录整理不代表 pVisor 内部已实现严格单向分层：`AttemptContext` 仍携带
运行时资源附件，运行时 Overlay 配置仍使用 Gateway 的配置类型。这些需要
修改契约，不能只靠移动文件解决。`cli/run.rs`、`runtime/overlay.rs` 和较大的
Agent 适配器仍包含多个阶段；后续修改相关行为时，应按生命周期或协议边界
拆分，而不是按行数切割。不要仅为缩短文件新增 crate；移动内部模块时保持
对外导出稳定，并运行受影响包的测试。


## 贡献者命令

| 命令 | 作用 |
|---|---|
| `just build` / `just build release` | 构建 debug/release CLI，并在 macOS 上签署 Hypervisor entitlement |
| `just install-cli` | 将已签名的 release CLI 安装到 `CARGO_INSTALL_ROOT` 或 `~/.cargo` |
| `just wheel` / `just wheel debug` | 构建全新 wheel，通过安装验证后再放入 `dist/` |
| `just check` | 检查产品及其依赖能否通过编译检查 |
| `just fmt` / `just fmt-check` | 格式化 Rust/Python 源码，或仅检查格式 |
| `just lint` | 运行 Clippy 和 Python 包 lint 检查 |
| `just test` | 通过 nextest 跑工作区 Rust 测试，再跑 Python 测试 |
| `just test control pvisor` | 测试指定 Rust 包，支持简称或 Cargo 包名 |
| `just test-py -k packaging` | 将选项传给 pytest |
| `just test-isolation` | 运行严格的 Linux rootless/FUSE 回归，不跳过缺失的用户命名空间能力 |
| `just smoke` | 构建 debug CLI 并检查主要命令入口 |
| `just examples` | 构建 release CLI 并运行全部示例；追加场景名可选择子集 |
| `just cases --case A01,A02` | 运行选定的文档场景 |
| `just benchmark` / `just benchmark nightly` | 运行进程与 Run Bundle 基准 |
| `just docs-build` | 构建双语文档并检查链接 |
| `just docs-serve --port 3000` | 构建、监听并预览文档；重建后手动刷新浏览器 |
| `just ci` | 检查格式、lint、测试并构建，不改写源码 |
| `just clean` | 清理构建产物，保留开发环境和本地 Run 记录 |

`just test` 和 `just test-rust` 支持 Cargo 包名，以及 `pvisor`、`control`、
`agentctl`（Control 的兼容别名）、`capture`（Gateway）这些简称。
带参数的 `just test` 只运行指定 Rust 包的测试。CI 分片使用 `just test-rust`，
不会额外触发 Python 测试。

需要指定 Rust 集成测试或过滤条件时，直接调用 nextest，例如：
`cargo nextest run --locked -p persisting-gateway --test llm_fixtures`。
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

共享 action 默认只安装 Python、uv 和 just；Rust、nextest 和 guest Rust target 按需启用。Linux 静态 CLI/shim 构建通过
`static-musl` 启用 Zig 和 cargo-zigbuild；Rust 检查和单元测试不需要这两个工具。
双平台 wheel 矩阵集中在一个可复用工作流中。PR 文档构建不会取消 Pages 部署。

## 构建环境

仓库使用 `rust-toolchain.toml` 中的 stable 工具链、默认 LLVM backend 和平台 linker。
请安装 nextest `0.9.137`，或使用仓库 CI setup action。guest supervisor 使用 Rust 自带 linker 构建成静态 Linux musl ELF；macOS VM 构建不再需要 Zig。
Apple Silicon 上首次构建前执行 `rustup target add aarch64-unknown-linux-musl`。
CI 仅安装当前架构的 guest target，工作区工具链不再为无关 crate 下载交叉编译 target。

`CARGO_TARGET_DIR` 指定原生构建目录，构建、安装、smoke、示例和场景任务共用此位置。
wheel 使用全新的暂存目录进行验证，避免误把 `dist/` 中的旧包当作本次产物。
Linux CLI 全静态链接 musl 并内嵌 VM 内核。构建需要 Zig、cargo-zigbuild 和
`rustup target add x86_64-unknown-linux-musl`。Linux wheel 保留 manylinux_2_28
标签以支持 glibc Python 安装器。

文档任务通过 uv 隔离环境使用与 CI 相同的锁定版 Zensical，不再要求单独维护文档虚拟环境。

发布流程见[发布 PolicyVisor](releasing.md)，运行时要求见[可复现示例](examples.md)。
