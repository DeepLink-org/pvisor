# 统一服务入口与节点共享资源

`pvisor service` 用一份 TOML 管理 Controller、节点资源服务、Worker 和可选 cold RAM pool。角色仍独立运行：重启 Controller 不会顺带结束 Worker、只读 backing owner 或 pool 会话。旧 `pvisor-cluster`、`pvisor-worker` 和 cache 命令继续可用。

本页先搭建受限资源的可信 host 示例；它不提供沙箱隔离。VM 使用[现有 VM 指南](vm-and-gateway.md)的环境、任务与资源合同，再接入本页节点服务。

## 构建与验证 {#verify}

在 Linux x86-64、可用的 systemd 用户 manager 和 cgroup v2 下，从仓库根目录构建：

```bash
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 just service-build
```

构建包括 `pvisor`、`pvisor-cache`、`pvisor-cluster`、`pvisor-worker` 和 `pvisor-memory-pool`，包含可选 Gateway。制品须放在同一可信安装目录；service 不从 PATH 寻找 companion。

专用验收运行真实 HTTP/Unix 服务、FUSE RAM backing 和 delegated cgroup，不启动 guest VM：

```bash
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 NEXTEST_TEST_THREADS=1 just test-service
```

缺少 FUSE 或用户 manager 时明确失败，不把模拟或跳过当作成功。这个 gate 检查 Controller 独立重启、相同执行身份、跨 store 的同一 RAM inode、私有 COW 写入、active pin 停止保护，以及实际内核 CPU/内存/swap 限额；另以代理提前退出验证失败 fixture 的 unit 清理。它不替代原生 VM 启动/恢复的硬件验收或 S1–S5 性能实验。

另一个 gate 验证两个真实 KVM guest，各 128 MiB/1 vCPU、独立 Worker cgroup 512 MiB/0.5 核，服务 unit 总限 2 GiB/1 核。需要 `/dev/kvm`、FUSE 以及已安装的匹配 firmware，显式设置目录：

```bash
export PVISOR_TEST_LIBKRUNFW_DIR="$HOME/.cache/pvisor/firmware/5.5.0/linux-x86_64"
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 NEXTEST_TEST_THREADS=1 \
  PVISOR_TEST_LIBKRUNFW_DIR="$PVISOR_TEST_LIBKRUNFW_DIR" just test-service-vm
```

此 gate 检查两个 Worker 的环境 pin 共用一个 owner、私有 upper 写入隔离、Controller 重启后正确完成原任务，并在执行后检查角色限额和零 OOM/OOM-kill。它测的是共享环境，不是 restored VM RAM 的物理共享曲线。firmware 版本/安装路径不同则替换目录，缺少依赖会失败。

2026-10-05 Linux x86-64 实跑：四个无 guest gate 和两个真实 guest 的专用 gate 均通过。样例 TOML 的管理流程也已验证，仅替换私有 state、随机端口与 unit 名，实际 Controller/Node/Worker 限额分别为 128/256/512 MiB、0.25/0.5/0.5 核，零 swap、零 OOM/OOM-kill。此记录证明部署与正确性，不代表 S1–S5 性能收益。

## Service 工具命名空间 {#tools}

集群客户端/Controller、Worker、cache 与 pool 统一放在 service 下，原参数直接跟在工具名后。`run/status/restart/stop` 管理一套部署，工具子命令操作对应协议或独立角色；资源生命周期仍由所属组件负责。

```bash
pvisor service cluster --help
pvisor service worker --help
pvisor service cache --help
pvisor service memory-pool --help
pvisor help service cluster submit
```

## 启动与管理 {#launch}

样例位于 `examples/cluster/service.toml`。路径相对 TOML；`node.state` 相对服务 state，`node.socket` 相对 node state。默认服务 state 为 checkout 的 `.pvisor/services`；每套部署必须独占 state，端口 19800 也必须空闲。

```bash
export PVISOR_CLUSTER_TOKEN=$(openssl rand -hex 24)
export PVISOR_CLUSTER_WORKER_TOKEN=$(openssl rand -hex 24)

systemd-run --user --unit=pvisor-service-demo --collect \
  -p Delegate=yes -p MemoryMax=2G -p MemorySwapMax=0 -p CPUQuota=200% \
  --setenv=PVISOR_CLUSTER_TOKEN --setenv=PVISOR_CLUSTER_WORKER_TOKEN \
  --working-directory="$PWD" \
  "$PWD/target/debug/pvisor" service run --config "$PWD/examples/cluster/service.toml"

target/debug/pvisor service status --config examples/cluster/service.toml
target/debug/pvisor service restart --config examples/cluster/service.toml controller
```

`cgroup_root = ":self:"` 要求专用 delegated cgroup。Supervisor 将自己移入 `supervisor` 子组，再为每个角色创建子组，在 exec 前安装 `memory.max`、`memory.swap.max=0` 和 `cpu.max`。权限/控制器不足就失败；不会取消限额重试。

也可指定已委托的独占绝对 cgroup 路径。删除 `cgroup_root` 仅用于显式接受无内核硬封顶的本地 preview；此时 status 的 `kernel_limits` 为 `false`。`resources` 预留和 cache payload 上限都不是整组内存硬限制。

样例只有一个 Worker、一个槽位：Controller 限 128 MiB/0.25 核，节点服务 256 MiB/0.5 核，Worker 全进程树 512 MiB/0.5 核；整个 unit 另限 2 GiB。任务仍需按原指南设置内存、CPU 时间、输出和墙钟限制。VM 用 128 MiB/1 vCPU，并保持全会话最多四台，源 VM 也计入。

停止一个角色或整套服务：

```bash
target/debug/pvisor service stop --config examples/cluster/service.toml --role controller
target/debug/pvisor service restart --config examples/cluster/service.toml controller
target/debug/pvisor service stop --config examples/cluster/service.toml
```

整套 stop 先排空并停止 Worker，再停止 pool、节点 owner 和 Controller。角色未在 30 秒内完成排空时返回错误并保留数据 owner；不会为了退出入口强杀它们。pool 收到停止信号后拒绝新会话，等待已有会话断开。不要对仍有依赖 VM 的 pool 做强制升级或 SIGKILL。

## 接入环境与恢复 backing {#owners}

在 `[node]` 设置 `cache_backend = "filesystem"` 或 `"s3"`，以及已经发布的 `cache_location`；Worker profile 保持 `[environments] enabled = true`。服务管理的 Worker 自动获得 `--node-socket`，环境层由节点服务按不可变身份共享挂载，每个任务仍使用私有 upper。FS/S3 native cache 不需要额外 cache daemon；OCI 准备/发布仍沿用原 cache 命令。

恢复通过 `vm.node_socket` 进入节点 RAM owner；CLI 管理 Worker 时自动配置，独立 SDK/profile 可显式设置。每次 acquire 都检查发布内容与兼容性，相同封存 ID 在多个授权 store 中复用一个只读 inode；guest 仍用 `MAP_PRIVATE`，修改不会传给其他分支。

托管 Worker state 自动加入允许的 snapshot roots。共享检查点目录或 `snapshot_filesystem_pool` 位于其他目录时，必须在 `[node].snapshot_roots` 显式授权绝对路径。节点服务不会因为 caller 提供路径就打开任意 store。

连接 pin 保留到正常 native teardown。Worker 异常退出/连接丢失会释放会话引用；节点 owner 丢失可能使后续 FUSE fault 失败，尚不支持 live 重连/接管。重启 Controller 的独立故障合同不意味着 Worker 或节点服务故障也能无损恢复。

## 预算、保温与观察 {#budget}

| 配置 | 限制 |
|---|---|
| `max_owners` / `warm_owners` | 环境与 RAM owner 共用总数量；有界强引用保温，压力下仅释放 idle owner |
| `max_sessions` | 活动 pin 连接数；stats 不占 pin 配额 |
| `max_preparations` | 同时下载/校验/准备 owner 的数量；相同 key 的 mount 创建串行去重 |
| `max_cache_bytes` | 同进程镜像热块、分页 metadata cache、Linux RAM decoded cache 的 retained payload 总量 |
| `limits."ROLE"` | 有 delegated cgroup 时，该角色及所有子进程的内核内存/CPU 封顶 |

缓存额度不足时仍可读取并校验，只是不保留新的热数据。Cache payload 计数不包含完整 metadata、临时读取/解码 buffer、外部 Arc 持有者和 kernel page cache；这些由整组内核限额与观察约束。macOS 独立 RAM pager 的局部缓存不并入 Node 进程的计数，支持范围仍需单独验收。

status 展示角色 PID、退出状态、`kernel_limits`、节点 active pins/live owners、cache bytes/limit/misses。`cache_misses` 计数的是预算申请被拒，不是 origin 读取 miss；重试也会增加计数。Worker 的 readiness 表示进程已启动；是否注册仍用 Controller 的 workers API 确认。日志位于 `STATE/logs/`；启动时保存解析后的 `active-config.toml`，角色重启沿用它，修改源配置后需停止并重新 run。

多机部署在节点配置中省略 `[controller]`，给 Worker 指定 `url`；控制节点以 `[node] enabled = false` 关闭节点角色。可选 `[pool]` 只在 Apple Silicon 启用，由单独进程服务 VM Worker，保持其原会话引用与 fail-stop 合同。

这次整合没有新增逐页 WAL，没有实现自动预取、动态 cache 调度 hint、跨节点物理 RAM 共享或 pool 无损迁移。架构与后续验收见[服务整合设计](../../design/cluster/server-consolidation.md)。
