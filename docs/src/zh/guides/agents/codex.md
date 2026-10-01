# Codex CLI

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## `--safe` 为 Codex 做了什么

- **网络**：按命令名 `codex` 匹配预设，只放行 `api.openai.com`、`chatgpt.com`、`ab.chatgpt.com` 的 443 端口；
- **工作区**：改动进入暂存区，审查后再 apply；
- **HOME 与 `CODEX_HOME`**：使用独立的私有 stage，Codex 的状态改动在 Run 结束后丢弃，不进入工作区 Run Bundle；
- **敏感路径**：视图内的 `.ssh`、`.gnupg` 与私钥文件被拒绝。

## 不加 `--safe` 时

直接运行 Codex（不加 `--safe`）时，pVisor 保留宿主环境继承，以维持账号与路由配置；但 Codex 的状态和项目写入会直接到达宿主，不能用 `drop` 撤销。需要审查改动时请使用 `--safe` 或 `--stage PATH`。

## 凭据

使用 `--safe` 时，凭据需要显式交付：用 `--pass-env OPENAI_API_KEY`，或配置 Gateway 由可信侧持有 Key，见[凭据与环境变量](../policies/credentials.md)。

## 在 VM 中运行

需要不可绕过的网络边界或固定的 Linux 用户空间时：

```bash
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
```

镜像中必须安装 Codex。VM 的准备与限制见 [libkrun VM](../executors/vm.md)。

## 分叉

对一次运行的结果不满意、但想保留它的文件状态换种方式继续时：

```bash
pvisor fork last -- codex
```

见[逻辑检查点与分叉](../fork-checkpoint.md)。
