# 完整环境快照与 CLI 迁移

独立 `pvisor snapshot` 前端已删除。VM/Cluster 和存储 SDK 继续使用完整状态封存、RAM 编码、基底引用与恢复机制；这些能力不再通过另一套独立实例命令暴露。普通 Job 已接入原生完整执行捕获与恢复，旧 store 不自动转换为 Job 检查点。

## 当前用户入口 {#current-entry}

普通 Job 的检查点、挂起、恢复与分叉只通过 `checkpoint`、`suspend`、`resume`、`fork` 管理。支持停止 Job 的 workspace 检查点，以及符合原生 profile 的 VM execution 检查点。

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

原 `save` 对应 `suspend`；恢复当前执行点用 `resume`，恢复历史点或创建分支用 `fork --state execution --checkpoint ID`。原 `run` 使用普通 `run --executor vm`；list/delete/gc、基底导入和校验进入 Job 的 `checkpoint` 命令。无网络设备、私有 RAM、拥有完整 rootfs 的原生 VM 支持 capture-and-continue 和完整恢复；普通 `run` 的配置不会自动改变。接口和限制见 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)，交接设计见[Job 检查点设计](job-checkpoint-cli.md#10-当前实现与验收边界)。

集群任务与 VM 控制使用 service 下的 Cluster 入口，保持 Task/Lease/控制修订身份与 Worker 对账：

```bash
pvisor service cluster --help
```

受限资源的实际保存、分叉与恢复流程见[VM 与 Gateway 指南](../guides/cluster/vm-and-gateway.md)，节点 backing 的管理见[统一服务指南](../guides/cluster/service.md)。

## 存储与内部生命周期 {#storage-contract}

封存对象仍包含兼容性绑定、CPU/设备状态和 RAM，以及实际 profile 捕获的文件系统。不可变基底和压缩内容可复用，恢复写入保持私有；普通新启动不会自动共享 guest 匿名 RAM。

SDK `SnapshotStore` 保留对象校验、引用与 GC；`open_for_restore`/`ram_reader` 提供带租约的按需读。`open_owned_stage_for_restore` 与 `materialize_owned_stage` 提供已由本地发布校验的 stage 复制，仍要求兼容性、基底 pin、拓扑检查与发布对象租约；不适用于有外部写者的 payload。完整审计继续使用 `open`/`open_for_restore`。

原 CLI 的 RAM server/watchdog 已迁到 native runner 的私有启动路径，保持 EOF 排空与清理，不依赖公开 `snapshot` 命令。`run` 的参数和默认执行行为保持原合同。

## 历史证据 {#historical-evidence}

2026-10-03/04 的独立 snapshot 正确性和时延记录属于当时的源码/制品。历史 harness 必须明确传入仍支持该旧命令的归档 binary，不能用当前 binary 复现；它们不是当前 Job execution profile 已交付的证据。原始数据保留在[VM 内存实验](../benchmarks/vm-memory/index.md)。
