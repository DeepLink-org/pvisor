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
| `PVISOR_BIN` | 已废弃的 Python 启动器覆盖；安装的原生脚本不读取它。直接调用二进制路径或通过 `PATH` 选择 |
| `XDG_CONFIG_HOME` | 用户策略根；未设置用 `~/.config` |
| `HOME` / `PATH` | 存储、工具发现与环境投影的宿主输入；safe 下 HOME 会重定向 |

缓存的端点语法、安全要求和失败行为见[共享镜像缓存](shared-image-cache.md)。不要把宿主读取变量与 Agent 可见变量混为一谈；显式投影用 `--pass-env NAME`。

## Daemon 凭据 {#daemon}

`OPEN_SANDBOX_API_KEY` 为独立 `pvisor-daemon serve` 提供生命周期 key。使用至少 32 字节的受保护随机秘密，不放在 argv 中，不投影给工作负载。客户端通过 `OPEN-SANDBOX-API-KEY` HTTP header 发送它；环境变量名称使用下划线，HTTP 名称使用连字符。

默认 daemon 端点在返回的 headers 中提供沙箱范围的 `X-PVISOR-SANDBOX-TOKEN`，客户端必须保留。它不是宿主环境设置，也不是生命周期凭据。见[端点认证](../guides/daemon/index.md#endpoints)。原生 cache/pool 凭据与 Job 环境投影独立保留；已退役的 Cluster token 不是 daemon 凭据。

## 运行时注入

`PVISOR_RUN_ID`、`PVISOR_RUNTIME`、`PVISOR_STORAGE`、`PVISOR_AGENT`、`PVISOR_ROLE` 标识运行环境。`PVISOR_AGENTCTL_ENDPOINT`、`PVISOR_AGENTCTL_TOKEN`、`PVISOR_AGENTCTL_TRANSPORT`、`PVISOR_AGENTCTL_VERSION` 用于可选的协作式 Guest AgentCtl 通道；token 仅是 Guest 凭据，不能授权 Host Job、VM 或 daemon supervisor 操作，不应写进日志。

启用代理时还会注入 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及小写形式。它们只为客户端指定代理，不单独提供不可绕过的网络隔离。完整投影以 Bundle 的 `environment` 变量名清单为准。

`PVISOR_KRUN_RUNNER_SPEC`、`PVISOR_KRUN_NETWORK_FD` 等是内部启动协议；`PVISOR_KRUN_LOG` 和 `PVISOR_KRUN_ENOMEM_WORKAROUND` 是 VM 诊断设置，不是稳定产品配置。使用上表配置宿主；内部变量留给启动器与诊断工具管理。

## 启动诊断与源码构建 {#build-and-diagnostics}

`PVISOR_STARTUP_TIMING` 默认启用启动阶段日志；设为 `0` 关闭，适合不带日志开销的测量。它只影响诊断输出，不改变任务策略。

`PVISOR_VM_FS_WRITEBACK` 控制 guest 写回：新建 staged VM 会话默认关闭，`1` 显式请求，`0` 关闭；未覆盖的恢复会话保留原协商模式，显式 `0` 拒绝已开启写回的快照。它与下方 host 写缓存互斥。本次新固件/宿主修复的验证范围为 Linux x86_64/KVM，不能据此外推 macOS。

协商 bit 42 的0048固件按实际脏页与临时 FUSE WRITE 页施加背压，不累计 write 字节。每连接/BDI 的软压力目标是内核实际 RAM 的1/8，截在16–256 MiB，最多保留256个句柄 lease；提交与完成使用独立队列。干净页缓存不计入目标，rootfs/workspace独立BDI可各有一份，Linux全局dirty/回收规则继续约束总量。原生BDI另尽力申请16 MiB最低份额，失败有警告，不是固定字节保证。

其上的0049将提前提交提示改为压力目标的1/4，截在1–16 MiB；小量写入仍由10 ms timer推进，协商的单次WRITE上限仍为1 MiB。入队在连接锁外只读近似脏页数，不重复逐folio背压检查，也不调度空pending；实际压力检查、完整性回写前后的两次WRITE屏障和错误传播保留。0049v2还在启动request virtqueue前预分配有界scatterlist元数据，按原VQ锁保护并复用；文件页与每请求argbuf保持原有完成期所有权，其他原子分配仍然存在。

0048历史证据与0049v2新批次分别保留。新固件已通过Linux x86_64/KVM的FULL/ERROR、7项固件合同、五次原压力历史复跑、独立N5曲线及普通CLI的WB0/1对照；正确性收据保留当时50项Python合同，后续CLI合同另有记录。CLI七工具整体与Ready未检出收益，新会话默认仍关闭。范围、逐工作负载结果与来源见[独立实验](../lab-notes/filesystem-io-performance-analysis.md#guest-writeback-tuning)。

在 guest 中读取 `/sys/module/fuse/parameters/pvisor_wb_budget_bytes` 可查看生效目标；这是只读诊断参数，不能经 sysfs 或 boot argument 设置。可写close可延迟完成，只读close保留原生FLUSH，fsync/O_SYNC和最终syncfs的错误边界保留。新宿主按FORGET保留unlink/rename覆盖后的upper对象引用或lower逻辑属性；仍有detached/orphan引用时snapshot返回Unsupported，旧快照未知计数的有限节点保守保留到DESTROY。完整Agent IOCP/offload仍未验收。

`PVISOR_VM_FS_HOST_WRITEBACK` 是默认关闭的实验性 host 写缓存：未设置或 `0` 关闭，`1` 启用，其他值拒绝。当前只支持 Linux staged virtio-fs、guest writeback 关闭和 DAX 关闭；每连接 64 MiB payload、1024 个在途项，相邻连续写按最多 1 MiB 或 10 ms 聚合。显式同步与退出仍观察后台错误；完整 Agent IOCP 和自动卸载尚未接通。范围与同步合同见 [EXP-003](../design/experimental/agent-io-completion/index.md#host-write-buffer)。

`PVISOR_VM_FS_HOST_WRITEBACK_CONFIG` 是私有工程调参 JSON：`capacity_mib` 为 4–256、`jobs` 为 1–4096、`batch_kib` 为 4–4096 且不超过容量、`chunks` 为 1–64、`delay_us` 为 0–50000。未设置沿用上述默认参数；未知、重复字段、错误类型和越界值拒绝。`qualification_lock` 和 `synchronous` 用于保留旧资格锁或等待 owned worker 完成的消融；不属于稳定产品设置。曲线和适用范围见[文件系统 I/O 实验](../lab-notes/filesystem-io-performance-analysis.md)。

下面两个变量影响 Linux x86_64 musl **构建时**嵌入的 guest 内核：

| 变量 | 输入 | 优先级 |
| --- | --- | --- |
| `PVISOR_KRUNFW_KERNEL_BUNDLE` | 含 `kernel.bin` 与 `kernel.json` 的目录 | 设置时优先使用 |
| `PVISOR_KRUNFW_PATH` | 要提取内核的 `libkrunfw.so.5` 文件 | 未设置 bundle 时使用 |

更换这两个输入后重新构建 CLI。运行已经构建好的 musl 二进制时设置这些变量，不会替换其中的内核；动态固件入口与平台条件见[平台矩阵](platforms.md)。构建源见 `crates/pvisor-vm/build_kernel.rs`。
