# 安装指南

PolicyVisor（pVisor）为 Agent CLI、脚本和自动化命令提供策略控制与可检查的执行记录。
Python 包和 CLI 统一使用 `pvisor`。仓库链接、Rust crate 名称和
`PERSISTING_*` 环境变量继续使用现有名称。

如果之前安装了 `persisting`，请先执行 `python -m pip uninstall persisting`，
再安装 `pvisor`（包括 nightly wheel）。两个发行包会安装到相同的 CLI 路径，
不应在同一环境中并存。

## 1. 安装工具

```bash
pip install pvisor
```

确认命令可用：

```bash
pvisor --version
```

wheel 会把匹配版本的 Python 包和 `pvisor` CLI 安装到当前 Python 环境。项目有其他
Python 依赖时，建议使用虚拟环境：

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install pvisor
```

!!! tip "从一次 Run 开始"

    继续阅读[第一次运行](first-run.md)。审查 staged workspace 不需要另起一个历史服务。

发布的 wheel 面向 Linux x86_64 和 macOS arm64。其他架构应先检查发布产物或准备源码构建。

## 2. 检查平台要求

CLI 支持 macOS 和 Linux，要求 Python 3.10 或更新版本。普通 host Job 默认直接写入工作区；
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

## 3. 需要时从源码安装

需要 `main` 最新构建时，可以使用 nightly wheel：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

本地开发时，从 checkout 安装 Python 包：

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e .
```

也可以从源码构建 CLI：

```bash
just install-cli
```

只有在明确测试特定 pVisor 二进制时才设置 `PERSISTING_PVISOR_BIN`。排查 Provider 行为时，
应尽量让 Python 包和 CLI 来自同一 revision。

## 4. 需要时启用 VM 或 OCI 执行

默认本地工作流不要求安装 Docker 或 Podman。使用 VM executor 运行 OCI 镜像时，可以显式指定：

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

未指定 rootfs 或镜像时，Linux 上 VM 默认通过 virtiofs 和 OverlayFS 使用宿主 `/`，
不拉取镜像；macOS 上需要显式指定 Linux rootfs 或镜像。
`--image-store DIR` 修改本地内容寻址缓存，
`--mount SOURCE[:TARGET]:ACCESS` 暴露宿主路径，`--rootfs DIR` 指向预先准备的 Linux rootfs。
Linux 使用 KVM；Apple Silicon macOS 使用 HVF。guest supervisor 是静态 musl Rust ELF，
由 Rust 自带 linker 构建，macOS 不再需要 C 交叉编译器。源码构建前的工具链准备见
[工程说明](../development/engineering.md)。

把这些选项当作独立的平台步骤。先完成 staged host workflow，再用已有 Run Bundle 对比不同
执行环境的边界。

## 5. 选择下一步

- [第一次运行](first-run.md) —— 暂存、审查并选择性应用修改。
- [选择工作流](index.md) —— 从安装到一次可审查 Run 的最短路径。
- [执行环境](../guides/execution.md) —— 比较 host、OCI 与 VM 的边界。
