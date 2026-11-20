# 记录与版本矩阵

pVisor 的执行身份、事实日志、文件发布和机器快照分别持有自己的记录。一次 Session 可以同时包含带完成协议 2 的 schema 3 `run.json`、schema 6 的 Bundle、version 5 的 Event 和 version 4 的 Session；这些数字分别约束各自的 reader，不能互相替代，也不等于 Cargo 包版本。

## 记录分别回答什么问题 {#ownership}

| 记录或协议 | 回答的问题 | 不能据此推断 |
| --- | --- | --- |
| RunSpec / Operation | 请求执行什么，有效策略与放置是什么？ | 控制已经安装、工作负载已经启动 |
| `run.json` / execution Session | 当前 Execution、生命周期、head 与请求归属是什么？ | 所有外部效果都成功，或所有记录共同原子提交 |
| Bundle / Event Journal | 观察到了什么，哪些事实已经提交？ | 完整机器可恢复，或外部请求 exactly-once |
| stage / preimage / apply ledger | 候选文件基于什么，哪些目标更新已执行？ | 多文件更新对外部读者原子可见 |
| checkpoint / snapshot manifest | 哪些文件或机器状态可以按指定 profile 恢复？ | 可跨任意宿主、binary 或架构恢复 |
| Host / Guest AgentCtl | 哪个端点接收哪一类控制或协作请求？ | 版本数字相同就具有相同权限或载荷 |

Event 的 ID 与 Journal position、Host `request_id`、Session 生命周期请求 ID、checkpoint ID 各自属于不同命名空间。关联记录要同时检查 Session、Execution、generation 和目标归属，不能只比较一个字符串。身份模型见[执行模型](execution-model.md)，事件关联见[Operation 与 Event](operations-events.md)。

## 执行与事实记录 {#execution-records}

下表按当前源码声明和读取分支整理；修改格式时需要同步核对 writer、reader、调用方和现存记录。字段级定义仍由对应专题维护。

| 对象 | 当前写入版本 | 读取校验 | 源码所有者（`crates/` 下） |
| --- | --- | --- | --- |
| RunSpec | `schema_version = 1` | runtime 拒绝不匹配版本；输入校验不等于执行成功 | `pvisor-core/src/execution.rs`、`pvisor/src/runtime/run.rs` |
| Operation | `version = 1` | `OPERATION_VERSION` 精确检查，并校验身份、种类与改写关系 | `pvisor-core/src/operation.rs` |
| Event | `version = 5` | `VERSION` 精确检查；Observation 的 `domain/name/version` 是独立载荷身份 | `pvisor-core/src/event.rs` |
| Journal Header / Record | `pvisor.trace/5` | 校验 Header 与每条 Event | `pvisor-journal/src/journal.rs` |
| Run Bundle | `schema_version = 6` | 要求匹配 Run/Execution 的完成回执与 Bundle 摘要 | `pvisor/src/runtime/bundle.rs` |
| RunRecord（`run.json`） | `schema_version = 3` | 完成协议 2 绑定终态结果与 Bundle；owner 退出且无回执时为 Unknown；终态清除 live control | `pvisor/src/runtime/registry.rs` |
| Execution 完成回执（`completion.json`） | `version = 2` | 绑定 `RunResult` 与 Bundle BLAKE3；缺失或不一致不能推断成功 | `pvisor/src/runtime/completion.rs` |
| 任务 / Unix 容器 Session（`session.json`） | `version = 4` | 校验版本、profile 与所有权绑定 | `pvisor/src/runtime/execution/session.rs`、`pvisor/src/docker/session.rs` |

Event version 5 与 Journal format 5 在当前实现中共用 Event 的版本常量；其他版本没有这种关系。一个领域 Observation 的载荷版本变化，也不能仅靠外层 Event version 5 判断业务兼容性。JSON 可被解析，只说明语法可读；还要经过具体记录的校验。

持久记录的公开字段见 [Run Bundle](../reference/run-bundle.md)，事件字节布局、回执与恢复见 [Journal](journal.md#format)。

## 文件状态与恢复载荷 {#state-records}

| 对象 | 当前格式 | 读取与发布边界 | 源码所有者（`crates/` 下） |
| --- | --- | --- | --- |
| workspace checkpoint | `schema_version = 3`，显式 `kind = workspace` | reader 要求 schema 3；不按 execution 机器载荷解释 | `pvisor/src/runtime/checkpoint.rs` |
| EnvironmentManifest | 写入版本由 profile 选择：2、3、4 或 5 | 校验 RAM 字段、文件布局、摘要与恢复前提 | `pvisor/src/environment_snapshot/store.rs` |
| 不可变基底 seal | `version = 2` | 必须保留摘要绑定的内容索引 | `pvisor/src/environment_snapshot/base.rs` |
| compact preimage log | `pvisor.preimages/2`，frame `PVR2` | 首次观察与完整性记录有独立合同 | `pvisor-overlay-core/src/preimage_log.rs`、`core.rs` |
| stage durability / seal | `durability-v1`、`sealed-v1` | 缺少 policy 按原 strict 合同处理；未知 policy 拒绝；受管理 stage 必须完整 seal 才可复用或 apply | `pvisor-overlay-core/src/stage.rs` |
| apply ledger / ApplyRecord | 普通 apply 使用 schema 2，项目范围 apply 使用 schema 3 | 在目标锁下按记录阶段恢复 | `pvisor-overlay-core/src/apply.rs` |
| `vm.ram` descriptor | `PVZRAM`，descriptor version 2 | 校验 descriptor、绝对代际目录与 head 记录；与环境 manifest 版本分别维护 | `pvisor/src/ram_backing.rs` |

环境 manifest 的多个版本代表不同 RAM/文件系统组合，不能把“最高版本为 5”解释成“所有新快照都写 5”。stage profile 的 raw RAM 使用 v4，压缩 RAM 使用 v5；其他 profile 按各自字段组合判定。

Session 的检查点入口与兼容 profile 见[Session 检查点设计](session-checkpoint-cli.md#10-当前实现与验收边界)。完整机器与文件引用见[环境快照](environment-snapshot.md#storage-contract)，RAM 子格式见[卸载文件格式](offload/disk-layout-and-schema.md)，镜像元数据与内容版本见[共享镜像缓存](lazy-image-v2.md)。

## 控制协议与构建身份 {#control-protocols}

| 镜像索引／内容池 | `format_version=2`、`pvisor-radix-v2`、`pvisor-v2:` | 条件更新 HEAD 提交发布 | `pvisor/src/image/cache/portable.rs`、`portable/`、`pool.rs` |
| 协议 | 版本与额外绑定 | 权限范围 |
| --- | --- | --- |
| Host AgentCtl envelope | `AGENTCTL_HOST_VERSION = 2` | Session/Execution/generation 目标由端点 owner 检查；Guest token 不授予 Host 权限 |
| Guest AgentCtl | `AGENTCTL_VERSION = 1` | 工作负载 `Hello`/`Sync`、状态与协作；和 Host 是两套协议 |
| Session owner 实例控制 | `pvisor-instance-control-3`，version-2 Host envelope，精确 Session/Execution 与记录的 socket device/inode | 同 UID 调用方访问记录中的 owner socket |
| CLI owner 启动 | 继承的私有 socketpair、内部 DTO 与同一二进制 BLAKE3 校验 | 仅前端启动自己的 owner；后续命令不使用该 DTO |
| 持久 Session 操作锁 | `session.lock` 诊断协议 `pvisor-session-lock-1`，PID 与操作名 | 独占 flock、0600、单硬链接 |
| daemon native supervisor | version-2 Host envelope，加 owner/token 与精确目标 | 凭据与准入独立于 Session owner 协议 |

[Host AgentCtl](architecture.md#host-wire)定义消息 framing 与 socket 权限。
[Rust API 边界](architecture.md#api-boundaries)列出 crate 的公开入口。

## 校验记录绑定 {#evolution}

1. 通过所属读取器校验 schema 与必需字段。
2. 对持久状态核对摘要、依赖引用及 Session、Execution 和 stage 归属。
   对活体控制核对端点与已记录的 socket 身份。
3. 超时、断连或发布失败后，定位原请求的提交点，并按[失败语义](failure-semantics.md)核对。
