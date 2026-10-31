---
description: 在 Linux x86_64 或 Apple Silicon macOS 上从 Python wheel 安装 pvisor 命令，检查平台前置条件并选择快速开始。
---

# 安装

在 **Linux x86_64** 或 **Apple Silicon macOS** 上安装 pVisor，再运行第一条命令。发布包无需 Rust 或内核构建工具链。

<a id="1"></a>

## 1. 安装 CLI {#install-cli}

使用 **Python 3.10+** 和独立虚拟环境：

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

打开新终端后重新激活环境。已有项目虚拟环境也可以使用。

<a id="2"></a>

## 2. 准备环境 {#prepare-environment}

两个快速开始均使用 CLI，无需模型账号、API Key 或运行中的 Daemon。

### Host：本地命令 {#local-prerequisites}

[运行与审查示例](first-run.md)使用你的 Shell 和本机工具，无需 VM 镜像、Docker 或 Podman。

- **Linux x86_64：**需要 FUSE 挂载支持、`/dev/fuse` 访问权限、可用的 `fusermount3` 或 `fusermount`，以及创建 user namespace 的权限。平台边界见 [Host 执行器](../guides/executors/host.md)。
- **Apple Silicon macOS：**需要 macFUSE **5.4.0+**，并在“系统设置 → 通用 → 登录项与扩展 → 文件系统扩展”中启用。使用 Homebrew 安装：

```bash
brew install --cask macfuse
```

macOS 使用 macFUSE 的 FSKit 后端，无需内核扩展或降低启动安全级别。FSKit 不可用时会报错，不会退化为直接写入。**使用 `--safe` 或 `--stage` 才会暂存文件**；普通 Host Job 直接写入工作区。

??? note "VM：工具执行前置条件"

    <a id="4-vm-oci"></a>

    <a id="vm-and-oci"></a>

    [VM 工具示例](infrastructure.md)需要 **Linux x86_64 节点**，具备：

    - **KVM 与隔离权限：**可读写 `/dev/kvm`，允许创建 user namespace。遇到 `setgroups` 或 `uid_map` 错误时，请管理员检查 user namespace 和 AppArmor 策略。仅有 KVM 权限还不够；AppArmor 规则须匹配实际 CLI 路径。
    - **调用方工具：**Git、Bash 及上方安装的 Python 环境。VM 内的 Python、项目代码和测试依赖由任务镜像提供，不继承 Host 虚拟环境。
    - **镜像获取：**首次运行可访问 Docker Hub，并有磁盘空间缓存镜像。无需 Docker Daemon。参见[离线 rootfs 准备](../guides/executors/vm.md#rootfs)。

    脚本会自动创建 VM。Apple Silicon macOS 的 VM 支持、其他 rootfs 选项和挂载方式见 [VM 执行器指南](../guides/executors/vm.md)。

## 3. 运行示例 {#verify-installation}

通过[运行与审查](first-run.md)创建两个暂存文件，再只留下一个。需要将工具结果交回 Agent 或框架时，试试[在 VM 中运行工具](infrastructure.md)。

需要其他执行方式时，查阅[选择执行环境](../guides/executors/index.md)。

<a id="other-installation-methods"></a>

## 其他安装方式 {#installation-options}

### 升级与卸载 {#upgrade-and-uninstall}

在安装 pVisor 的环境中升级：

```bash
python -m pip install --upgrade pvisor-sandbox
```

请先检查发布说明与部署中的 `PVISOR_*` 设置。需要卸载时：

```bash
python -m pip uninstall pvisor-sandbox
```

卸载不会清理 Job 状态和用户缓存。

<a id="homebrew"></a>

<a id="native-downloads"></a>

??? note "Homebrew 与原生下载包"

    从[项目 Homebrew tap](https://github.com/DeepLink-org/Formula) 安装：

    ```bash
    brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
    brew install DeepLink-org/formula/pvisor
    ```

    仓库名是 `Formula`，而非 `homebrew-formula`，因此需要完整 URL。tap 存放元数据，二进制来自 GitHub Releases。升级时先运行 `brew update`，再运行 `brew upgrade DeepLink-org/formula/pvisor`。

    使用原生归档时，从 [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) 下载对应平台的软件包及 `SHA256SUMS`。校验后解压，将 `bin` 加入 `PATH`，保持文件在一起；升级时替换完整目录。

    tap 发布条件见[发布指南](../community/releasing.md#one-time-setup)。

??? warning "Homebrew 排错：unsafe owner executable ancestor"

    Job 启动要求可执行文件及祖先目录属于当前用户或 root，且其他用户不可写入；root 拥有、带 sticky bit 的目录（如 `/tmp`）除外。

    若启动报 `unsafe owner executable ancestor`，先检查错误中指明的目录。**仅对于当前用户独占、Cellar 允许组写入的 Homebrew 安装**，去掉组和其他用户的写权限，再测试：

    ```bash
    chmod go-w "$(brew --cellar)"
    brew test DeepLink-org/formula/pvisor
    ```

    此操作只修改 Cellar 目录，不递归修改软件包。共享安装若需要保留组写权限，请改用 wheel 或原生归档，放在自己拥有的受保护目录中。

<a id="3"></a>
<a id="3-nightly"></a>

<a id="nightly-source"></a>

??? note "Nightly 与源码构建"

    需要 `main` 的 nightly 时，先下载并检查 [nightly 安装脚本](https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh)，再在已激活的 Python 环境中运行。安装脚本会在你的机器上执行来自该仓库的代码。

    需要修改或构建原生程序时，按[开发环境说明](../community/development.md)获取源码并准备工具链。

<a id="daemon"></a>

??? note "单节点 Daemon：OpenSandbox／E2B SDK 接入"

    发布包包含 `pvisor-daemon`。按 [Daemon 指南](../guides/daemon/index.md)准备 Linux/KVM 节点与委派的 cgroup v2，并确认所用 API 的支持范围。两个快速开始均不需要 Daemon。
