# 工程说明

从仓库根目录运行命令。`just` 列出支持的任务，每种工作流保留一个入口。

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
| CI | 面向 `main` 的 push/PR：格式、Clippy、actionlint、Python 测试、基准工具测试、Rust 测试、文档用例与示例 |
| Documentation | 文档变更：双语构建与链接检查；仅上游仓库的 `main` 部署 Pages |
| pVisor Benchmark | 运行时、构建或基准变更：与 PR 基线或前一提交比较并上传报告 |
| Nightly Build | 每日或在 `main` 手动触发：构建、校验双平台 wheel，更新 nightly release |
| Publish PyPI | 稳定版本 tag：检查版本、lockfile 和 main 祖先关系后构建发布；手动运行只构建校验 |

保留必需状态 `CI`：任一依赖失败、取消或跳过都会使其失败。Linux Rust 测试按
core、Gateway、pVisor 分片，macOS 对同一组包只跑一遍。独立 Linux 隔离任务
必须具备 user namespace 和 FUSE，不允许跳过隔离检查。文件系统示例与文档用例共用该任务的
release 构建和隔离环境。网络/Gateway 示例在单独任务运行。

共享 action 默认只安装 Python、uv 和 just；Rust、nextest、macOS Zig 按需启用。
双平台 wheel 矩阵集中在一个可复用工作流中。PR 文档构建不会取消 Pages 部署。

## 构建环境

仓库使用 `rust-toolchain.toml` 中的 stable 工具链、默认 LLVM backend 和平台 linker。
请安装 nextest `0.9.137`，或使用仓库 CI setup action。macOS VM 构建还需要 Zig。

`CARGO_TARGET_DIR` 指定原生构建目录，构建、安装、smoke、示例和场景任务共用此位置。
wheel 使用全新的暂存目录进行验证，避免误把 `dist/` 中的旧包当作本次产物。
Linux 发布 wheel 使用 manylinux_2_28（glibc 2.28）。

文档任务通过 uv 隔离环境使用与 CI 相同的锁定版 Zensical，不再要求单独维护文档虚拟环境。

发布流程见[发布 PolicyVisor](releasing.md)，运行时要求见[可复现示例](examples.md)。
