# 安装指南

用 Python 3.10 或更新版本安装原生 CLI，随后运行一个本地命令。
当前 wheel 支持 Linux x86_64 和 Apple Silicon macOS；安装发布包无需 Rust 或内核构建工具链。

## 1. 安装工具

建议在独立虚拟环境中安装，避免修改系统 Python 环境：

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor
```

确认安装：

```bash
pvisor --version
pvisor spawn -- /bin/echo hello
```

wheel 将 `pvisor`、`pvisor-tui`、`pvisor-replay` 和 `pvisor-daemon` 安装到该环境的
bin 目录，不经过 Python 启动器。打开新终端后重新激活环境，或直接使用
`~/.venvs/pvisor/bin/pvisor`。已有项目虚拟环境时，也可以在其中安装。

在同一环境中升级：

```bash
python -m pip install --upgrade pvisor
```

需要卸载时运行：

```bash
python -m pip uninstall pvisor
```

升级前检查发布说明和部署中的 `PVISOR_*` 设置。本地状态（`.pvisor`）和用户缓存
（`pvisor/`）不会自动搬迁，卸载 Python 包也不清理这些运行数据。
npm、Cargo 安装入口与可选版本的独立安装器仍属
[分发提案](../community/releasing.md#distribution-plan)。

### Homebrew 与原生下载包

项目 tap 完成配置并发布首个稳定版本后，可运行：

```bash
brew install DeepLink-org/pvisor/pvisor
brew test DeepLink-org/pvisor/pvisor
# Upgrade: brew upgrade pvisor / brew uninstall pvisor
```

tap 使用 `DeepLink-org/homebrew-pvisor`；维护者需要先完成[一次性设置](../community/releasing.md#one-time-setup)，此命令才能使用。
支持 Apple Silicon macOS 和 Linux x86_64。Homebrew 不安装 Python、不编译 Rust，也不把 daemon 启动为系统服务。

不使用包管理器时，打开 [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases)，下载
`pvisor-X.Y.Z-linux-x86_64.tar.gz` 或 `pvisor-X.Y.Z-macos-arm64.tar.gz`，以及 `SHA256SUMS`。
核对对应的校验值（Linux 用 `sha256sum 归档文件`，macOS 用 `shasum -a 256 归档文件`），
将归档解压到当前用户拥有、按版本命名的目录（例如 `~/.local/opt/pvisor` 下），把其中的 `bin` 加入 `PATH`，并保持 `bin` 中的全部文件在一起。
升级时替换整个目录；卸载时删除该目录及 `PATH` 项，Job 状态和用户缓存仍保留。
滚动 `nightly` 发布提供相同布局的原生归档。

## 2. 检查平台要求

wheel 安装支持 macOS 和 Linux，要求 Python 3.10 或更新版本。普通 host Job 默认直接写入工作区；
`--safe` 或 `--stage` 才使用文件系统暂存。在 macOS 上运行暂存的 host Job 前，先安装 macFUSE：

```bash
brew install --cask macfuse
```

macOS 默认使用 macFUSE 的 **FSKit 后端**。安装 macFUSE 5.4.0 或更新版本（旧版 FSKit 存在小写入变零的数据损坏问题）后，在
“系统设置 → 通用 → 登录项与扩展 → 文件系统扩展”中启用 macFUSE。
这条路径不加载内核扩展，不需要进入 Recovery 或降低启动安全级别。
挂载点位于 `/Volumes/pvisor-*`，暂存数据仍保存在 Job 的 stage 目录。
FSKit 不可用时会直接报错，不会自动切换内核后端或退化为直写。
libkrun VM executor 不需要 macFUSE。

## 3. Nightly 与源码构建

需要 `main` 的 nightly 构建时，在已激活的 Python 环境中安装：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

需要自行修改或构建原生程序时，获取源码并按
[开发环境说明](../community/development.md)准备工具链：

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

在准备完成的 checkout 中构建并安装 CLI：

```bash
mise run install-cli
```

测试特定原生构建时，直接调用其路径（例如 `target/debug/pvisor`），或把其目录放在 `PATH` 最前。
安装后的原生脚本忽略 `PVISOR_BIN`。Editable Python 安装不是原生 CLI 构建；
使用 `mise run build` 或 `mise run install-cli`。

## 安装单节点 daemon {#daemon}

Linux x86_64 和 macOS ARM64 wheel 都包含独立的 `pvisor-daemon` 可执行文件。缓存和独立内存池可在两平台运行；OpenSandbox／E2B sandbox API 需要 Linux x86_64/KVM 与委派 cgroup v2。

通过 `prepare-image` 准备可信基础镜像，新模板默认使用 Rust guest 引擎与共享压缩镜像池。固定版本 SDK 的核心生命周期、命令和文件流程已有原生验证；完整 SDK conformance 与生产密度仍未建立。部署命令、协议范围和证据见[daemon 指南](../guides/daemon/index.md)。Stage/apply 与 checkpoint API 仍由原生 Job 路径提供；跨节点编排交给外部调度器。

## 4. 需要时启用 VM 或 OCI 执行

默认本地工作流不要求安装 Docker 或 Podman。使用 VM executor 运行 OCI 镜像时，可以显式指定：

```bash
pvisor spawn --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

未指定 rootfs 或镜像时，Linux 上 VM 默认通过 virtiofs 和 OverlayFS 使用宿主 `/`，
不拉取镜像；macOS 上需要显式指定 Linux rootfs 或镜像。
`--image-store DIR` 修改本地内容寻址缓存，
`--mount SOURCE[:TARGET]:ACCESS` 暴露宿主路径，`--rootfs DIR` 指向预先准备的 Linux rootfs。
Linux 使用 KVM；Apple Silicon macOS 使用 HVF。guest supervisor 是静态 musl Rust ELF，
由 Rust 自带 linker 构建，macOS 不需要 C 交叉编译器。源码构建前的工具链准备见
[工程说明](../community/development.md)。

环境准备完成后，用[第一次运行](first-run.md)完成暂存、审查和选择性 apply。
需要检查实际安装的文件与网络控制时，查看[执行器边界](../security/executor-boundaries.md)。
