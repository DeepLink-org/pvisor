---
status: todo
search:
  exclude: true
---

# 配置文件参考

!!! warning "规划中"
    完整字段表尚未完成；现有信息见 CLI 参考的[一套配置模型](cli.md)。

## 要回答的问题

`pvisor run --config run.toml` 支持哪些字段，每个字段的类型、默认值、对应的 CLI 参数和覆盖规则是什么？

## 需求

- 从 `RunConfig` 的 serde 定义**自动生成**字段表，不手写，避免与代码漂移；
- 每个字段列出：TOML 路径、类型、默认值、对应 CLI 参数、标量替换还是列表追加；
- 标出当前不按配置值生效的字段（例如部分 CLI 路径上的 `run.inherit_env`）。

## 验收标准

- `just docs-build` 时生成，或由 CI 检查生成结果与代码一致；
- 覆盖 `[run]`、`filesystem`、`[overlayfs]`、`[overlaynet]`、`[gateway]`、`[record]`、`[policies.*]`、`[container]`、`[vm]`。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[CLI 参考](cli.md)、[策略字段参考（规划中）](policy.md)

## 当前可用的配置入口

`--config` 读取 TOML `RunConfig`；`--spec` 读取已解析的 JSON `RunSpec`，两者不是同一种文件。配置必须显式传入，不会自动读取项目里的 `run.toml`。工作区 `policy.toml` 则会自动加载，见[策略字段](policy.md)。

下面的配置在宿主执行一个不需要网络的任务，捕获标准输出，并把改动保留在项目外：

```toml
filesystem = "sandbox"

[run]
executor = "host"
command = ["/bin/sh", "-c", "printf 'ready\n' > result.txt"]
timeout_ms = 30000
stdio = "capture"

[overlayfs]
stage = "../stage-config-001"
max_size = 67108864

[overlaynet]
mode = "proxy"
policy = "deny"
```

```bash
pvisor run --config run.toml
pvisor status --review ../stage-config-001
pvisor inspect ../stage-config-001 -- cat result.txt
```

`proxy` 的 deny 策略只约束经过代理的流量；这个例子不等价于强制离线。需要普通出口不可绕过地被阻止时，加上 `--overlaynet-deny-all`。暂存与文件 sandbox 也相互独立。

## 字段导航（当前实现）

| TOML 路径 | 类型与默认值 | CLI / 用途 |
| --- | --- | --- |
| `filesystem` | `host`（默认）/ `sandbox` | `--filesystem`；它是顶层字符串，不是 `[filesystem]` 表 |
| `run.executor` | `host`（默认）/ `container` / `vm` | `--executor` |
| `run.command` | 字符串数组，默认空 | `--` 后的命令；实际运行必须提供 |
| `run.agent` | 字符串，`agent` | `--name` |
| `run.timeout_ms` | 可选整数，毫秒 | `--timeout`；CLI 接受时长字符串 |
| `run.stdio` | `inherit` / `capture` | `--stdio` |
| `run.policy` | `observe` / `enforce` | `--strict` 请求强制准入 |
| `run.inherit_env` / `run.pass_env` | 布尔值 / 字符串数组 | CLI 会按命令适配与 safe 规则重新解析环境继承；不要仅凭 TOML 判断凭据是否传入 |
| `run.resource_limits` | 可选整数额度 | `memory_bytes`、`processes`、`cpu_time_ms`、`open_files`、`file_size_bytes`；以有效额度证据为准 |
| `overlayfs.stage` / `max_size` | 可选路径 / 字节数 | `--stage` / `--overlayfs-max-size`；存在 `[overlayfs]` 即请求暂存 |
| `overlayfs.mount` / `access` | 表数组 | `--mount` / `--access`；规则语法见[文件策略](../guides/policies/files.md) |
| `overlaynet` | 网络模式、策略和规则 | `mode` 默认 `auto`，`listen` 默认 `127.0.0.1:19081`，`policy` 默认 `public` |
| `gateway` | 可选捕获与路由 | `mode` 默认 `off`；capture 需要编译 Gateway feature |
| `record.destination` | 可选路径 | `--record-destination`；文件或目录 |
| `policies.session/workspace/user` | 策略层 | 不同层取最严格约束 |
| `container` | OCI 参数 | runtime 默认 `crun`、network 默认 `host`；image/rootfs 二选一 |
| `vm` | VM 参数 | `memory_mib = 2048`、`cpus = 2`；macOS 必须提供 Linux rootfs/image |

类型与默认值来自 `crates/pvisor/src/config.rs`。这张导航表不替代仍待生成的完整字段表；标为内部解析或 `serde(skip)` 的字段不是配置接口。

## 覆盖规则与常见错误

- 显式 CLI 标量覆盖配置值；命令替换 `run.command`。
- 重复列表选项通常替换整份配置列表；`--mount` 与 `--access` 是追加，`--clear-access` 才清空默认与配置规则。
- `--safe` 位于配置与显式 CLI 之间，并清空配置的 `run.pass_env`；之后显式的 `--pass-env` 生效。
- 未知字段被拒绝；`filesystem = "sandbox"` 不能写成 `[filesystem] mode = "sandbox"`。
- 含路径的配置没有“以配置文件目录为根”的通用承诺；从预期工作区启动，跨环境使用绝对路径。


`[vm].memory_pool` 是实验性 macOS / Apple Silicon 共享冷页池的 socket 路径，默认未设置；CLI 对应 `--vm-memory-pool SOCKET`，Rust SDK 对应 `VmSettings.memory_pool`。见[首版内存共享](../design/memory-sharing/index.md#v1-integration)。
