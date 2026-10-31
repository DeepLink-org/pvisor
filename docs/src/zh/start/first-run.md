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

# 运行并审查 {#local-workflow}

暂存两个文件，预览改动，再只留下一个。无需 Agent、模型账号或 VM 镜像。

**开始前：**[安装 pVisor 并完成平台准备](installation.md#local-prerequisites)。Linux 需要 FUSE 和 user namespace；Apple Silicon macOS 需要启用 FSKit 的 macFUSE 5.4.0+。以下操作使用同一个终端。

<a id="prepare-demo"></a>

## 1. 运行命令 {#run-and-stage}

创建一个新的演示目录：

```bash
mkdir pvisor-demo
cd pvisor-demo
```

启用文件暂存，拒绝出站网络访问，再运行命令：

```bash
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

两个文件仍在暂存区。确认原目录没有变化：

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

```text
Workspace unchanged
```

## 2. 审查改动 {#review-changes}

```bash
pvisor review last --diff
```

应看到 `hello.txt` 和 `scratch.txt` 两项新增及其文本差异。`last` 表示当前工作区最近一次登记的 Job。

预览只接受 `hello.txt`，不写入工作区：

```bash
pvisor apply last --path hello.txt --dry-run --diff
```

确认预览包含 `hello.txt`，不包含 `scratch.txt`。

## 3. 只留下一个文件 {#apply-changes}

合入选中的文件，丢弃其余改动，再检查结果：

```bash
pvisor apply last --path hello.txt  # Keep only hello.txt
pvisor drop last                   # Discard remaining staged changes
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

最后两条命令输出：

```text
Hello from pVisor
scratch.txt: absent
```

## 刚才发生了什么？ {#what-happened}

`spawn --safe` 在写时复制的工作区中运行命令，并记录一个 Job。`review` 展示暂存改动，`apply --path` 只将选中的文件写入原目录，`drop` 丢弃其余内容。

合入前请停止并发写入；若原文件发生冲突，`apply` 会拒绝覆盖。文件审查和 `drop` 无法撤销外部 API 调用，`drop` 也会保留已合入的文件。详见[审查并应用改动](../guides/review-apply.md)。

!!! note "如果启动失败"
    `--safe` 无法落实所需隔离或文件暂存时会拒绝启动。请检查[平台前置条件](installation.md#local-prerequisites)，并保留该参数；去掉它会让普通 Host Job 直接写入原工作区。

## 下一步 {#next-steps}

在自己的项目中，将演示命令换成已安装的 Agent CLI：

```bash
pvisor spawn --safe -- claude
pvisor review last --diff  # Review before applying
```

- [Agent 接入](../guides/agents/index.md)：配置工具和模型凭据。
- [审查并应用改动](../guides/review-apply.md)：接受目录、处理冲突。
- [VM 工具执行](infrastructure.md)：在 Linux VM 中执行工具并收集反馈。
- [CLI 参考](../reference/cli.md)：查看命令与参数。
