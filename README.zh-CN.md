<div align="center">

<img src="https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-icon.png" alt="pVisor" width="88" />

# PolicyVisor

**Agent 原生沙箱框架**

[English](README.md) · [简体中文](README.zh-CN.md)

[快速开始](https://deeplink-org.github.io/pvisor/zh/start/installation/) · [文档](https://deeplink-org.github.io/pvisor/zh/) · [示例](examples/pvisor/README.md) · [英文文档](https://deeplink-org.github.io/pvisor/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![文档](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/zh/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

</div>

pVisor 是一个沙箱框架，有两种模式。Docker style sandbox 为每个容器提供独立 VM。
Agent style stagebox 是 pVisor 独创的 Agent 模式：文件改动先暂存，由你审查后选择接收。

## 两种模式

| 模式 | 使用方式 | 隔离与结果管理 |
| --- | --- | --- |
| Docker style sandbox | 使用熟悉的镜像、run、exec、stop 和删除命令 | 每个容器对应独立 Linux VM；管理容器生命周期和文件操作 |
| Agent style stagebox | 让 Agent 尝试方案，审查暂存改动，再选择接收 | 原生 Job 显式启用暂存，通过 review/apply/drop 处理改动；隔离取决于所选执行器 |

### 沉浸式氛围编程

*无需反复权限确认。*

在 Agent style stagebox 模式中，让 Agent 在暂存工作区里尝试修改，完成后审查文件，
只接收需要的成果。免审批运行仍需按 Agent 和配置逐一验证，接入指南列有现有证据与限制。

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
原生安装包，以及两种模式各自的平台检查步骤。

### 2. 选择模式

#### Docker style sandbox

Linux x86_64 上准备 KVM，Apple Silicon macOS 上准备 Hypervisor.framework，然后运行：

```bash
pvisor run --rm alpine:3.20 echo hello
```

预期输出 `hello`。命令在独立 Linux VM 中执行，退出后删除该容器。首次使用会下载镜像。
[Docker 上手教程](https://deeplink-org.github.io/pvisor/zh/start/first-run/#docker-style)
包含保持沙箱运行、执行后续命令、检查状态和清理的步骤。
当前提供实验性的 Docker 兼容命令子集，不支持 Docker Engine API 或 Compose。

#### Agent style stagebox

下面的例子使用 Host 执行器。Linux x86_64 需要 FUSE 和 user namespace；
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
`last` 选择当前工作区最近一次登记的 Job。
[Stagebox 上手教程](https://deeplink-org.github.io/pvisor/zh/start/first-run/#agent-style)
会逐步检查原目录、暂存视图和最终文件状态。这个 Host 示例不需要模型账号或 VM 镜像。

### 3. 用 Agent 完成一个任务

[Claude Code 教程](https://deeplink-org.github.io/pvisor/zh/guides/agents/claude-code/#repair-task)
从一个测试失败的小型 Python 项目开始，给出任务提示和凭据配置，再带你检查暂存文件、
接收修改并在原项目复测。需要 Python、Claude Code 和模型访问权限。

> 开发者预览：“无需反复权限确认”描述 Agent style stagebox 的目标体验。
> Agent 指南列出了已有的受控客户端测试，以及仍待验证的云模型和审批行为。
> pVisor 不会自动关闭 Agent 或 IDE 的审批。

`--safe` 无法落实所需控制时会拒绝启动。普通 Host Job 默认直接写入工作区。
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

- 原生 Job：使用 `pvisor spawn` 选择 Host、OCI 或 VM 执行，显式启用暂存后进行 review/apply。
- 嵌入式运行时：集成 [Rust runtime](crates/pvisor/README.md)。
- 平台 API：使用[本地 daemon](crates/pvisor-daemon/README.md)接入 Linux/KVM 上部分 OpenSandbox/E2B SDK profile，或使用 [containerd shim](crates/pvisor-shim/README.md)。
- 容器工作流：实验性的 [Docker 风格 CLI](crates/pvisor-cli/README.md#docker-command-profile-experimental)为每个容器运行一个 VM。

这些接口分别定义生命周期和能力范围。Daemon 不提供原生 Job 的 stage/apply 语义；容器暂存挂载有独立的 review/apply/drop 契约。
需要调用脚本示例时，从 [VM 工具执行教程](https://deeplink-org.github.io/pvisor/zh/start/infrastructure/)开始；
各接口的区别见[接入矩阵](https://deeplink-org.github.io/pvisor/zh/guides/infrastructure/integration/)。

## 文档

- [安装指南](https://deeplink-org.github.io/pvisor/zh/start/installation/)：准备并检查环境
- [首次运行](https://deeplink-org.github.io/pvisor/zh/start/first-run/)：完成所选模式的启动与清理
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
