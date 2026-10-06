# 配置文件参考

把一次已经跑通的命令保存为 TOML，就能在本机、CI 和批量任务里复用同一套运行设置。先从下面的本地示例开始：它把输出留在 Stage，方便检查后再决定是否写回项目。

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

## Daemon 配置 {#daemon}

`pvisor-daemon serve` 使用命令行设置，不读取这里的原生 `RunConfig` TOML 或已解析 `RunSpec`。`OPEN_SANDBOX_API_KEY` 使用至少 32 字节的受保护秘密。见 [daemon 启动](../guides/daemon/index.md#start)与 [Service 入口](../guides/daemon/service.md)。

| 选项 | 默认值 / 含义 |
| --- | --- |
| `--podman PATH` | 必需，可信 rootless Podman 可执行文件绝对路径 |
| `--listen ADDRESS` | `127.0.0.1:8080` |
| `--public-endpoint HOST:PORT` | 外部路由 authority，不带 scheme/path；反向代理、通配/零端口监听时必需 |
| `--state PATH` | `.pvisor/daemon`；使用私有持久目录 |
| `--max-sandboxes N` | 32 个本机沙箱 |
| `--cpu-millis N` | 4000；准入硬 CPU 限制总和，以千分之一 CPU 为单位 |
| `--memory-bytes N` | 8589934592；准入硬内存限制总和，不是整机物理内存 |
| `--max-timeout-seconds N` | 86400；创建 TTL 上限，可配置范围 60 秒至一年 |

旧 `[controller]`、`[[workers]]`、Worker profile 和 Cluster task JSON 都不是 daemon 输入。原生 node/cache/memory-pool 配置独立保留。daemon 没有选择原生 VM、checkpoint/fork、stage/apply、全局 DAG 或分布式 lease 的选项。

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
- 重复列表选项通常替换整份配置列表；`--mount` 替换整份挂载列表，`--access` 追加规则，`--clear-access` 才清空默认与配置规则。
- `--safe` 位于配置与显式 CLI 之间，并清空配置的 `run.pass_env`；之后显式的 `--pass-env` 生效。
- 未知字段被拒绝；`filesystem = "sandbox"` 不能写成 `[filesystem] mode = "sandbox"`。
- 含路径的配置没有“以配置文件目录为根”的通用承诺；从预期工作区启动，跨环境使用绝对路径。


`[vm].memory_pool` 是实验性 macOS / Apple Silicon 共享冷页池的 socket 路径，默认未设置；CLI 对应 `--vm-memory-pool SOCKET`，Rust SDK 对应 `VmSettings.memory_pool`。见[首版内存共享](../design/memory-optimization/proof-of-concept.md#v1-integration)。

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
| `vm` 可选字段 | `rootfs`、`image`、`image_store`、`library_dir`、`ram_backing`、`memory_pool`、`node_socket`、`snapshot_filesystem_pool` |

`container.platform` 取 `linux-amd64` 或 `linux-arm64`；`container.network` 取 `host`、`bridge` 或 `none`。Linux container 的注入二进制必须与 rootfs 的架构和 ABI 匹配。

VM 的内存以 MiB 为单位，CPU 是正整数。`ram_backing` 保存 RAM 文件；`ram_compression` 启用相应的压缩 backing。macOS 的压缩 backing 与共享池有额外 FUSE 条件，见[内存共享概念验证](../design/memory-optimization/proof-of-concept.md)。

`[vm].snapshot_filesystem_pool` 为首次启动及从快照恢复的 VM capture 启用不可变 lower 引用，也覆盖不同 VM 的首次 capture。首次 seal 后，控制连接持有已验证的 owner；后续 capture 验证完整原 lower 并复用已封存的 pool 树，不扩大 runner 的访问范围。原生调用方应使用宿主管理的绝对路径，与 Job store 位于同一卷，并处于所有 VM 可写根和快照 store 之外。首次缓存未命中时，每个不可变摘要创建一棵 pool 树；并发未命中按摘要串行，命中不产生临时 lower 副本。此选项启用的原生 v5 快照以独立持有的 64 KiB 压缩块保留私有文件内容，复用未变化的内容；恢复时重建私有可写 inode，并保留完整元数据及硬链接关系。运行中的块 owner 在父快照退役和 GC 后仍然有效。封存先按解码后的内容标识查找 pool 块，命中时完整校验并直接复用，仅未命中才压缩；完整 RAM 压缩封存也使用这一路径。原生 capture 直接编码经过宿主认证的冻结私有目录，不再产生中间私有数据树；导入、恢复和暂停任务的文件导出均不打开记录中的原始私有路径。完整数据校验仍保留；延迟和密度收益需要实测。此配置不支持网络、共享内存池、普通 RAM 压缩及显式 RAM backing。备份须保留 pool 与相关 Job store，或导出完整快照。

### 捕获与记录

`[gateway]` 默认 `mode = "off"`，捕获模式为 `capture`。默认 `admin_listen = "127.0.0.1:9876"`、`level = "dialogue"`、`session_header = "x-pvisor-session-id"`、`debug = false`、`routes = []`。可选 `profile` 当前为 `zcode-bigmodel`；`zcode_builtin_config` 指向该适配配置文件。路由与捕获级别按 [Gateway 指南](../guides/capture.md)配置。

`[record].destination` 是可选路径。Gateway 的模型通信记录与 Trace Event journal 有不同数据职责，按[记录概念](../concepts/jobs.md)保留所需产物。

## 完整配置字段表 {#all-fields}

每行列出一个可序列化字段、Rust 类型、TOML/SDK 原始默认值与用途。`Option<T>` 表示可选，TOML 中省略键即可，不写 `null`；`Vec<T>` 表示数组，结构体使用表。`required` 指创建数组条目时必须提供。表中的默认值在 CLI 预设与解析之前生效。

下方的 `network_rule`、`bandwidth_limit`、`policy_layer`、`network_layer` 是复用的条目结构，不是顶层 TOML 表名。网络规则放在 `overlaynet.rules`、`overlaynet.deny` 或某个作用域的 network 层中；带宽限制放在对应的 `limits` 数组。`policies.session`、`policies.workspace`、`policies.user` 均使用 `policy_layer`。

文档构建会对照 Rust serde 结构检查字段名与类型。默认值和 CLI 行为单独核查，字段覆盖检查不能替代行为验证。

<!-- config-fields:start -->
| TOML 路径 / 条目字段 | Rust 类型 | 默认值 | 用途 |
| --- | --- | --- | --- |
| `run` | `RunSettings` | `{}` | 命令与进程设置 |
| `container` | `ContainerSettings` | `{}` | 选择 OCI 执行器时使用 |
| `vm` | `VmSettings` | `{}` | VM 执行器设置 |
| `filesystem` | `FilesystemMode` | `"host"` | `host` 或 `sandbox`；访问控制独立于暂存 |
| `overlayfs` | `Option<OverlayFsSettings>` | `未设置` | 省略时直接写入；即使是空表也请求暂存 |
| `overlaynet` | `OverlayNetSettings` | `{}` | 网络驱动与基础策略 |
| `gateway` | `GatewaySettings` | `{}` | 模型流量捕获与路由 |
| `record` | `RecordSettings` | `{}` | Trace Event journal 目的地 |
| `policies` | `pvisor_core::SessionPolicies` | `{}` | 额外的 session、workspace、user 约束 |
| `run.agent` | `String` | `"agent"` | `--name`；保持默认时 CLI 取命令名 |
| `run.executor` | `RunExecutorKind` | `"host"` | `--executor`：host、container、vm |
| `run.timeout_ms` | `Option<u64>` | `未设置` | 实际经过时间，毫秒；`--timeout 30s` |
| `run.stdio` | `RunStdio` | `"inherit"` | inherit 或 capture；`--stdio` |
| `run.policy` | `RunPolicy` | `"observe"` | observe 或 enforce；`--strict` 选择 enforce |
| `run.inherit_env` | `bool` | `true` | TOML/SDK 原始默认值；CLI 仅对命令名 codex 设为 true |
| `run.pass_env` | `Vec<String>` | `[]` | 环境变量名；`--pass-env KEY` 替换列表 |
| `run.filesystem` | `Vec<FilesystemCapability>` | `[]` | 工作区以外的宿主权限；条目字段见下方 |
| `run.resource_limits` | `ResourceLimits` | `{}` | 请求额度；运行后对照 Bundle 的有效额度 |
| `run.command` | `Vec<String>` | `[]` | 参数数组；`--` 后命令替换它；不自动经 shell 展开 |
| `container.runtime` | `PathBuf` | `"crun"` | OCI runtime 程序；`--container-runtime` |
| `container.image` | `String` | `""` | OCI 镜像引用；`--container-image` |
| `container.rootfs` | `Option<PathBuf>` | `未设置` | 已有 rootfs，替代镜像；`--container-rootfs` |
| `container.pvisor_binary` | `Option<PathBuf>` | `未设置` | 注入的 Linux 程序，默认当前程序；`--container-pvisor-binary` |
| `container.platform` | `Option<ContainerPlatform>` | `未设置` | linux-amd64 或 linux-arm64；`--container-platform` |
| `container.network` | `ContainerNetwork` | `"host"` | host、bridge、none；`--container-network` |
| `container.workdir` | `Option<PathBuf>` | `未设置` | 未挂载 Run cwd 时的容器目录；`--container-workdir` |
| `container.user` | `Option<String>` | `未设置` | uid、uid:gid 或用户名；`--container-user` |
| `container.read_only_rootfs` | `bool` | `false` | 镜像根目录只读；`--container-read-only-rootfs` |
| `container.mounts` | `Vec<ContainerMount>` | `[]` | bind mounts；重复 `--container-mount` 替换列表 |
| `container.mounts[].source` | `PathBuf` | `必需` | 宿主路径 |
| `container.mounts[].target` | `PathBuf` | `必需` | 容器路径 |
| `container.mounts[].read_only` | `bool` | `false` | 只读 bind mount |
| `vm.ram_backing` | `Option<PathBuf>` | `未设置` | 新建 RAM backing 路径；拒绝已有文件；`--vm-ram-backing` |
| `vm.ram_compression` | `bool` | `false` | Seekable 压缩 backing；`--vm-ram-compression` |
| `vm.memory_pool` | `Option<PathBuf>` | `未设置` | 实验性 macOS pool socket；`--vm-memory-pool` |
| `vm.snapshot_filesystem_pool` | `Option<PathBuf>` | `未设置` | 宿主管理的不可变快照 lower 池；仅 Linux x86-64 无网络私有 RAM 配置；通过配置或 SDK 设置 |
| `vm.node_socket` | `Option<PathBuf>` | `未设置` | 同宿主 node 资源服务 socket；恢复时保留共享只读 RAM backing 的引用，直到 native VM 退出；通过配置或 SDK 设置 |
| `vm.rootfs` | `Option<PathBuf>` | `未设置` | Linux 根目录；Linux CLI 默认宿主 `/`；`--rootfs` |
| `vm.image` | `Option<String>` | `未设置` | OCI 镜像，替代 rootfs 目录；`--rootfs IMAGE` |
| `vm.image_store` | `Option<PathBuf>` | `未设置` | OCI 缓存路径；`--vm-image-store` |
| `vm.rootfs_immutable` | `bool` | `false` | 拒绝将变更 apply 到 rootfs lower |
| `vm.library_dir` | `Option<PathBuf>` | `未设置` | 固件目录；musl 内嵌固件并拒绝此参数；`--vm-library-dir` |
| `vm.memory_mib` | `u32` | `2048` | guest RAM，MiB；CLI `--memory` 还设置进程额度 |
| `vm.cpus` | `u16` | `2` | vCPU 数量；`--cpu` |
| `overlayfs.mount` | `Vec<FilesystemMount>` | `[]` | 统一挂载；重复 `--mount` 替换列表 |
| `overlayfs.access` | `Vec<FilesystemAccessRule>` | `[]` | 访问规则；重复 `--access` 追加；`--clear-access` 清空 |
| `overlayfs.stage` | `Option<PathBuf>` | `未设置` | 项目外的新持久 Stage；`--stage` |
| `overlayfs.durability` | `pvisor_core::overlay::StageDurability` | `"checkpoint"` | `checkpoint` 或 `strict`；`--stage-durability`；不改变隔离和内容冲突检测 |
| `overlayfs.max_size` | `Option<u64>` | `未设置` | 暂存总字节额度；`--overlayfs-max-size` |
| `overlayfs.mount[].source` | `PathBuf` | `必需` | 宿主源路径 |
| `overlayfs.mount[].target` | `Option<PathBuf>` | `未设置` | Agent 可见的挂载路径；省略时由规范化过程补充 |
| `overlayfs.mount[].access` | `FilesystemAccessLevel` | `必需` | deny、ask、read、warn、stage、write |
| `overlayfs.access[].path` | `String` | `必需` | Agent 可见路径/规则；语法见文件策略指南 |
| `overlayfs.access[].level` | `FilesystemAccessLevel` | `必需` | deny、ask、read、warn、stage、write |
| `overlaynet.mode` | `OverlayNetMode` | `"auto"` | auto、off、proxy；`--overlaynet`；auto 在 VM 中使用 smoltcp |
| `overlaynet.listen` | `String` | `"127.0.0.1:19081"` | 代理地址；CLI 将此默认值换成空闲端口；`--overlaynet-listen` |
| `overlaynet.policy` | `OverlayNetPolicy` | `"public"` | public、deny、allowlist；`--overlaynet-policy` |
| `overlaynet.allow` | `Vec<String>` | `[]` | 兼容的目标字符串授权；优先使用结构化 rules |
| `overlaynet.rules` | `Vec<NetworkAccessRule>` | `[]` | 结构化授权；字段见 network_rule；`--overlaynet-rule` 替换 |
| `overlaynet.deny` | `Vec<NetworkAccessRule>` | `[]` | 结构化拒绝；`--overlaynet-deny` 替换 |
| `overlaynet.limits` | `Vec<NetworkBandwidthLimit>` | `[]` | 带宽条目；字段见 bandwidth_limit；`--overlaynet-limit` 替换 |
| `gateway.mode` | `GatewayMode` | `"off"` | off 或 capture；`--gateway-mode` |
| `gateway.profile` | `Option<GatewayProfile>` | `未设置` | zcode-bigmodel；`--gateway-profile` 同时选择 capture |
| `gateway.zcode_builtin_config` | `Option<PathBuf>` | `未设置` | Zcode 适配配置路径 |
| `gateway.admin_listen` | `String` | `"127.0.0.1:9876"` | 管理地址；CLI 将此默认值换成空闲端口；`--gateway-admin-listen` |
| `gateway.level` | `CaptureLevel` | `"dialogue"` | summary、dialogue、full；`--gateway-level` |
| `gateway.session_header` | `String` | `"x-pvisor-session-id"` | Session 关联头；`--gateway-session-header` |
| `gateway.debug` | `bool` | `false` | Gateway 调试；`--gateway-debug` |
| `gateway.routes` | `Vec<ModelRoute>` | `[]` | 模型路由条目；`--gateway-route` 替换列表 |
| `record.destination` | `Option<PathBuf>` | `未设置` | Trace Event 文件/目录；`--record-destination` |
| `run.resource_limits.memory_bytes` | `Option<u64>` | `未设置` | 字节；`--memory` |
| `run.resource_limits.processes` | `Option<u64>` | `未设置` | 进程/线程数量；`--max-processes` |
| `run.resource_limits.cpu_time_ms` | `Option<u64>` | `未设置` | CPU 毫秒；`--max-cpu-time`；独立于任务超时 |
| `run.resource_limits.open_files` | `Option<u64>` | `未设置` | 文件描述符数；`--max-open-files` |
| `run.resource_limits.file_size_bytes` | `Option<u64>` | `未设置` | 单文件字节；`--max-file-size` |
| `run.filesystem[].path` | `String` | `必需` | 暂存项目以外的宿主路径 |
| `run.filesystem[].access` | `FilesystemAccess` | `必需` | read 或 read_write |
| `network_rule.host` | `String` | `必需` | 主机名、通配后缀、IP 或 CIDR；不带 URL scheme |
| `network_rule.ports` | `Vec<u16>` | `[]` | 1–65535 端口；空表示所有端口 |
| `network_rule.transports` | `Vec<NetworkTransport>` | `[]` | http、https、tcp_tunnel；空表示所有协议 |
| `network_rule.allow_private_ips` | `bool` | `false` | 允许主机名解析到私网/回环地址 |
| `bandwidth_limit.host` | `Option<String>` | `未设置` | 省略匹配所有拦截目标 |
| `bandwidth_limit.port` | `Option<u16>` | `未设置` | 省略匹配所有端口 |
| `bandwidth_limit.bytes_per_second` | `u64` | `必需` | 正的字节速率；匹配的限制叠加 |
| `gateway.routes[].name` | `String` | `必需` | 模型匹配：精确值、prefix*、*suffix、* |
| `gateway.routes[].provider` | `Option<String>` | `未设置` | openai、anthropic、gemini、vertex、bedrock、azure、copilot、custom |
| `gateway.routes[].upstream` | `Option<String>` | `未设置` | 包含 /v1 等前缀的上游 API base |
| `gateway.routes[].upstream_anthropic` | `Option<String>` | `未设置` | Anthropic API base；省略时用 upstream |
| `gateway.routes[].api_key_env` | `Option<String>` | `未设置` | 宿主密钥变量名；避免将密钥明文写入 TOML |
| `gateway.routes[].api_key` | `Option<String>` | `未设置` | 明文上游密钥；配置需私密保存 |
| `gateway.routes[].forward` | `Option<String>` | `未设置` | 转发到精确路由名，并改写 model |
| `policies.session` | `PolicyLayer` | `{}` | 当前 Session 的额外策略层 |
| `policies.workspace` | `PolicyLayer` | `{}` | 显式策略或 .pvisor/policy.toml 默认值 |
| `policies.user` | `PolicyLayer` | `{}` | 显式策略或用户配置默认值 |
| `policy_layer.network` | `Option<NetworkPolicyLayer>` | `未设置` | 可选 network_layer 表；省略不增加该层约束 |
| `policy_layer.filesystem` | `Option<FileAccessPolicy>` | `未设置` | 可选 deny/ask/warn/allow glob 数组；见策略参考 |
| `network_layer.default_action` | `Option<NetworkDefaultAction>` | `未设置` | allow 或 deny；存在此层时，省略会拒绝未匹配目标 |
| `network_layer.allow` | `Vec<NetworkAccessRule>` | `[]` | network_rule 授权条目 |
| `network_layer.deny` | `Vec<NetworkAccessRule>` | `[]` | network_rule 拒绝条目；拒绝优先 |
| `network_layer.limits` | `Vec<NetworkBandwidthLimit>` | `[]` | bandwidth_limit 条目 |
<!-- config-fields:end -->
