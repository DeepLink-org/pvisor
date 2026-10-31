---
description: "在 pVisor 下运行 Codex CLI：--safe 的网络白名单、独立的 HOME 与 CODEX_HOME 暂存，以及如何审查暂存的文件改动。"
---

# Codex CLI

在自己的项目中运行 Codex，结束后检查暂存改动，再选择要接收的文件。下面使用本机已安装的 Codex 和 API Key；项目文件通过 Host stagebox 暂存。

!!! note "开始前"
    完成[安装与 Host 暂存准备](../../start/installation.md#local-prerequisites)，并安装 Codex CLI。准备一个可测试的小任务和模型访问权限；保留 Codex 自身的审批设置。

## 运行 Codex {#run-codex}

<div class="pv-guide-steps" markdown="1">

1. ### 检查工具并进入项目 {#check-tools}

    在要修改的项目根目录打开终端，确认工具可用：

    ```bash title="Shell"
    pvisor --version
    codex --version
    ```

    应看到两个版本号。记录版本和当前项目状态，再通过你已有的凭据管理方式，在当前终端设置 `OPENAI_API_KEY`。不要把 Key 写进项目文件。

1. ### 在暂存环境中认证并启动 {#launch-codex}

    使用 Bash 或兼容的宿主 Shell 执行：

    ```bash title="Shell"
    pvisor spawn --safe \
      --overlaynet-allow api.openai.com:443 \
      --pass-env OPENAI_API_KEY -- sh -c '
      printenv OPENAI_API_KEY | codex login --with-api-key && exec codex
    '
    ```

    这条命令在同一个 Job 的私有 HOME 中完成 API Key 登录并启动 Codex。登录从标准输入读取 Key，不把它放进命令参数。认证方式见 [OpenAI 官方说明](https://developers.openai.com/codex/auth/)。

    !!! tip "认证状态"
        `--safe` 的 HOME 写入在任务结束后丢弃，下次启动需要重新认证。这个例子只放行模型 API；依赖下载和其他目标需要另行配置，见[网络策略](../policies/network.md)。

1. ### 交付任务并检查改动 {#review-task}

    给 Codex 一个明确目标，例如修复指定失败测试，并要求保留现有测试、运行相关检查。完成后退出 Codex，回到原终端：

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    ```

    检查任务状态、文件差异和实际测试结果。文件应保留在暂存区，原目录尚未接收修改。`last` 指当前工作区最近一次登记的 Job；完成审查前不要在此目录启动其他 Job。

1. ### 接收需要的文件 {#accept-task}

    根据差异选择项目中实际存在的路径，例如源码目录：

    ```bash title="Shell"
    pvisor apply last --path src
    ```

    在原项目重新运行相关测试。确认剩余候选都不需要时，再执行 `pvisor drop last`；若已全部接收，跳过 drop。冲突和失败恢复见[审查并应用改动](../review-apply.md)。

</div>

!!! note "验证范围"
    这是一条 API Key 接入路径，不代表已完成云模型端到端或免审批验收。已有受控客户端测试使用 Codex 0.160.0，范围见[端到端任务](../../benchmarks/agent-tasks.md)。需要带失败测试和明确验收结果的任务材料时，可使用 [Python 修复示例](claude-code.md#task-project)，将任务交给 Codex。

## `--safe` 为 Codex 做了什么

网络预设按命令名 `codex` 匹配，只放行 `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` 的 443 端口。HOME 与 `CODEX_HOME` 使用独立的私有 stage，状态改动在 Run 结束后丢弃，不进入工作区 Run Bundle。工作区暂存与敏感路径规则沿用[Agent 通用设置](index.md)。

## 不加 `--safe` 时

直接运行 Codex（不加 `--safe`）时，pVisor 保留宿主环境继承，以维持账号与路由配置；但 Codex 的状态和项目写入会直接到达宿主，不能用 `drop` 撤销。需要审查改动时请使用 `--safe` 或 `--stage PATH`。

## 凭据

使用 `--safe` 时，凭据需要显式交付：用 `--pass-env OPENAI_API_KEY`，或配置 Gateway 由可信侧持有 Key，见[凭据与环境变量](../policies/credentials.md)。

## 在 VM 中运行

需要不可绕过的网络边界或固定的 Linux 用户空间时：

```bash
pvisor spawn --safe --vm --rootfs image=my-agent-image:latest -- codex
```

镜像中必须安装 Codex。VM 的准备与限制见 [libkrun VM](../executors/vm.md)。

## 分叉

对符合[工作区分叉前提](../fork-checkpoint.md)的已停止 host Job，可以保留文件状态，换种方式继续：

```bash
pvisor fork last -- codex
```

见[逻辑检查点与分叉](../fork-checkpoint.md)。
