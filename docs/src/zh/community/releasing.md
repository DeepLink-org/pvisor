# 发布 PolicyVisor

当前稳定发布流程由 GitHub Actions 从版本 tag 构建原生组件，以 Python wheel
交付，并通过 Trusted Publishing 上传到 PyPI。安装方式见[安装指南](../start/installation.md)。
wheel 不包含 PyO3 扩展，也不使用 Maturin。

每个平台 wheel 标记为 `py3-none-<platform>`，并包含：

- Python `pvisor` 版本标记，不是启动器；
- 三个原生脚本 `pvisor`、`pvisor-tui`、`pvisor-replay`，直接安装到环境的 bin 目录；
- 独立构建的 `pvisor-daemon`，Linux x86_64 和 macOS ARM64 wheel 均包含；
- `libkrunfw.5.dylib`，仅 Apple Silicon macOS 包含；Linux 在构建时内嵌内核。

当前发布集包含 Linux x86_64 和 Apple Silicon macOS wheel，以及对应固件源码。
Python 源码分发包（sdist）不在发布集中。

## 一次性设置

1. 创建名为 `pypi` 的 GitHub environment。不要要求 reviewer；把部署限制到
   匹配 `v*` 的 tag。
2. 在 PyPI 发布设置中添加 pending Trusted Publisher：
   - PyPI project: `pvisor`
   - GitHub owner: `DeepLink-org`
   - Repository: `pvisor`
   - Workflow: `release.yml`
   - Environment: `pypi`

3. 注册带 `pvisor-release` 标签的可信原生 runner：Linux x86_64 需要 KVM
   权限，Apple Silicon macOS 需要 Hypervisor.framework 权限。安装 mise 锁定的
   Python/Rust 与对应的 Linux musl Rust target（`mise run setup`）。这些 runner
   应专用于可信发布提交，并在各次运行间清理旧 Job 状态。缺少 runner 或虚拟化
   权限会阻止发布。

GitHub 中不存放 PyPI API token。pending publisher 可以在首次成功上传时
创建项目，但不预留名称。

首次推送发布 tag 前，确认 `pvisor` 的 Trusted Publisher 与当前仓库、工作流和 environment 匹配。

## 准备一次发布

1. 在 `pyproject.toml`、`Cargo.toml` 的 workspace package 段以及
   `pvisor/__init__.py` 中更新同一 `X.Y.Z` 版本。
2. 刷新 lockfile 中的本地 workspace 版本，且不升级依赖：

   ```bash
   cargo metadata --format-version 1 >/dev/null
   ```

3. 提交版本变更并合并到 `main`。工作流会拒绝其 commit 无法从 `main` 到达
   的 tag。
4. 可选地手动运行 **Publish PyPI**。手动运行会构建并校验全部 wheel，但不
   发布它们。
5. 创建并推送匹配的稳定 tag：

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

## 构建与校验路径

PEP 517 backend 是 setuptools，配合仓库自有的
`scripts/packaging/build_backend.py`。组装 wheel 之前，它会构建 `pvisor` Rust
CLI 等原生组件并暂存平台载荷。`setup.py` 直接安装二进制脚本，不使用 Python console-entry-point 包装器，并把 wheel 标为
平台相关，同时把 Python 与 ABI tag 保持为 `py3-none`。

打包使用 `fw/` 中固定版本的固件源码，校验下载的内核输入，并按输入缓存构建。
`PVISOR_LIBKRUNFW_PATH` 可选择已有 payload。缺少固件是构建错误。各平台导出
对应固件源码，与 wheel 一起发布。

Linux CLI 使用 `x86_64-unknown-linux-musl` 全静态链接，并内嵌 libkrunfw 内核，
wheel 不包含固件共享库。wheel 保留 manylinux_2_28 标签以支持 glibc Python
安装器。构建需要 Zig、cargo-zigbuild 和 Rust musl target；固件只在构建时加载。

Apple Silicon macOS 使用原生 Darwin linker 构建 CLI，签署 HVF entitlement，
并打包 `libkrunfw.5.dylib`。两个平台都内嵌静态 Linux musl Rust guest，
由 `rust-lld` 自动构建。macOS 需要 `aarch64-unknown-linux-musl` Rust stdlib，
构建 guest 不需要 Zig。

稳定发布先在发布提交上执行必需 CI，再构建 wheel。每个最终 wheel 在对应
原生 runner 的全新环境中安装。验收覆盖 host 退出码、strict host 执行拒绝、真实
VM 启动、暂停／冷恢复后内存进度继续、Job 身份保持且 Attempt 更换、文件暂存、
review。另一条新 VM Job 验证选择性 apply；恢复后的叠层 Job 必须拒绝当前不支持
的 apply。小型静态 guest 由仓库源码编译，缺少硬件会导致验收失败。
`release-acceptance-*` 制品保存 wheel 与安装后二进制的 SHA-256 及命令结果。
本地运行同一验收：`mise run accept-wheel -- WHEEL LOG_JSON`。

发布集检查要求每个平台恰好一个受支持 wheel、版本匹配、包元数据有效，以及产物
大小有界。两个平台的验收与完整发布集均通过后，才能向任一渠道发布。

稳定版本不可变。重跑在发布前核对 GitHub/PyPI 现有文件的 SHA-256；相同内容
保留，缺失文件可以补齐，同文件名内容不同则失败，必须发布新版本。GitHub 上传
不覆盖已有资源，PyPI 不再仅凭文件名跳过。先在 GitHub 发布对应固件源码，再向
PyPI 发布 wheel。配置了门禁不等于验收已经通过。

Linux x86_64 daemon 使用原生 `pvisor-vm`，需要 KVM 和委派 cgroup v2。新模板
使用共享 Rust guest 引擎与宿主协议适配，显式提供 execd/envd 时保留上游后端。
wheel 验收覆盖原生 Job；daemon SDK 兼容性、containerd 集成和密度仍需各自的
验收证据。见 [daemon 准备](../guides/daemon/index.md)。

## Nightly 构建

**Nightly Build** 每天 UTC 03:00（北京时间 11:00）运行，也可以在 `main` 上手动
触发；普通 push 运行 CI，不构建 nightly wheel。Nightly 与稳定发布共用
Linux/macOS 构建矩阵、安装 smoke test 和完整产物集校验。
Nightly 版本追加 `+g<run-number>.<commit>`，仅更新 GitHub 的 `nightly` release。
稳定 tag 发布先把已校验 wheel 与对应固件源码上传到 GitHub Release，再将 wheel 上传到 PyPI。daemon 可从两个平台的 nightly wheel 安装或从源码构建。缓存服务支持两个平台；sandbox 服务仍需上述 Linux 运行条件。不发布 daemon 独立归档包。

## 分发演进 {#distribution-plan}

!!! note "提案，尚未实现"
    优先完善原生二进制安装、CLI 和结构化输出，再按接入需求扩展包管理器与 SDK。
    以下渠道扩展、统一发行清单和安装器尚未接入当前发布流程。

### 渠道定位

| 渠道 | 面向用户 | 当前状态与方向 |
| --- | --- | --- |
| PyPI | Python 环境中的 CLI 用户 | 已有 wheel 流程；补充独立工具环境的安装、升级验收 |
| GitHub Release | CI、服务器与离线安装 | 已有 wheel 和固件源码；拟增加无需 Python/Rust 的二进制归档与版本化安装器 |
| Homebrew | macOS 开发者 | 拟先提供自有 tap，复用固定版本的原生产物 |
| npm | Node 工具链中的 CLI 用户 | 按需求增加原生程序安装入口；与 TypeScript SDK 分开交付 |
| crates.io / Cargo | Rust 嵌入与源码构建用户 | 后续评估；先处理内部 path 依赖、构建资源和公开 API 的发布边界 |

### 共享原生产物

每个平台完成原生构建后，wheel、二进制归档和包管理器入口复用同一组组件。
发行清单拟记录版本、commit、平台、组件哈希及固件来源；各归档分别记录自己的
SHA-256。发布阶段上传已验收的产物，不再次编译。同版本的 CLI、companion、
daemon 与固件作为一个安装单元升级，避免部分更新造成不兼容。

新增渠道需验证最终安装布局，包括 Homebrew 软链接、Python 工具环境中的
companion 和固件定位。安装器应支持指定版本、用户级目录、校验、升级、回退和
卸载；卸载保留 Job 与用户缓存。包管理器安装的程序由该包管理器升级。
安装 daemon 可执行文件与注册、启动系统服务分开进行。

首批继续覆盖 Linux x86_64 与 macOS arm64。扩大平台支持前，增加相应原生验收。
渠道验收除版本输出外，还需覆盖首次有效执行、已有暂存审查流程、升级和卸载；
VM 能力沿用真实硬件验收。Stable 保持不可变，nightly 使用独立入口。

### CLI、协议与 SDK

运行现有 Agent CLI 不要求语言 SDK：pVisor 直接启动命令，Agent 的实现语言不影响
这一入口。平台集成可先使用已文档化的命令、退出码和
[JSON 输出](../reference/json-output.md)，并按[兼容边界](../reference/stability.md)
固定版本；完整 schema 和跨版本稳定协议仍需完善。

先收敛启动、取消、状态、事件与文件审查的结构化契约，再根据实际集成用户选择
TypeScript 或 Python SDK。npm 安装入口可以只分发 Rust 原生 CLI；它不要求
TypeScript 重写，也不意味着已经提供 SDK。SDK 不是当前 CLI 发布的前置条件。
