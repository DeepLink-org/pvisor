<div align="center">

<img src="docs/overrides/assets/logos/pvisor-logo.png" alt="PolicyVisor" width="420" />

# PolicyVisor

**面向 Agent 工作负载优化的容器运行时**

[English](README.md) · [简体中文](README.zh-CN.md)

[快速开始](https://deeplink-org.github.io/pvisor/zh/start/installation/) · [文档](https://deeplink-org.github.io/pvisor/zh/) · [示例](examples/pvisor/README.md) · [英文文档](https://deeplink-org.github.io/pvisor/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![文档](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/zh/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

</div>

pVisor 兼容一组常用 Docker CLI 命令，并提供审查和接收 Agent 改动的命令。

- **更好的隔离性**：每个容器使用[独立的 Linux 内核](docs/src/zh/security/executor-boundaries.md)，减少 Agent 运行工具时对宿主和其他任务的影响。
- **更节约资源**：启用[内存页共享](docs/src/zh/benchmarks/vm-memory/index.md)后，多个 Agent 使用相同的工具和库时，宿主只需为共享页保留一份副本。
- **提速智能体应用**：[惰性镜像](docs/src/zh/benchmarks/lazy-image-startup.md)按需加载内容，Agent 不必等整个镜像下载完就能开始运行工具。

在 Linux 上，用 `:stage` 挂载项目，Agent 的文件改动就不会直接写回工作区。容器退出后，用 `review` 查看差异、`apply` 接收选中的路径、`drop` 丢弃其余改动。

## 快速开始

### 1. 安装 CLI

使用 Python 3.10 或更新版本：

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

[安装指南](https://deeplink-org.github.io/pvisor/zh/start/installation/)还包含 Homebrew、
原生安装包，以及运行各示例所需的平台检查步骤。

### 2. 试用 CLI

#### 运行容器

Linux x86_64 上准备 KVM，Apple Silicon macOS 上准备 Hypervisor.framework，然后运行：

```bash
pvisor run --rm alpine:3.20 echo hello
```

预期输出 `hello`。命令在独立 Linux VM 中执行，退出后删除该容器。首次使用会下载镜像。
[Docker 上手教程](https://deeplink-org.github.io/pvisor/zh/start/first-run/#docker-style)
包含保持沙箱运行、执行后续命令、检查状态和清理的步骤。
当前提供实验性的 Docker 兼容命令子集，包括基于镜像的
[`pvisor compose`](crates/pvisor-cli/README.md#compose-command-profile-experimental)；
不提供 Docker Engine API，也不支持完整的 Compose 规范。

在支持的 Unix 命令范围内，`alias docker=pvisor` 使用同一套 Session 运行时：
根命令 `inspect` 返回元数据，`kill` 向当前执行发送信号，`export` 导出已停止容器的
rootfs tar。未知命令直接报错，不会作为主机任务执行。原生任务使用显式的 `spawn`
或 `pvisor [SESSION_OPTIONS] -- COMMAND`，也保留仅含选项的 `--config` / `--spec`
文件执行入口；`inspect --files` 选择任务的文件系统视图。
Shell alias 不会重定向 Docker SDK 的 socket，也不会替换子进程查找的可执行文件。

#### 用 Agent 命令审查任务改动

如果工具已安装在本机，`pvisor spawn --safe` 可以直接启动暂存写入的 Host Session，不经过 VM。Linux x86_64 需要 FUSE 和 user namespace；
Apple Silicon macOS 需要启用 FSKit 的 macFUSE 5.4.0 或更新版本。在新建的演示目录中运行：

```bash
mkdir pvisor-demo
cd pvisor-demo
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
pvisor review last --diff
pvisor apply last --path hello.txt
pvisor drop last
cat hello.txt
```

预期输出 `Hello from pVisor`。问候文件已进入项目，临时笔记已丢弃。
`last` 选择当前工作区最近一次登记的 Session。
[文件暂存教程](https://deeplink-org.github.io/pvisor/zh/start/first-run/#agent-style)
会逐步检查原目录、暂存视图和最终文件状态。这个 Host 示例不需要模型账号或 VM 镜像。

### 3. 用 Agent 完成一个任务

[Claude Code 教程](https://deeplink-org.github.io/pvisor/zh/guides/agents/claude-code/#repair-task)
从一个测试失败的小型 Python 项目开始，给出任务提示和凭据配置，再带你检查暂存文件、
接收修改并在原项目复测。需要 Python、Claude Code 和模型访问权限。

> 开发者预览：Agent 免审批运行仍需验证云模型和审批行为，
> 已有的受控客户端测试见 Agent 指南。pVisor 不会自动关闭 Agent 或 IDE 的审批。

`--safe` 无法落实所需控制时会拒绝启动。普通 Host Session 默认直接写入工作区。
接收或丢弃文件不会撤销远端 API 调用、数据库写入、费用，也不会撤销已经合入的文件。
详见[安全边界](https://deeplink-org.github.io/pvisor/zh/security/)。

## Linux 预览：宿主工具与容器暂存挂载

`pvisor stage` 在 Linux VM 中运行宿主已安装的工具，保留 rootfs 改动供审查。
apply 默认只处理记录的项目范围，环境变化继续暂存。宿主文件仍可读取，底层宿主文件系统
不是冻结快照。系统级写回是独立的特权操作，不是操作系统升级事务。

```bash
pvisor stage -it --stage ../pvisor-experiment
# Use the shell, then exit before reviewing.
pvisor review ../pvisor-experiment --diff
pvisor apply ../pvisor-experiment --path src --dry-run --diff
pvisor apply ../pvisor-experiment --path src
pvisor drop ../pvisor-experiment
```

确认剩余候选均不需要后才执行 drop；全部接收后应跳过。
平台准备与限制见 [stage 指南](crates/pvisor-cli/README.md#staged-system-experiments-linux-preview)。

Linux 的容器命令还支持暂存项目挂载：

```bash
mkdir project
pvisor run --name edit -v ./project:/workspace:stage alpine:3.20 \
  sh -c 'echo "Hello from a VM" > /workspace/hello.txt'
pvisor review edit --diff
pvisor apply edit --path hello.txt --dry-run --diff
pvisor apply edit --path hello.txt
pvisor rm edit
```

`:stage` 是 pVisor 扩展；普通 `:ro` 和 `:rw` 挂载行为保持不变。只有暂存挂载可以向
记录的宿主来源写回，不能接收 instance rootfs 的改动。退出后再审查；多个暂存挂载需用
`--mount` 选择。暂存挂载不能使用 `--rm` 或混合直接可写挂载。有剩余候选时，先按需
丢弃再删除实例。挂载选择和生命周期见[容器暂存挂载](docs/src/zh/reference/cli.md#container-staged-binds)。

## 执行与集成

pVisor 也为 Agent 推理、评测和 RL 后训练提供执行环境。资源共享和空闲内存回收用于管理机器容量，
执行记录和暂存改动用于检查结果。具体能力取决于接口、执行器和平台。

- 原生 Session：使用 `pvisor spawn` 选择 Host、OCI 或 VM 执行，显式启用暂存后进行 review/apply。
- Agent 插件：[Pi、DSH 和 Codex 任务适配器](docs/src/zh/guides/agents/plugins.md)沿用原生 `spawn → review → apply/drop`；不接管 Agent 内置工具。
- 嵌入式运行时：集成 [Rust runtime](crates/pvisor/README.md)。
- 平台 API：使用[本地 daemon](crates/pvisor-daemon/README.md)接入 Linux/KVM 上部分 OpenSandbox/E2B SDK profile，或使用 [containerd shim](crates/pvisor-shim/README.md)。
- 容器工作流：实验性的 [Docker 风格 CLI](crates/pvisor-cli/README.md#docker-command-profile-experimental)为每个容器运行一个 VM。

这些接口分别定义生命周期和能力范围。Daemon 不提供原生 Session 的 stage/apply 语义；容器暂存挂载有独立的 review/apply/drop 契约。
需要调用脚本示例时，从 [VM 工具执行教程](https://deeplink-org.github.io/pvisor/zh/start/infrastructure/)开始；
各接口的区别见[接入矩阵](https://deeplink-org.github.io/pvisor/zh/guides/infrastructure/integration/)。

### 试用实验性的 Rust guest kernel

在具备 KVM 的 Linux x86_64 主机上，使用内嵌 Rust guest kernel 的 `pvisor`。
从当前源码执行 `mise run build` 即可构建。下面只为这次运行切换固件：

```bash
PVISOR_NATIVE_KERNEL=rust pvisor run --rm --cpus 2 --memory 512m alpine:3.20 echo hello
```

预期输出 `hello`。Linux 固件仍是默认值；如果之前用 `export` 设置了变量，
执行 `unset PVISOR_NATIVE_KERNEL` 切回 Linux。x86_64 上的 Rust 固件支持
1–2 个 vCPU、至少 256 MiB guest 内存，目前不支持快照和恢复。详见
[Rust guest kernel README](pvisor-kernel/README.md#experimental-selection)。

## 文档

- [pVisor 0.4.0 发布说明](docs/src/zh/community/changelog.md#v040)
- [安装指南](https://deeplink-org.github.io/pvisor/zh/start/installation/)：准备并检查环境
- [首次运行](https://deeplink-org.github.io/pvisor/zh/start/first-run/)：运行容器或审查暂存任务，并清理环境
- [Agent 接入](https://deeplink-org.github.io/pvisor/zh/guides/agents/)：配置命令、凭据和网络访问
- [CLI 参考](https://deeplink-org.github.io/pvisor/zh/reference/cli/)：命令与配置
- [基准测试](https://deeplink-org.github.io/pvisor/zh/benchmarks/)：测量结果与实验条件
- [架构](https://deeplink-org.github.io/pvisor/zh/design/architecture/)：执行机制与职责划分

## 参与开发

按 [CONTRIBUTING.md](CONTRIBUTING.md)配置工具链并运行相应检查。
使用 [Discussions](https://github.com/DeepLink-org/pvisor/discussions)讨论问题，
通过 [SECURITY.md](SECURITY.md)报告安全漏洞。

## 许可证

[Apache-2.0](LICENSE)。随附第三方组件见 [NOTICE](NOTICE)。
