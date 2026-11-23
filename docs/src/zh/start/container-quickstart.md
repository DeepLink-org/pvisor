---
description: 在 Apple Silicon 上安装 pVisor，运行一个输出 hello 的 Alpine 容器。
---

# 运行你的第一个容器 {#container-quickstart}

安装 pVisor，再运行一个输出 `hello` 的 Alpine 容器。不需要 Docker daemon、
模型账号或 API key。

## 开始前 {#prerequisites}

- **Apple Silicon Mac：** `uname -sm` 输出 `Darwin arm64`，且
  `sysctl -n kern.hv_support` 输出 `1`。Linux x86_64 用户请按
  [容器命令演练](first-run.md#docker-style)操作。
- **下载工具：** `command -v curl tar shasum` 应列出三个命令。若缺少工具，
  请参阅[其他安装方式](installation.md#installation-options)。
- **网络：** 安装脚本需要访问 GitHub；首次运行会从 Docker Hub 拉取
  `alpine:3.20`，需要留出镜像缓存空间。

## 1. 安装稳定版 CLI {#install-cli}

这条命令会执行项目的 Shell 安装脚本。如需先审阅脚本，可以去掉管道，下载后再执行。

```sh
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install.sh | sh
```

预期看到以 `Installed pVisor` 开头的一行。脚本安装到你的主目录，不修改 Shell 配置。

如果没看到安装成功提示，先检查能否访问 GitHub，或改用[其他安装方式](installation.md#installation-options)。

## 2. 确认 pVisor 能创建 VM {#check-vm}

```sh
"$HOME/.local/share/pvisor/cli/current/bin/pvisor" vm-check --json
```

预期看到 `"can_create_vm": true` 和 `"firmware": "embedded"`。此时只创建了一个空 VM，
还没有启动容器。

如果任一值不符，请检查 [VM 前置条件](installation.md#docker-prerequisites)。

## 3. 运行 Alpine 并看到 `hello` {#run-container}

```sh
"$HOME/.local/share/pvisor/cli/current/bin/pvisor" run --rm --network none alpine:3.20 echo hello
```

如果终端先显示镜像下载日志，等它结束，随后应看到：

```text
hello
```

这行输出来自容器。`--rm` 会在退出后删除容器，已下载的镜像仍保留在缓存中。
镜像由宿主拉取，guest 启动后不联网。

拉取失败时检查 Docker Hub 连通性与磁盘空间；拉取成功但 VM 无法启动时，
查阅 [VM 排查说明](../guides/executors/vm.md)。

## 下一步 {#next-steps}

- [让容器保持运行并执行更多命令](first-run.md#docker-exec)。
- [把 `pvisor` 加入 `PATH`](installation.md#shell-installer)。
- [提问求助](https://github.com/DeepLink-org/pvisor/discussions)。
