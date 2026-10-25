# 安装指南

在运行工具命令的机器上安装 pVisor：本地工作流使用你的电脑，执行基础设施示例使用 Linux/KVM 节点。当前发布包支持 Linux x86_64 和 Apple Silicon macOS；安装发布包无需 Rust 或内核构建工具链。

<a id="1"></a>

## 1. 安装 CLI {#install-cli}

使用 Python 3.10 或更新版本，建议安装到独立虚拟环境：

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor
pvisor --version
```

应输出 pVisor 的版本号。打开新终端后，重新激活此环境；已有项目虚拟环境时，也可以在其中安装。

<a id="2"></a>

## 2. 按使用场景准备环境 {#prepare-environment}

只需完成所选路径的准备。两个快速开始示例均无需模型账号或 API Key。

### 本地工作流 {#local-prerequisites}

示例使用本机工具和工作区，通过 Host 执行器暂存文件改动，无需 VM 镜像、Docker 或 Podman。请准备 Git、Bash，以及平台对应的文件系统挂载支持：

- **Linux x86_64**：需要 FUSE 挂载支持，当前用户可访问 `/dev/fuse`，并可使用 `fusermount3` 或 `fusermount`。示例的隔离模式还要求系统允许创建 user namespace；平台边界见 [Host 执行器](../guides/executors/host.md)。
- **Apple Silicon macOS**：安装 macFUSE 5.4.0 或更新版本，并在“系统设置 → 通用 → 登录项与扩展 → 文件系统扩展”中启用 macFUSE：

```bash
brew install --cask macfuse
```

macOS 使用 macFUSE 的 FSKit 后端，无需加载内核扩展或降低启动安全级别。FSKit 不可用时会报错，不会自动退化为直接写入。暂存仅在使用 `--safe` 或 `--stage` 时启用；普通 Host Job 默认直接写入工作区。

<a id="4-vm-oci"></a>

### 执行基础设施 {#vm-and-oci}

在运行示例的 **Linux x86_64 节点**上准备：

- **KVM 与隔离权限**：当前账号可读写 `/dev/kvm`，且允许创建 user namespace。若出现 `setgroups` 或 `uid_map` 权限错误，请管理员检查 user namespace 和 AppArmor 策略；KVM 权限不能替代这些条件，AppArmor 规则应匹配实际 CLI 路径。
- **调用方环境**：安装 Git、Bash，并使用上一步的 Python 环境运行调用脚本。VM 内的 Python、项目代码和测试依赖由任务镜像提供，不继承 Host 的虚拟环境。
- **镜像获取**：首次运行可访问 Docker Hub，并有足够磁盘空间缓存镜像，无需 Docker Daemon。离线准备方式见 [VM 执行器指南](../guides/executors/vm.md#rootfs)。

示例脚本会创建 VM，无需先手动启动一台。其他 VM 平台、rootfs 和挂载选项见 [VM 执行器指南](../guides/executors/vm.md)。

## 3. 选择上手路径 {#verify-installation}

| 你想做什么 | 从这里开始 |
| --- | --- |
| 在本机运行命令，审查并接受文件改动 | [本地工作流：运行与审查](first-run.md) |
| 从调用脚本执行工具，并把反馈交回 Agent 或框架 | [执行基础设施：工具执行接入示例](infrastructure.md) |

需要更换执行方式时，再查阅[选择执行环境](../guides/executors/index.md)。

<a id="other-installation-methods"></a>

## 其他安装与维护方式 {#installation-options}

### 升级与卸载 {#upgrade-and-uninstall}

在安装 pVisor 的 Python 环境中运行：

```bash
python -m pip install --upgrade pvisor
# Uninstall when needed:
python -m pip uninstall pvisor
```

升级前检查发布说明和部署中的 `PVISOR_*` 设置。卸载不会清理 Job 状态与用户缓存。

<a id="homebrew"></a>

### Homebrew 与原生下载包 {#native-downloads}

不使用 Python 安装时，可从 [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) 下载对应平台的原生归档及 `SHA256SUMS`。校验后解压，将 `bin` 加入 `PATH`，并保持其中的文件在一起；升级时替换完整目录。

Homebrew tap 的发布条件与配置见[发布指南](../community/releasing.md#one-time-setup)。

<a id="3"></a>

<a id="3-nightly"></a>

### Nightly 与源码构建 {#nightly-source}

需要 `main` 的 nightly 构建时，在已激活的 Python 环境中安装：

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

需要修改或构建原生程序时，按[开发环境说明](../community/development.md)获取源码、准备工具链并构建 CLI。

### 单节点 Daemon {#daemon}

发布包包含 `pvisor-daemon`。需要通过 OpenSandbox／E2B SDK 接入时，按 [Daemon 指南](../guides/daemon/index.md)准备 Linux/KVM 节点与委派的 cgroup v2，并确认所用 API 的支持范围。两个快速开始示例使用 CLI，无需启动 Daemon。
