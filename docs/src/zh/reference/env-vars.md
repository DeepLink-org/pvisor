# 环境变量

通常只需要设置任务存储位置，以及明确传给 Agent 的凭据。宿主上的变量影响 pVisor 自身；任务实际收到的变量由环境投影规则决定。

```bash
PVISOR_RUN_HOME="$HOME/.pvisor/runs" pvisor run --safe \
  --stage ../stage-env-001 --overlaynet-deny-all -- /bin/sh -c 'printf "%s\n" "$PVISOR_RUN_ID"'
```

这次任务打印自己的 Run ID。需要联网凭据时，按[凭据与环境指南](../guides/policies/credentials.md)使用 `--pass-env`。

## 常用宿主设置

| 变量 | 当前行为 |
| --- | --- |
| `PVISOR_RUN_HOME` | 默认 Run 存储根目录；未设置用 `~/.pvisor/runs`，无 HOME 时用系统临时目录 |
| `PVISOR_IMAGE_STORE` | OCI 镜像缓存目录的环境覆盖；显式 `--image-store` 优先 |
| `PVISOR_CACHE_SERVER` | 共享镜像缓存端点；`off` 禁用，未设置使用按用户的 Unix socket 自动探测 |
| `PVISOR_CACHE_TOKEN` | TCP 缓存端点的共享密钥；不要通过 `--pass-env` 交给 Agent |
| `PVISOR_BIN` | Python 启动器选择的 pVisor 二进制；通常无需设置 |
| `XDG_CONFIG_HOME` | 用户策略根；未设置用 `~/.config` |
| `HOME` / `PATH` | 存储、工具发现与环境投影的宿主输入；safe 下 HOME 会重定向 |

缓存的端点语法、安全要求和失败行为见[共享镜像缓存](shared-image-cache.md)。不要把宿主读取变量与 Agent 可见变量混为一谈；显式投影用 `--pass-env NAME`。

## 运行时注入

`PVISOR_RUN_ID`、`PVISOR_RUNTIME`、`PVISOR_STORAGE`、`PVISOR_AGENT`、`PVISOR_ROLE` 标识运行环境。`PVISOR_AGENTCTL_ENDPOINT`、`PVISOR_AGENTCTL_TOKEN`、`PVISOR_AGENTCTL_TRANSPORT`、`PVISOR_AGENTCTL_VERSION` 用于协作控制通道；token 是凭据，不应写进日志。

启用代理时还会注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及小写形式。它们只为客户端指定代理，不单独提供不可绕过的网络隔离。完整投影以 Bundle 的 `environment` 变量名清单为准。

`PVISOR_KRUN_RUNNER_SPEC`、`PVISOR_KRUN_NETWORK_FD` 等是内部启动协议；`PVISOR_KRUN_LOG` 和 `PVISOR_KRUN_ENOMEM_WORKAROUND` 是 VM 诊断设置，不是稳定产品配置。使用上表配置宿主；内部变量留给启动器与诊断工具管理。

## 启动诊断与源码构建 {#build-and-diagnostics}

`PVISOR_STARTUP_TIMING` 默认启用启动阶段日志；设为 `0` 关闭，适合不带日志开销的测量。它只影响诊断输出，不改变任务策略。

下面两个变量影响 Linux x86_64 musl **构建时**嵌入的 guest 内核：

| 变量 | 输入 | 优先级 |
| --- | --- | --- |
| `PVISOR_KRUNFW_KERNEL_BUNDLE` | 含 `kernel.bin` 与 `kernel.json` 的目录 | 设置时优先使用 |
| `PVISOR_KRUNFW_PATH` | 要提取内核的 `libkrunfw.so.5` 文件 | 未设置 bundle 时使用 |

更换这两个输入后重新构建 CLI。运行已经构建好的 musl 二进制时设置这些变量，不会替换其中的内核；动态固件入口与平台条件见[平台矩阵](platforms.md)。构建源见 `crates/pvisor-vm/build_kernel.rs`。
