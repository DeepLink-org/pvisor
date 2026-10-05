# 协议、部署与故障处理

一个 Controller 与多个 Worker 构成当前最小部署。服务身份、存储目录和执行后端应显式配置；任务资源预留和运行隔离分别验证。

## 本地启动与部署边界 {#deployment}

在仓库根目录构建，分别用两个 shell 启动 Controller 和一个可信 host Worker：

```sh
just cluster-build
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-cluster serve --journal /tmp/pvisor-controller/journal
```

```sh
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-worker --id worker-1 --state /tmp/pvisor-worker-1 \
  --backend host --slots 16 --memory-bytes 8589934592 --cpu-millis 4000
```

```sh
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
target/debug/pvisor-cluster submit crates/pvisor-cluster/examples/task.json
target/debug/pvisor-cluster show hello
target/debug/pvisor-cluster workers
```

示例凭据仅用于本地复现；部署使用独立随机凭据。Controller 默认监听 `127.0.0.1:19800`，Worker 默认 rootless，不支持所选后端时失败；`host` 是显式可信进程执行。多个 Worker 必须使用不同 ID 和独占状态目录。Gateway 功能使用 `just cluster-build-gateway`。

跨机器部署需要可达 Controller URL、TLS 终止、节点状态持久化、环境/检查点仓库权限及平台执行器依赖。共享 Worker token 是服务角色认证，未提供每节点、每租户细粒度鉴权或恶意 Worker 隔离；tenant 字段不是认证身份。

## 配置与有界性 {#configuration}

| 配置/界限 | 默认值或限制 | 所属位置 |
| --- | --- | --- |
| Controller URL | `http://127.0.0.1:19800` | `--url` / `PVISOR_CLUSTER_URL` |
| 管理 / Worker token | 两个不同、至少 16 字符的值 | `PVISOR_CLUSTER_TOKEN` / `PVISOR_CLUSTER_WORKER_TOKEN` |
| journal | `.pvisor/cluster/journal` | serve `--journal` |
| lease duration | 30,000 ms，允许 100–300,000 ms | serve `--lease-ms` |
| metadata quota | 1 GiB；至少容纳 16 MiB 帧 | `--max-journal-bytes` |
| artifact payload cap | 8 GiB | `--max-artifact-bytes` |
| 对象存储策略 / 租户资源配额 | 显式 JSON 配置 | `--artifact-limits` / `--quotas` |
| ready 窗口 / 分配批量 / task history | 256 / 64 / 1,000,000 | SchedulerConfig；CLI 未暴露全部字段 |
| HTTP 调度请求体 | 4 MiB；产物上传另按 chunk 限制 | server |
| Worker state / poll | `.pvisor/worker` / 1000 ms | `--state` / `--poll-ms` |
| Worker 槽位 / RAM / CPU | 16 / 8 GiB / 4000 millis | `--slots` / `--memory-bytes` / `--cpu-millis` |
| Worker 配置 | host-owned TOML，未知字段拒绝 | `--config` |

Worker profile 配置 Gateway、VM/container、网络、只读 lower、admission、环境、检查点存储、CPU QoS 及内存/CPU 采样。密钥和供应商凭据由节点配置持有；能力广告必须与实际可执行配置一致。

## HTTP 协议目录 {#api}

共享类型位于 `pvisor-core/src/cluster.rs`，当前 `CLUSTER_VERSION = 1`。path 相同的 GET/POST 分别表示读取与提交；表中省略的所有路径前缀均为 `/v1`。

| 角色 | 方法和路径 | 请求/响应用途 |
| --- | --- | --- |
| public | `GET /health`（无 `/v1`） | 协议版本与 Dispatcher 可用性；不证明 Worker 或存储充裕 |
| Admin | `POST /tasks`、`GET /tasks/{id}` | TaskSpec → TaskRecord；查询 pending 和终态 |
| Admin | `POST /tasks/{id}/cancel`、`POST /tasks/{id}/resolve-lost` | 取消；精确 LeaseKey 结束待对账身份 |
| Admin | `POST /tasks/{id}/control`、`GET /tasks/{id}/inference-wait` | ControlRequest；观察自动 wait，不能据此授权回复 |
| Admin | `POST /graphs`、`GET /graphs/{id}`、`POST /graphs/{id}/cancel` | 原子 DAG 提交、查询、取消 |
| Admin | `POST /tasks/{id}/forks`、`GET /tasks/{id}/forks/{request_id}` | sealed fork 请求与创建回执 |
| Admin | `POST /tasks/{id}/live-forks`、`GET /tasks/{id}/live-forks/{request_id}` | live capture/fork 进度与回执 |
| Admin | `POST /environments`、`GET /environments/{digest}` | 不可变模板注册/读取 |
| Admin | `GET /workers`、`POST /workers/{id}/drain`、`GET /counts` | 节点、drain、派生计数 |
| Admin | `GET /tasks/{id}/artifacts`、`GET /artifacts/{digest}` | 保留 manifest / 对象 |
| Admin | `POST /tasks/{id}/artifact-downloads`、`POST /artifact-downloads/{id}/renew`、`POST /artifact-downloads/{id}/release` | 多对象下载期间的显式保护 |
| Admin | `GET /artifact-storage`、`POST /artifact-storage/limits` | 唯一对象空间/个数、发布预留与在线策略 |
| Admin | `POST /artifact-storage/gc/plan`、`POST /artifact-storage/gc/apply` | 预览/执行不可变回收计划 |
| Worker | `POST /workers/register`、`POST /workers/poll` | WorkerRegistration；PollRequest → PollResponse |
| Worker | `POST /workers/recover`、`POST /workers/decline` | 旧终态身份恢复；未启动拒绝 |
| Worker | `POST /workers/native-done`、`POST /workers/complete` | 原生终止交接；Completion 与终态回执 |
| Worker | `POST /workers/control-ack`、`POST /workers/inference-wait` | 原生观察；begin/ready/observe 屏障 |
| Worker | `POST /workers/memory`、`POST /workers/cpu`、`POST /workers/node-memory` | 租约绑定观察 / 节点采样 |
| Worker | `POST /workers/artifacts/{task_id}/{generation}/{worker_id}/{incarnation}/{digest}` | 精确租约绑定对象上传 |

角色 token 用 Bearer header 传递。非法凭据返回 401，域冲突常返回 409，已退休证据返回 410，配额拒绝返回 507，过载/不确定提交/可重试发布故障返回 503；超大请求体由 HTTP 框架拒绝。查询不存在的任务也属于当前域错误合同，不能假设全部 REST 状态码语义。

重试依据身份和内容，而非 HTTP 请求次数。任务/图 ID 对应同一不可变规格，控制/分叉对应相同 request ID，完成对应精确 key 与一致证据。协议版本不变不代表旧二进制理解所有新枚举和事务：启用新控制、环境或推理等待功能前先升级 Controller，禁止不经验证降级读取新日志。

## 故障与处理步骤 {#runbook}

| 现象 | 当前行为 | 处理 |
| --- | --- | --- |
| HTTP 超时/丢响应 | 排队操作可能已提交 | 用相同幂等身份重试并查询，不立即生成替代执行 |
| Controller 正常重启 | 非终态租约 pending；保留预留 | 原 Worker 继续 poll；查询 pending 清除情况 |
| Controller 故障超过本地 lease | Worker watchdog 请求停止 | 核对终态交付和副作用；不要把 watchdog 请求视为已确认停止 |
| Worker 重启 | 先交付已知终态；未知旧执行不收养 | 保留 state，必要时处理待对账身份再注册新 incarnation |
| 长期 pending | 旧 Worker 不可联系，GC 受阻 | 核对节点/业务后，用完整 key 显式 resolve-lost |
| Controller quota 满 | 续租/读取可继续；新意图/首次结果可能拒绝 | 检查磁盘并增加 metadata quota；outbox 继续保留证据 |
| journal write/fsync 不确定 | poison，503 | 检查存储，备份现场，重启验证；完整坏帧不能静默截断 |
| artifact 满 | 发布拒绝、保留 pending | 预览退休/GC、核对计划或提高存储策略；先处理待对账 |
| drain / SIGTERM | drain 停止新预留；Worker 尝试原生取消和终态交付 | 等待结果与 outbox 收敛；SIGKILL 属未确认丢失 |

重启保留原 journal 与匹配的 artifact authority/store。不要在线拷贝部分目录作为一致备份，不把 Worker 缓存与 source checkpoint 的任意删除当作安全回收。当前没有自动化一致备份/恢复协议。

可观察字段包括 TaskRecord phase/pending/lease/result/control history、Worker reserved/admission、counts、artifact-storage 及 Worker 原生证据。容量告警至少覆盖日志字节、对象空间/个数、pending 时长、outbox 积压和节点压力；统一指标导出、告警服务和计费是集成工作。

## 验证矩阵 {#validation}

| 层次 | 入口 / 现有覆盖 | 不能由其推出的结论 |
| --- | --- | --- |
| 共享协议和调度器 | `just test pvisor-cluster pvisor-core`：幂等、fencing、DAG、控制、配额、重放、GC、推理等待 | 原生隔离、跨主机密度 |
| HTTP 与独立 Worker | `just test-cluster`：实际进程执行、Controller 重启、同 key 对账、丢 ACK/outbox | 任意外部效果 exactly-once |
| Gateway 特性 | `just test-cluster-gateway`：等待屏障、模型/工具协议与恢复边界 | 真实供应商性能、全量硬件路径 |
| Linux VM/环境 | `just test-cluster-vm`：需 KVM/FUSE 的显式硬件 gate | 任意主机/架构可迁移 |
| VM + Gateway | `just test-cluster-vm-gateway`：真实 guest 推理等待、人工暂停、CPU 重新准入和短暂 server 重启 | abrupt process crash、长故障、跨主机容灾 |
| 节点配额实验 | `just test-cluster-cgroup`：有限 Linux cgroup quota 与受控 overcommit | 通用吞吐/成本提升 |

重启对账测试让真实 Worker 持有超过落盘旧 deadline 的执行，重新打开 Controller 后确认同 key，并核对一次执行标记；它验证身份和热路径无心跳写入。Group commit 测试验证共同 sync、队列/阈值及不确定提交。硬件 gate 要求设备并显式运行；默认测试跳过这些 gate 不表示它们已验证。

语义规格遵循项目 semspec 流程；测试通过与人工批准分开。设计文档不替代测量报告、语义审批或生产验收，也不以历史测试总数证明新版本规模。

## 扩展方向与设计约束 {#evolution}

| 方向 | 必须保持的约束 |
| --- | --- |
| 进一步减少 Controller 持久状态 | 明确上层 desired state、Worker terminal inventory 和保留 manifest 的权威，先解决未分配意图与已 ACK 历史重建 |
| 多 shard / HA | 稳定 shard ownership、任务路由、执行/对象 authority fencing；本地文件锁不提供跨节点 leader 选举 |
| 在线元数据压缩 / Worker GC | 保留幂等 ID、终态回执、lineage 和活动根；先定义退休和历史可见合同 |
| 更细 GC | 对未知租约建立可验证的对象根，再缩小当前 shard-wide pending 阻塞范围 |
| 每节点/租户身份与远端 CAS | 凭据生命周期、租户授权、对象归属和传输保护；shared token 不足以覆盖 |
| 调度策略与 RAM 回收 | 用原生、节点级证据定义可复用容量；受控实验再调整预留/overcommit |
| 跨主机恢复与 RL 生命周期 | 固定运行时兼容矩阵，验证分区/重启/发布故障，协调 scaffold/训练状态 |
| 生产规模 | 多主机长期运行、密度/有效工作/尾延迟基准，故障与运维成本测量 |

扩展时首先保持[状态权威](state-and-recovery.md#authority)与[设计不变量](index.md#invariants)。研究场景和未解决的问题见[集群执行研究](../research/cluster-execution.md)。
