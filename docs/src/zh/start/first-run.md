---
description: 运行一条命令，审查文件改动，只合入你想保留的文件。
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

# 运行第一个 Job {#local-workflow}

让一条命令创建两个文件，再只留下其中一个。这个例子不需要 Agent、模型账号或 VM 镜像。

开始前，请[安装 pVisor 并完成平台准备](installation.md#local-prerequisites)：Linux 需要 FUSE 和 user namespace；Apple Silicon macOS 需要启用 FSKit 的 macFUSE 5.4.0+。以下命令在同一个终端执行。

<a id="prepare-demo"></a>


## 1. 运行命令 {#run-and-stage}

创建一个新目录，运行命令：

```bash
mkdir pvisor-demo && cd pvisor-demo
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

命令已结束，但两个文件仍在暂存区。确认原目录没有变化：

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

```text
Workspace unchanged
```

## 2. 审查改动 {#review-changes}

查看这次运行产生的改动：

```bash
pvisor review last --diff
```

应看到 `hello.txt` 和 `scratch.txt` 两项新增，以及它们的文本差异。`last` 表示当前工作区最近一次登记的 Job。

## 3. 只留下需要的文件 {#apply-changes}

只接受 `hello.txt`，丢弃剩余候选改动：

```bash
pvisor apply last --path hello.txt
pvisor drop last
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

```text
Hello from pVisor
scratch.txt: absent
```

## 刚才发生了什么？ {#what-happened}

`spawn --safe` 在写时复制的工作区中运行命令，并记录一个 Job。命令写出的两个文件都留在暂存区；`review` 让你检查它们，`apply --path` 将选中的文件写入原目录，`drop` 丢弃其余候选内容。

`drop` 不会撤销已合入的文件，也不会撤销外部 API 调用。合入前请停止并发写入；若原文件发生冲突，`apply` 会拒绝覆盖。更多规则见[审查并应用改动](../guides/review-apply.md)。

!!! note "如果启动失败"
    `--safe` 无法落实所需隔离或文件暂存时会拒绝启动。请检查[平台前置条件](installation.md#local-prerequisites)，并保留该参数；去掉它会让普通 Host Job 直接写入原工作区。

## 下一步 {#next-steps}

### 换成你的 Agent

在自己的项目中，将 `sh -c '…'` 换成已安装的 Agent CLI：

```bash
pvisor spawn --safe -- claude
pvisor review last --diff
```

工具和模型凭据的配置见 [Agent 接入](../guides/agents/index.md)。

- [审查并应用改动](../guides/review-apply.md)：接受目录、处理冲突。
- [VM 工具执行](infrastructure.md)：在 Linux VM 中执行工具并收集反馈。
- [CLI 参考](../reference/cli.md)：查看命令与参数。
