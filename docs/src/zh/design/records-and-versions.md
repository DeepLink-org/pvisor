# 记录与版本矩阵

pVisor 的执行身份、事实日志、文件发布和机器快照分别持有自己的记录。一次 Job 可以同时包含 schema 1 的 `run.json`、schema 4 的 Bundle、version 5 的 Event 和 version 2 的 execution Job；这些数字分别约束各自的 reader，不能互相替代，也不等于 Cargo 包版本。

## 记录分别回答什么问题 {#ownership}

| 记录或协议 | 回答的问题 | 不能据此推断 |
| --- | --- | --- |
| RunSpec / Operation | 请求执行什么，有效策略与放置是什么？ | 控制已经安装、工作负载已经启动 |
| `run.json` / execution Job | 当前 Attempt、生命周期、head 与请求归属是什么？ | 所有外部效果都成功，或所有记录共同原子提交 |
| Bundle / Event Journal | 观察到了什么，哪些事实已经提交？ | 完整机器可恢复，或外部请求 exactly-once |
| stage / preimage / apply ledger | 候选文件基于什么，哪些目标更新已执行？ | 多文件更新对外部读者原子可见 |
| checkpoint / snapshot manifest | 哪些文件或机器状态可以按指定 profile 恢复？ | 可跨任意宿主、binary 或架构恢复 |
| Host / Guest AgentCtl | 哪个端点接收哪一类控制或协作请求？ | 版本数字相同就具有相同权限或载荷 |

Event 的 ID 与 Journal position、Host `request_id`、Job 生命周期请求 ID、checkpoint ID 各自属于不同命名空间。关联记录要同时检查 Job、Attempt、generation 和目标归属，不能只比较一个字符串。身份模型见[执行模型](execution-model.md)，事件关联见[Operation 与 Event](operations-events.md)。

## 执行与事实记录 {#execution-records}

下表按当前源码声明和读取分支整理；修改格式时需要同步核对 writer、reader、调用方和现存记录。字段级定义仍由对应专题维护。

| 对象 | 当前写入版本 | 读取与兼容边界 | 源码所有者（`crates/` 下） |
| --- | --- | --- | --- |
| RunSpec | `schema_version = 1` | runtime 拒绝不匹配版本；输入校验不等于执行成功 | `pvisor-core/src/execution.rs`、`pvisor/src/runtime/run.rs` |
| Operation | `version = 1` | `OPERATION_VERSION` 精确检查，并校验身份、种类与改写关系 | `pvisor-core/src/operation.rs` |
| Event | `version = 5` | `VERSION` 精确检查；Observation 的 `domain/name/version` 是独立载荷身份 | `pvisor-core/src/event.rs` |
| Journal Header / Record | `pvisor.trace/5` | Header 与 Event 校验相互配合；完整旧格式拒绝，不当作坏尾部截掉 | `pvisor-journal/src/journal.rs` |
| Run Bundle | `schema_version = 4` | 要求当前 schema 和观察合同；不能用默认零值伪造缺失观察 | `pvisor/src/runtime/bundle.rs` |
| RunRecord（`run.json`） | `schema_version = 1` | reader 检查 schema 1；可选/default 字段按当前 serde 定义读取 | `pvisor/src/runtime/registry.rs` |
| execution Job（`execution-job.json`） | `version = 2` | 精确版本与 Job/root/stage 绑定检查；旧记录不自动转换 | `pvisor/src/runtime/job_execution.rs` |

Event version 5 与 Journal format 5 在当前实现中共用 Event 的版本常量；其他版本没有这种关系。一个领域 Observation 的载荷版本变化，也不能仅靠外层 Event version 5 判断业务兼容性。JSON 可被解析，只说明语法可读；还要经过具体记录的校验。

持久记录的公开字段见 [Run Bundle](../reference/run-bundle.md)，事件字节布局、回执与恢复见 [Journal](journal.md#format)。

## 文件状态与恢复载荷 {#state-records}

| 对象 | 当前格式 | 读取与发布边界 | 源码所有者（`crates/` 下） |
| --- | --- | --- | --- |
| workspace checkpoint | `schema_version = 3`，显式 `kind = workspace` | reader 要求 schema 3；不按 execution 机器载荷解释 | `pvisor/src/runtime/checkpoint.rs` |
| EnvironmentManifest | 写入版本由 profile 选择：2、3、4 或 5 | reader 接受明确的 v1–v5/RAM 字段组合，并检查文件布局、摘要、平台条件和 `SnapshotCompatibility` | `pvisor/src/environment_snapshot/store.rs` |
| 不可变基底 seal | `version = 2` | 必须保留摘要绑定的内容索引；旧 seal 拒绝 | `pvisor/src/environment_snapshot/base.rs` |
| compact preimage log | `pvisor.preimages/2`，frame `PVR2` | 首次观察与完整性记录有独立合同；旧逐文件 journal 由旧路径读取，不原地强转格式 | `pvisor-overlay-core/src/preimage_log.rs`、`core.rs` |
| stage durability / seal | `durability-v1`、`sealed-v1` | 缺少 policy 按原 strict 合同处理；未知 policy 拒绝；受管理 stage 必须完整 seal 才可复用或 apply | `pvisor-overlay-core/src/stage.rs` |
| apply ledger / ApplyRecord | 当前 schema 2；reader 接受 1 或 2 | 兼容读取不把旧记录补成新保证；在目标锁下按记录阶段恢复 | `pvisor-overlay-core/src/apply.rs` |
| `vm.ram` descriptor | `PVZRAM`，descriptor version 2 | 校验 descriptor、绝对代际目录与 head 记录；与环境 manifest 版本分别维护 | `pvisor/src/ram_backing.rs` |

环境 manifest 的多个版本代表不同 RAM/文件系统组合，不能把“最高版本为 5”解释成“所有新快照都写 5”。stage profile 的 raw RAM 使用 v4，压缩 RAM 使用 v5；其他 profile 按各自字段组合判定。SDK 可以读取某种存储格式，也不表示当前 Job CLI 能直接采用任意历史独立 snapshot store。

Job 的检查点入口与兼容 profile 见[Job 检查点设计](job-checkpoint-cli.md#10-当前实现与验收边界)。完整机器与文件引用见[环境快照](environment-snapshot.md#storage-contract)，RAM 子格式见[卸载文件格式](memory-optimization/offload-format.md)，镜像元数据与内容版本见[共享镜像缓存](shared-image-cache-storage.md)。

## 控制协议与构建身份 {#control-protocols}

| 协议 | 版本与额外绑定 | 权限与兼容范围 |
| --- | --- | --- |
| Host AgentCtl envelope | `AGENTCTL_HOST_VERSION = 1` | Job/Attempt/generation 目标由端点 owner 检查；Guest token 不授予 Host 权限 |
| Guest AgentCtl | `AGENTCTL_VERSION = 1` | 工作负载 `Hello`/`Sync`、状态与协作；和 Host 是两套协议 |
| CLI Job listener / worker ticket | `pvisor-job-ticket-4`，再绑定 Cargo 包版本与可执行文件 BLAKE3 | 内部精确 schema/build 匹配；同包版本重新构建也可能不兼容 |
| daemon native supervisor | version-1 Host envelope，加 owner/token 与精确目标 | 不使用 Guest 协议或 CLI worker ticket；旧 supervisor 无透明接管兼容 |

源码入口是 `pvisor-core/src/host_protocol.rs`、`protocol.rs`、`pvisor-cli/src/cli/host_service.rs` 和 `pvisor/src/runtime/supervisor.rs`。线上 framing、同 UID 边界和升级顺序由[Host AgentCtl](architecture.md#host-wire)维护。Rust `api` 的组织方式与线上兼容性分别管理，见[API 边界与迁移状态](architecture.md#api-boundaries)。

## 版本变化时沿哪些关系核对 {#evolution}

1. 确认变化属于输入 DTO、磁盘对象、领域载荷还是 live 协议，找到实际 reader。
2. 明确接受旧格式、提供显式转换，或拒绝旧格式；禁止只改版本数字绕过结构与绑定检查。
3. 对存储对象同时核对摘要、依赖引用、stage/Attempt 归属和恢复 profile；对 live 协议同时核对权限、generation、ticket 与构建身份。
4. 在升级前处理旧 listener/supervisor 的活动请求和实例；文件格式兼容不等于运行中进程可接管。
5. 发生超时、断连或发布失败时，先判断原请求在哪个提交点，按[失败语义与重试](failure-semantics.md)核对，再决定是否重试。

