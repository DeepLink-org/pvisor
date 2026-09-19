# 安装指南

PolicyVisor（pVisor）为 Agent CLI、脚本和自动化命令提供策略控制与可检查的执行记录。
CLI 仍为 `pvisor`，已发布的 Python 包仍为 `persisting`。仓库链接、crate 名称和
`PERSISTING_*` 环境变量继续使用现有名称。

## 1. 安装工具

```bash
pip install persisting
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
pip install persisting
```

!!! tip "从一次 Run 开始"

    继续阅读[第一次运行](first-run.md)。审查 staged workspace 不需要另起一个历史服务。

发布的 wheel 面向 Linux x86_64 和 macOS arm64。其他架构应先检查发布产物或准备源码构建。

## 2. 检查平台要求

CLI 支持 macOS 和 Linux，要求 Python 3.10 或更新版本。普通 host Run 不需要文件系统扩展。
在 macOS 上使用 host-process 的 staged Run（`pvisor run --stage …`）前，先安装 macFUSE：

```bash
brew install --cask macfuse
```

macOS 提示时允许 macFUSE system extension。不带 `--stage` 时命令 可能直写项目目录；
带 `--stage` 且挂载能力不可用时，Run 会 fail closed，而不会静默退化为无 COW 直写。
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

未指定时 VM 也默认使用 `ubuntu:latest`。`--image-store DIR` 修改本地内容寻址缓存，
`--overlayfs-path` 选择 guest workspace，`--rootfs DIR` 指向预先准备的 Linux rootfs。
Linux 使用 KVM；Apple Silicon macOS 使用 HVF。从源码在 macOS 构建 VM 支持还需要 Zig：

```bash
brew install zig
```

把这些选项当作独立的平台步骤。先完成 staged host workflow，再用已有 Run Bundle 对比不同
执行环境的边界。

## 5. 选择下一步

- [第一次运行](first-run.md) —— 暂存、审查并选择性应用修改。
- [选择工作流](index.md) —— 从安装到一次可审查 Run 的最短路径。
- [执行环境](../guides/execution.md) —— 比较 host、OCI 与 VM 的边界。
