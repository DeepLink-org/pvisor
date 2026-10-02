---
status: todo
search:
  exclude: true
---

# 环境变量

!!! warning "规划中"
    本页尚无完整参考。投影给 Agent 的变量见[凭据与环境变量](../guides/policies/credentials.md)。

## 要回答的问题

pVisor 读取哪些 `PVISOR_*` 环境变量（例如 `PVISOR_RUN_HOME`、`PVISOR_CACHE_SERVER`），又向 Agent 注入哪些变量？

## 需求

- 从代码中收集全部读取点，自动生成两张表：pVisor 读取的变量、注入给 Agent 的变量；
- 每个变量说明：作用、默认值、适用的执行器与平台、是否稳定。

## 验收标准

- 表格由脚本生成，CI 检查没有遗漏代码中新增的 `PVISOR_*` 读取点。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[CLI 参考](cli.md)

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

缓存的端点语法、安全要求和失败行为见[共享镜像缓存](shared-image-cache.md)。不要把宿主读取变量与 Agent 可见变量混为一谈。

## 运行时注入

`PVISOR_RUN_ID`、`PVISOR_RUNTIME`、`PVISOR_STORAGE`、`PVISOR_AGENT`、`PVISOR_ROLE` 标识运行环境。`PVISOR_AGENTCTL_ENDPOINT`、`PVISOR_AGENTCTL_TOKEN`、`PVISOR_AGENTCTL_TRANSPORT`、`PVISOR_AGENTCTL_VERSION` 用于协作控制通道；token 是凭据，不应写进日志。

启用代理时还会注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及小写形式。它们只为客户端指定代理，不单独提供不可绕过的网络隔离。完整投影以 Bundle 的 `environment` 变量名清单为准。

`PVISOR_KRUN_RUNNER_SPEC`、`PVISOR_KRUN_NETWORK_FD` 等是内部启动协议；`PVISOR_KRUN_LOG` 和 `PVISOR_KRUN_ENOMEM_WORKAROUND` 是 VM 诊断设置，不是稳定产品配置。全部读取点的自动清单仍待生成；当前表覆盖用户最常接触的设置。
