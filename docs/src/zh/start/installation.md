# 安装指南

装好 `pvisor` 后，你可以在策略边界内运行 Agent CLI、脚本和自动化命令，并拿到可检查的执行记录。
Python 包、CLI 和核心 Rust crate 统一使用 `pvisor`；其他 crate 使用 `pvisor-*`，
环境变量使用 `PVISOR_*`。

升级时同步更新部署中的 `PVISOR_*`；本地状态默认写入 `.pvisor`，用户缓存写入 `pvisor/`，
已有数据不会自动搬迁。

## 1. 安装工具

```bash
pip install pvisor
```

确认命令可用：

```bash
pvisor --version
```

wheel 将 Python 版本标记与原生 CLI 脚本直接安装到当前 Python 环境的 bin 目录，不经过 Python 启动器。项目有其他
Python 依赖时，建议使用虚拟环境：

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install pvisor
```

发布的 wheel 面向 Linux x86_64 和 macOS arm64。其他架构应先检查发布产物或准备源码构建。

## 2. 检查平台要求

wheel 安装支持 macOS 和 Linux，要求 Python 3.10 或更新版本；安装后的原生 CLI 不使用 Python 启动器。普通 host Job 默认直接写入工作区；
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
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

本地开发时，从 checkout 安装 Python 包：

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
pip install -e .
```

也可以从源码构建 CLI：

```bash
just install-cli
```

测试特定原生构建时，直接调用其路径（例如 `target/debug/pvisor`），或把其目录放在 `PATH` 最前。
安装后的原生脚本不使用旧启动器的 `PVISOR_BIN` 覆盖。Editable Python 安装不是原生 CLI 构建；
使用 `just build` 或 `just install-cli`。

## 单独安装单机 daemon {#daemon}

需要一台 Linux 主机上的 OpenSandbox 兼容生命周期 API 时，按 [daemon 安装与启动](../guides/daemon/index.md)操作。`pvisor-daemon` 是独立可执行文件，仅当前 Linux x86_64 wheel 包含它，也可使用独立 nightly 归档或源码构建。macOS wheel 不包含它，任何分发产物都不提供可直接使用的沙箱镜像。其部分 OpenSandbox 1.1.0 profile 已有 VM-only NativeRuntime，在 Linux x86_64/KVM 与委派 cgroup v2 上嵌入 pVisor；可执行入口已接入原生运行时构造，同步内部 VM 派发先于 Tokio。

可工作的沙箱需要可信本机 manifest/rootfs，以及经 guest CID 3 vsock bridge 连接的真实 execd/egress。Bootstrap 与镜像配方未提供或端到端验证；启动 API 不建立 SDK 兼容或密度证据。Stage/apply 与 checkpoint API 未实现，也不自动获取 node 共享。Controller/Worker 与 Cluster SDK 已退役；跨节点编排属于外部调度器。

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
[工程说明](../community/development.md)。

把这些选项当作独立的平台步骤。先完成 staged host workflow，再用已有 Run Bundle 对比不同
执行环境的边界。
