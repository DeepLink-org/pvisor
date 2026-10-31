---
description: 逐步运行离线命令、检查暂存文件、选择性合入并丢弃剩余改动。
---

<a id="_1"></a>
<a id="1"></a>
<a id="2-agent"></a>
<a id="demo-script"></a>
<a id="3"></a>
<a id="4"></a>
<a id="5"></a>
<a id="6"></a>
<a id="7"></a>

# 两种模式快速上手 {#local-workflow}

完成[安装](installation.md)后选择一条路径。首次体验只需完成所选模式的前置条件。

- [Docker style sandbox](#docker-style)：拉取镜像、启动独立 VM、执行命令，再停止并删除容器。
- [Agent style stagebox](#agent-style)：用离线命令创建两个文件，审查并只接收一个，再清理剩余改动。

## Docker style sandbox {#docker-style}

使用本机终端。Linux x86_64 需要可读写的 `/dev/kvm`；Apple Silicon macOS 需要具备 Hypervisor entitlement 的发布包。按[VM 平台准备](installation.md#docker-prerequisites)完成检查，无需 Docker Daemon 或模型账号。这里不挂载宿主目录。

### 1. 准备镜像 {#docker-image}

```bash
pvisor pull alpine:3.20
```

等待下载完成。首次运行需要访问镜像仓库和足够缓存空间；后续可复用缓存。镜像拉取失败时先检查网络和磁盘空间，不继续启动步骤。

### 2. 执行一次命令 {#docker-run}

```bash
pvisor run --rm --network none alpine:3.20 echo hello
```

预期输出包含 `hello`。`--network none` 关闭 guest 网络；镜像下载在宿主完成。命令退出后 `--rm` 删除这次容器，镜像缓存保留。

### 3. 保持运行并执行工具 {#docker-exec}

使用一个尚未占用的名字；若 `pvisor-demo` 已存在，换名并同步替换后续命令：

```bash
pvisor run -d --name pvisor-demo --network none alpine:3.20 sleep 600
pvisor ps
pvisor exec pvisor-demo sh -c 'echo "inside the VM" > /tmp/result.txt'
pvisor exec pvisor-demo cat /tmp/result.txt
```

`ps` 应列出运行中的 `pvisor-demo`；最后一条命令应输出 `inside the VM`。两次 `exec` 使用同一容器，文件位于 VM 内的 `/tmp`，不会创建到宿主当前目录。`sleep 600` 让它最多运行十分钟，请在到期前完成后续步骤。

### 4. 停止并清理 {#docker-cleanup}

```bash
pvisor stop pvisor-demo
pvisor rm pvisor-demo
pvisor ps -a
```

最后的列表不应再包含 `pvisor-demo`。若十分钟已过、容器已停止，跳过 `stop`，直接 `rm`。删除容器会清理该容器写层及其中的 `/tmp/result.txt`；镜像缓存保留供重用。

| 问题 | 下一步 |
| --- | --- |
| KVM 权限或虚拟化不可用 | 检查[VM 前置条件](installation.md#docker-prerequisites)，虚拟化服务器还需开放嵌套虚拟化。 |
| 镜像架构不匹配 | 使用宿主原生架构镜像；当前不提供 CPU 模拟。 |
| 名称已存在 | 用 `pvisor ps -a` 检查，选新名称；只清理确认属于本次演示的容器。 |
| `exec` 找不到运行实例 | 用 `pvisor ps -a` 核对状态和名称，确认没有超过 `sleep` 时限。 |

该模式使用容器命令管理 VM；参数与卷的持久化规则见[容器参考](../reference/cli.md#container-commands)。未配置卷的演示不需要 Job 的 review/apply/drop。

## Agent style stagebox {#agent-style}

用本机 Shell 创建两个候选文件，只把问候文件交回原目录。不需要模型账号、VM 镜像或运行中的 Daemon。以下步骤在同一个终端、同一个目录依次执行。

### 开始前 {#before-you-start}

先完成[安装与平台准备](installation.md)。Linux x86_64 需要 FUSE 和 user namespace；Apple Silicon macOS 需要启用 FSKit 的 macFUSE 5.4.0+。

如果已完成安装，激活安装时的虚拟环境并检查 CLI：

```bash
source ~/.venvs/pvisor/bin/activate
pvisor --version
```

应看到版本号。通过 Homebrew 或原生包安装时，跳过 `source`。如果提示找不到命令，先按[安装检查](installation.md#verify-installation)修复，再继续。

<a id="prepare-demo"></a>

### 1. 创建演示目录 {#prepare-workspace}

在你希望存放演示文件的位置打开终端，创建一个空目录：

```bash
mkdir pvisor-demo
cd pvisor-demo
pwd
```

后续命令都在这个目录运行。若目录已存在，换个新名称，避免上次留下的文件影响结果。无需克隆 pVisor 仓库或初始化 Git。

### 2. 在 stagebox 模式中创建文件 {#run-and-stage}

运行下面的命令：

```bash
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

等待命令结束并回到终端提示符。`spawn` 启动并记录一次 Job；`--safe` 请求隔离和工作区暂存；`--overlaynet-deny-all` 为这个离线任务禁用网络；`--` 后面是要执行的程序及其参数。

命令将内容写入文件，没有终端输出。两个文件应留在暂存区。检查它们是否都没有出现在原目录：

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

预期输出：

```text
Workspace unchanged
```

如果没有这行输出，请先停止后续合入操作，确认启动命令包含 `--safe`、Job 成功启动，并检查当前目录。启动失败时见下方[常见问题](#troubleshooting)。

### 3. 审查暂存改动 {#review-changes}

查看刚才 Job 的状态和文件差异：

```bash
pvisor status last
pvisor review last --diff
```

`status` 应表明任务已停止；`review` 应列出 `hello.txt` 和 `scratch.txt` 两项新增，内容分别为 `Hello from pVisor` 和 `Temporary notes`。输出中的身份、路径和时间会随运行变化。

这里的 `last` 指当前工作区最近登记的 Job。完成本教程前不要在这个目录启动另一个 Job，否则 `last` 会指向新的任务。实际并行任务请使用明确的 [Job 身份](../reference/jobs.md)。

还可以在只读的任务视图中查看文件：

```bash
pvisor inspect last -- cat hello.txt
```

预期输出 `Hello from pVisor`。你可以在 Job 内读取这个文件；直接在原目录执行 `cat hello.txt` 仍应提示文件不存在。

写入原目录前，先预览所选路径：

```bash
pvisor apply last --path hello.txt --dry-run --diff
```

预览应只包含 `hello.txt`，两个文件仍留在暂存区。

### 4. 只合入一个文件 {#apply-changes}

假设你想保留问候文件，但不要临时笔记。只接受 `hello.txt`：

```bash
pvisor apply last --path hello.txt
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

预期输出包含：

```text
Hello from pVisor
scratch.txt: absent
```

现在 `hello.txt` 已进入原目录，`scratch.txt` 仍是未接受的候选改动。合入前停止其他可能修改相同文件的进程；若原文件存在冲突，`apply` 会拒绝覆盖，请按[冲突处理](../guides/review-apply.md)检查，保留 stage 后再决定。

### 5. 丢弃剩余改动 {#discard-changes}

确认不需要临时笔记后，丢弃剩余候选：

```bash
pvisor drop last
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

应再次看到相同的两行结果。`drop` 不会删除已经合入的 `hello.txt`。

### 刚才发生了什么？ {#what-happened}

| 操作 | Job 中的文件 | 原目录中的文件 |
| --- | --- | --- |
| `spawn --safe` 完成 | 两个文件均为候选改动 | 两个文件均不存在 |
| `apply --path hello.txt` 完成 | 临时笔记仍待处理 | 只有 `hello.txt` |
| `drop` 完成 | 剩余候选改动已丢弃 | 仍只有 `hello.txt` |

文件暂存控制的是文件改动的接受过程。`drop` 不会撤销已合入的文件或外部 API 调用；网络和读取限制取决于执行器与实际安装的控制，详见[安全边界](../security/index.md)。

### 遇到问题时 {#troubleshooting}

| 现象 | 检查与下一步 |
| --- | --- |
| `pvisor: command not found` | 激活安装环境，或检查原生包的 `bin` 是否在 `PATH`；见[安装检查](installation.md#verify-installation)。 |
| FUSE、FSKit 或挂载失败 | 完成[平台准备](installation.md#local-prerequisites)。Linux 检查设备与挂载工具，macOS 检查文件系统扩展。 |
| `setgroups`、`uid_map` 或 user namespace 权限错误 | 检查系统的 user namespace 与 AppArmor 策略；见 [Host 执行器](../guides/executors/host.md)。 |
| `last` 找不到任务或指向别的任务 | 回到启动 Job 的目录，用记录的 Job 身份定位；见 [Job 与存储](../reference/jobs.md)。 |
| `apply` 拒绝执行 | 确认 Job 已停止，并检查冲突或未完成的记录；见[审查与合入](../guides/review-apply.md)。 |

`--safe` 无法落实所需控制时会拒绝启动。不要通过删掉这个参数继续教程：普通 Host Job 默认直接写入原工作区。

## 下一步 {#next-steps}

- [用 Claude Code 修复测试](../guides/agents/claude-code.md#repair-task)：使用可复现的小项目、任务提示和验收步骤，完成真实 Agent 成果交接。
- [本地工作流](../guides/index.md)：把这套操作用于已有项目，验证候选改动并处理冲突。
- [VM 工具执行](infrastructure.md)：从调用脚本执行 Linux VM 中的测试并读取结构化反馈。
- [CLI 参考](../reference/cli.md)：查询参数、Job 定位与存储规则。
