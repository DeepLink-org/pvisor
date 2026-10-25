# Codex CLI

```bash
pvisor spawn --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

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
