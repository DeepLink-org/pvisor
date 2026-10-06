# 发布 PolicyVisor

稳定发布由 GitHub Actions 从版本 tag 构建，并通过 Trusted Publishing 发布到
PyPI。项目仍然以 Python wheel 交付，但不包含 PyO3 扩展，也不使用 Maturin。

每个平台 wheel 标记为 `py3-none-<platform>`，并包含：

- Python `pvisor` 版本标记，不是启动器；
- 原生脚本 `pvisor`、`pvisor-cache`、`pvisor-tui`、`pvisor-replay` 与 `pvisor-memory-pool`，直接安装到环境的 bin 目录；
- 独立构建的 `pvisor-daemon`，仅 Linux x86_64 wheel 包含；
- `libkrunfw.5.dylib`，仅 Apple Silicon macOS 包含；Linux 在构建时内嵌内核。

当前发布集包含 Linux x86_64 和 Apple Silicon macOS wheel。源码分发不是已
发布产物的一部分。

## 一次性设置

1. 创建名为 `pypi` 的 GitHub environment。不要要求 reviewer；把部署限制到
   匹配 `v*` 的 tag。
2. 在 PyPI 发布设置中添加 pending Trusted Publisher：
   - PyPI project: `pvisor`
   - GitHub owner: `DeepLink-org`
   - Repository: `pvisor`
   - Workflow: `release.yml`
   - Environment: `pypi`

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

打包脚本会拉取 pinned 的 libkrun firmware 归档（Linux x86_64 与 Apple
Silicon macOS），除非 `PVISOR_LIBKRUNFW_PATH` 指向已有 payload。本地
wheel 构建必须走这些受支持路径之一；缺少 payload 是构建错误，而不是不完整
的 wheel。

Linux CLI 使用 `x86_64-unknown-linux-musl` 全静态链接，并内嵌 libkrunfw 内核，
不再随 wheel 分发固件共享库。wheel 保留 manylinux_2_28 标签以支持 glibc Python
安装器。构建需要 Zig、cargo-zigbuild 和 Rust musl target；固件只在构建时加载。

Apple Silicon macOS 使用原生 Darwin linker 构建 CLI，签署 HVF entitlement，
并打包 `libkrunfw.5.dylib`。两个平台都内嵌静态 Linux musl Rust guest，
由 `rust-lld` 自动构建。macOS 需要 `aarch64-unknown-linux-musl` Rust stdlib，
构建 guest 不需要 Zig。

每个 wheel 都会检查组件集和安装时 CLI smoke test。发布集检查随后要求每个
平台恰好一个受支持 wheel、版本匹配、包元数据有效，以及发布前产物大小有界。

对部分完成的 tagged 发布再跑一遍时，会跳过 PyPI 已经接受的文件，并补齐
缺失的 GitHub Release 资源。

Linux x86_64 daemon 使用原生 `pvisor-vm`，不是 rootless Podman。wheel／独立程序都需要 KVM 权限、委派 cgroup v2，以及可信预制镜像 manifest/rootfs；bootstrap 须监督真实 execd/egress 服务，并提供 guest CID 3、44772/18080 端口的 vsock bridge。Bootstrap／镜像配方未提供或端到端验证。分发程序不建立 SDK 兼容或密度证据。见 [daemon 准备](../guides/daemon/index.md)。

## Nightly 构建

**Nightly Build** 每天 UTC 03:00（北京时间 11:00）运行，也可以在 `main` 上手动
触发；普通 push 运行 CI，不再重复构建 nightly wheel。Nightly 与稳定发布共用
Linux/macOS 构建矩阵、安装 smoke test 和完整产物集校验。
Nightly 版本追加 `+g<run-number>.<commit>`，仅更新 GitHub 的 `nightly` release。
稳定 tag 发布先上传 PyPI，再把同一组已校验 wheel 附加到 GitHub Release。Nightly 还发布独立的 `pvisor-daemon-linux-x86_64.tar.gz` 及 SHA-256 checksum；它无需 wheel 或单独 CLI，但仍需要上述原生运行时条件。
