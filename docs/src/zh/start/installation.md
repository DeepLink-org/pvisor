---
description: 逐步安装 pVisor，检查 Linux 或 macOS 的文件暂存支持，再进入第一个 Agent style stagebox 任务。
---

# 安装指南

安装 CLI 后，按所选模式准备平台，再按[快速上手](first-run.md)运行示例并检查结果、清理环境。

发布包支持 Linux x86_64 和 Apple Silicon macOS。Python 安装需要 3.10+，无需 Rust 工具链或 Docker Daemon。Docker style 路径需要虚拟化与镜像下载；离线 Agent style 演示使用本机 Shell，无需 VM 镜像或模型账号。

<a id="1"></a>

## 1. 安装 CLI {#install-cli}

在普通用户终端执行。先确认 Python 版本：

```bash
python3 --version
```

应为 3.10 或更新版本。创建专用虚拟环境并安装：

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
```

安装后命令名为 `pvisor`，Python 分发包名为 `pvisor-sandbox`。打开新终端后，运行 `source ~/.venvs/pvisor/bin/activate` 再使用 CLI。若 `venv` 不可用，先安装操作系统提供的 Python venv 支持。

已有项目虚拟环境时可以直接在其中安装。希望不依赖 Python 时，使用下方的 [Homebrew 或原生包](#native-downloads)。

<a id="2"></a>

## 2. 准备平台环境 {#prepare-environment}

按所选模式准备环境。Docker style 使用 VM，不需要安装 Host 暂存所用的 FUSE。

| 路径 | 需要准备 |
| --- | --- |
| Docker style sandbox | [VM 支持](#docker-prerequisites)，以及镜像仓库访问与缓存空间。 |
| Agent style stagebox 离线演示 | [Host 文件暂存](#local-prerequisites)，以及本机 Shell。 |

### Docker style sandbox：VM 支持 {#docker-prerequisites}

Linux x86_64 上检查架构与当前账号的 KVM 权限：

```bash
uname -m
test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
```

应输出 `x86_64` 和 `KVM accessible`。设备缺失或无权限时，请管理员配置虚拟化和账号权限；在虚拟化服务器内还需开放嵌套虚拟化。Linux 的 VMM 限制也可能受 namespace 策略影响，启动权限错误见 [VM 指南](../guides/executors/vm.md)。

Apple Silicon macOS 上，`uname -m` 应输出 `arm64`，使用带 Hypervisor entitlement 的发布包；源码构建需按 [VM 平台说明](../guides/executors/vm.md)签名。这个 VM 路径不要求安装用于 Host 暂存的 macFUSE。

首次运行需要访问 Docker Hub，并留出镜像缓存空间。按[Docker 教程](first-run.md#docker-style)启动 VM，确认配置可以正常工作。

### Agent style stagebox：Host 文件暂存 {#local-prerequisites}

`--safe` 需要能挂载暂存文件视图。请只完成与你的平台对应的一组步骤。

#### Linux x86_64 {#linux-setup}

检查架构、FUSE 设备和挂载工具：

```bash
uname -m
ls -l /dev/fuse
command -v fusermount3 || command -v fusermount
```

架构应为 `x86_64`，设备应存在，最后一条命令应输出工具路径。Debian/Ubuntu 上缺少工具时，可安装 `fuse3`；其他发行版使用对应软件包。设备缺失或无访问权限时，由管理员配置 FUSE；在容器或受管服务器中还需允许设备访问与挂载。

再检查 user namespace 的基本可用性：

```bash
unshare --user --map-root-user true
```

成功时没有输出，退出码为 0。若出现 `Operation not permitted`、`setgroups` 或 `uid_map` 错误，请管理员检查 user namespace 与 AppArmor 策略。此探测不能替代 pVisor 的实际启动检查；细节见 [Host 执行器](../guides/executors/host.md)。

#### Apple Silicon macOS {#macos-setup}

确认架构：

```bash
uname -m
```

应输出 `arm64`。安装 macFUSE 5.4.0 或更新版本：

```bash
brew install --cask macfuse
```

在“系统设置 → 通用 → 登录项与扩展 → 文件系统扩展”中启用 macFUSE，按系统提示完成后续操作。pVisor 使用 FSKit，无需加载内核扩展或降低启动安全级别。若 FSKit 不可用，暂存启动会失败。

普通 Host Job 默认直接写入工作区。教程使用 `--safe` 启用暂存；不要通过移除它绕过挂载失败。

??? note "可选：Linux VM 工具执行环境"

    <a id="4-vm-oci"></a>

    ### 执行基础设施 {#vm-and-oci}

    [VM 工具教程](infrastructure.md)在 Linux x86_64 节点上运行。除 CLI、Python、Git 和 Bash 外，确认当前账号可访问 KVM：

    ```bash
    test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
    ```

    应输出 `KVM accessible`。没有输出时，请管理员检查设备与账号权限；虚拟化服务器还需提供嵌套虚拟化。该示例也需要 user namespace 权限，不能仅凭 KVM 检查通过就认定环境可用。

    首次准备任务镜像需要访问 Docker Hub，并有足够磁盘空间缓存镜像。镜像内已含 Python、测试工具和项目代码，不继承 Host 虚拟环境，无需 Docker Daemon。离线 rootfs 准备见 [VM 指南](../guides/executors/vm.md#rootfs)。

    这组检查对应 Linux VM 教程；其他平台的 VM 支持与前置条件见 [VM 执行器](../guides/executors/vm.md)。

## 3. 检查安装 {#verify-installation}

在已激活安装环境的终端运行：

```bash
command -v pvisor
pvisor --version
pvisor run --help
pvisor spawn --help
```

应看到 CLI 路径、版本号和两种入口的帮助。下一步通过所选教程的实际运行验证 VM 或暂存是否可用。

| 遇到的问题 | 处理方法 |
| --- | --- |
| 找不到 `pvisor` | 重新激活虚拟环境；原生包用户将包内 `bin` 加入 `PATH`。 |
| pip 报 Python 环境受系统管理 | 使用上方虚拟环境安装。 |
| 没有匹配当前平台的安装包 | 核对 Python 版本与系统架构；见[平台矩阵](../reference/platforms.md)。 |
| CLI 可用，但任务启动失败 | 按[本地前置条件](#local-prerequisites)检查 FUSE、FSKit 和 user namespace，保留 `--safe`。 |

## 4. 完成首次任务 {#next-steps}

- 已准备 VM：[Docker style sandbox](first-run.md#docker-style)，从镜像到运行、exec、停止与删除。
- 已准备 Host 暂存：[Agent style stagebox](first-run.md#agent-style)，从离线命令到审查、接收和清理。

完成 stagebox 演示后，用 [Claude Code 修复任务](../guides/agents/claude-code.md#repair-task)体验真实 Agent 的成果交接。需要从 Python 调用方收集 VM 工具反馈时，再使用[执行基础设施示例](infrastructure.md)。

<a id="other-installation-methods"></a>

## 其他安装与维护方式 {#installation-options}

### 升级与卸载 {#upgrade-and-uninstall}

在安装 pVisor 的 Python 环境中运行：

```bash
python -m pip install --upgrade pvisor-sandbox
# Uninstall when needed:
python -m pip uninstall pvisor-sandbox
```

升级前检查发布说明和部署中的 `PVISOR_*` 设置。卸载不会清理 Job 状态与用户缓存。

<a id="homebrew"></a>

### Homebrew 与原生下载包 {#native-downloads}

从[项目 Homebrew tap](https://github.com/DeepLink-org/Formula) 安装：

```bash
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor
```

tap 仓库名为 `Formula`，而不是 `homebrew-formula`，因此添加时需要指定完整 URL。tap 只包含软件包元数据，二进制从 GitHub Releases 下载。升级时运行 `brew update` 和 `brew upgrade DeepLink-org/formula/pvisor`。

不使用 Python 安装时，可从 [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) 下载对应平台的原生归档及 `SHA256SUMS`。校验后解压，将 `bin` 加入 `PATH`，并保持其中的文件在一起；升级时替换完整目录。

Homebrew tap 的发布条件与配置见[发布指南](../community/releasing.md#one-time-setup)。

Job 启动要求可执行文件及其祖先目录属于当前用户或 root，且其他用户不能写入；
root 拥有、带 sticky bit 的目录（如 `/tmp`）除外。Homebrew 的 Cellar 可能带有
组写权限。如果启动报 `unsafe owner executable ancestor`，先检查错误中指明的目录。
对于当前用户独占、Cellar 允许组写入的 Homebrew 安装，可先去掉组和其他用户的写权限：

```bash
chmod go-w "$(brew --cellar)"
brew test DeepLink-org/formula/pvisor
```

此命令只修改 Cellar 目录本身的权限，不递归修改已安装的软件包。
需要保留组写权限的共享 Homebrew 安装，应改用 wheel 或原生归档，安装到自己拥有的受保护目录。

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
