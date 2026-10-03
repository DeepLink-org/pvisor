# 配置文件参考

把一次已经跑通的命令保存为 TOML，就能在本机、CI 和批量任务里复用同一套运行设置。先从下面的离线示例开始：它把输出留在 Stage，方便检查后再决定是否写回项目。

需要查命令行参数时，使用 [CLI 参考](cli.md)；需要限制具体文件或网络目标时，使用[策略字段参考](policy.md)。

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

类型与默认值来自 `crates/pvisor/src/config.rs`。标为内部解析或 `serde(skip)` 的字段不是配置接口。

## 覆盖规则与常见错误

- 显式 CLI 标量覆盖配置值；命令替换 `run.command`。
- 重复列表选项通常替换整份配置列表；`--mount` 与 `--access` 是追加，`--clear-access` 才清空默认与配置规则。
- `--safe` 位于配置与显式 CLI 之间，并清空配置的 `run.pass_env`；之后显式的 `--pass-env` 生效。
- 未知字段被拒绝；`filesystem = "sandbox"` 不能写成 `[filesystem] mode = "sandbox"`。
- 含路径的配置没有“以配置文件目录为根”的通用承诺；从预期工作区启动，跨环境使用绝对路径。


`[vm].memory_pool` 是实验性 macOS / Apple Silicon 共享冷页池的 socket 路径，默认未设置；CLI 对应 `--vm-memory-pool SOCKET`，Rust SDK 对应 `VmSettings.memory_pool`。见[首版内存共享](../design/memory-sharing/index.md#v1-integration)。

## 常用分组的具体字段 {#settings}

### Run 与资源额度

`[run]` 的 `command` 是字符串数组，不经 shell 自动展开。需要管道或重定向时显式使用 `/bin/sh -c`。`executor` 默认 `host`，`stdio` 默认 `inherit`，`policy` 默认 `observe`，`timeout_ms` 默认未设置。

`[run.resource_limits]` 接受以下可选整数；未设置表示不在配置中请求该额度。

| 字段 | 单位 | CLI |
| --- | --- | --- |
| `memory_bytes` | 字节 | `--memory` |
| `processes` | 进程/线程数量 | `--max-processes` |
| `cpu_time_ms` | 毫秒 CPU 时间 | `--max-cpu-time` |
| `open_files` | 文件描述符数量 | `--max-open-files` |
| `file_size_bytes` | 单文件字节数 | `--max-file-size` |

额度能否落实取决于执行器。运行后核对 `resources` 中的 requested、effective、mechanisms 与 limitations；CPU 时间与任务 wall-time 超时分别设置。

### 文件与网络

`[overlayfs]` 的 `mount` 与 `access` 使用表数组。mount 提供 `source`、可选 `target` 与必需的 `access`；access 规则提供 `path` 与 `level`。等级取 `deny`、`ask`、`read`、`warn`、`stage`、`write`，使用方法见[文件策略](../guides/policies/files.md)。

`[overlaynet]` 的 `mode` 取 `auto`、`off`、`proxy`，`policy` 取 `public`、`deny`、`allowlist`。`allow` 是目标字符串数组，`rules`、`deny`、`limits` 是结构化表数组。默认列表为空；端口、协议与地址规则见[策略字段](policy.md)。

### Container 与 VM

| 表 | 字段及默认值 |
| --- | --- |
| `container` | `runtime = "crun"`，`image = ""`，`network = "host"`，`read_only_rootfs = false`，`mounts = []` |
| `container` 可选字段 | `rootfs`、`pvisor_binary`、`platform`、`workdir`、`user` |
| `container.mounts` 每项 | `source`、`target`，`read_only = false` |
| `vm` | `memory_mib = 2048`，`cpus = 2`，`rootfs_immutable = false`，`ram_compression = false` |
| `vm` 可选字段 | `rootfs`、`image`、`image_store`、`library_dir`、`ram_backing`、`memory_pool` |

`container.platform` 取 `linux-amd64` 或 `linux-arm64`；`container.network` 取 `host`、`bridge` 或 `none`。Linux container 的注入二进制必须与 rootfs 的架构和 ABI 匹配。

VM 的内存以 MiB 为单位，CPU 是正整数。`ram_backing` 保存 RAM 文件；`ram_compression` 启用相应的压缩 backing。macOS 的压缩 backing 与共享池有额外 FUSE 条件，见[内存共享设计](../design/memory-sharing/index.md)。

### 捕获与记录

`[gateway]` 默认 `mode = "off"`，捕获模式为 `capture`。默认 `admin_listen = "127.0.0.1:9876"`、`level = "dialogue"`、`session_header = "x-pvisor-session-id"`、`debug = false`、`stream_markdown = false`、`routes = []`。可选 `profile` 当前为 `zcode-bigmodel`；`zcode_builtin_config` 指向该适配配置文件。路由与捕获级别按 [Gateway 指南](../guides/capture.md)配置。

`[record].destination` 是可选路径。Gateway 的模型通信记录与 Trace Event journal 有不同数据职责，按[记录概念](../concepts/jobs.md)保留所需产物。
