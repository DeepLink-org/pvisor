# 本机状态与重启恢复

重启使用相同私有状态目录和兼容运行时配置，保留原生归属及未解决的预留。已存状态是最近一次持久观察，不证明 VM 仍存活。

## 权威与身份 {#authority}

| 状态 | 权威 | 重启行为 |
| --- | --- | --- |
| Registry 版本与 owner UUID | Daemon 持久 registry | 校验并保留相同 owner |
| Sandbox 身份、镜像/argv/env/metadata、资源、TTL 与端点 token | 持久记录 | 保留意图、访问身份与保守容量 |
| Running/Paused/Terminated | 原生运行时观察 | 查询已登记对象，更新变化后的观察 |
| `Stopping` | 持久删除意图 | 重试删除，查询不能覆盖它 |
| 实际 VM 与服务就绪 | 原生 supervisor、VM 控制、内核 cgroup 与真实服务 | Registry 不能重建它们 |

一个 daemon 独占锁定 `daemon.lock`。私有 IPC 校验同 UID peer、owner、sandbox ID、generation 与秘密 token；持久身份绑定 boot ID 和 cgroup device/inode。ID 不复用、不重新启动。IPC 丢失表示不确定，不是 Missing 或清理证据。持久删除意图与 supervisor 独占锁阻止迟到启动；清理使用身份绑定的 `cgroup.kill`，不保存 PID 或按 PID kill，确认 cgroup 为空且 owner 锁释放后才释放容量。同一 boot 下没有持久 tombstone 证据的 cgroup 被替换或丢失不证明对象不存在。

版本 1 中已有记录的 `sandboxes.json` 若缺少原生 `owner.json` marker，会在既有 store 独占锁内被拒绝：它可能仍持有活动 Podman 容器。使用全新原生状态并保留／清理旧部署，或通过旧 Podman daemon 删除全部 sandbox、确认清理后，再用已清空的 registry 切换后端。不要删除 registry 条目、预留或所有权状态，也不要伪造原生 marker 绕过检查。原生 daemon 不会把这些容器接管为 Missing 或静默释放其预留。

版本 2 header 缺少 `owner.json` 时也会被拒绝，即使 `sandboxes` map 为空：该 header 激活私有逐 sandbox 记录，不表示清单为空。恢复原始归属状态，不要自行制造 marker。见[存储激活与迁移](storage.md#migration)。

## 启动对账 {#reconcile}

1. 打开并校验私有 registry，拒绝损坏、超限或不兼容内容，不通过遗忘原生归属继续启动。
2. 使用已持久 owner 构造运行时。只有 factory 接受该 owner 后才初始化／迁移存储：提交带 owner 信封的记录与 metadata，再原子激活 v2 header。激活前 v1 完整快照始终是权威。存储初始化后、绑定 API 前完成原生前置检查。
3. 重试 `Stopping` 记录；持有各自生命周期锁查询其他已登记 sandbox。
4. 将原生 Running/Paused/Stopped/Missing 映射为 `Running`/`Paused`/`Terminated`/`Failed`。缺失 sandbox 保持可见且预留资源，直到显式删除。
5. 报告延后对账错误，不虚构清理成功。启动维护以重试过期／待删除对象。

正常 daemon 关闭保留独立 supervisor/VM 与记录。重启重新连接私有 IPC，不重建 VM 或生成新 Attempt；IPC 丢失保留不确定状态。GET 对账，list 返回持久观察。启动不接管未知 VM，也不重新启动缺失／崩溃工作负载；宿主重启不能保留 live VM。

清理独立于启动资源，校验持久 owner/ID/generation 与 boot/cgroup 绑定：不要求原 rootfs 或 firmware 目录仍存在，也不要求 guest argv/env/资源设置通过启动校验。证明原生对象不存在并取得 owner 锁后，持久发布 `tombstone.json`，删除空的所属 cgroup，回收私有 run 存储、live RAM backing、临时 overlay/spec、socket 与含秘密的 identity 记录及可能遗留的 observation 缓存。最小目录保留 tombstone、owner 锁和生命周期 marker，阻止 ID 复用。中断的回收从 tombstone 继续，包含 cgroup 已移除的情况；报错保留预留。同一 boot 下没有此持久证据的 cgroup 丢失／被替换仍不授权不存在结论。 见[存储](storage.md#gc)。

## 不确定性与恢复 {#failures}

| 故障 | 保留边界 | 操作 |
| --- | --- | --- |
| 创建失败，清理已确认 | 记录／预留已移除 | 再次请求前排查镜像／运行时 |
| 创建清理不确定 | Failed 记录、错误中的 ID、完整预留 | 查询／删除该 ID，不盲目重提 |
| 原生查询不可用 | 不虚构原生状态 | 修复运行时访问，再次查询 |
| 删除失败或不存在状态未知 | 保留 `Stopping`、token/记录与预留 | 恢复运行时访问并重试 |
| Registry 提交不确定 | Storage-failed 标记拒绝后续 registry 访问 | 保留状态，修复存储，重启对账 |
| Daemon 停机超过 TTL | 停机期间无维护 | 独立终止要求由宿主监督落实 |

Registry 是增量提交的本机清单，不是完整执行历史或业务副作用账本。版本 2 根据 owner/header 与已校验的逐 sandbox 记录重建清单；已激活的记录树损坏或不完整时不回退到 v1。重启不回滚外部调用、不恢复丢失 RAM，也不提供跨主机故障切换。自动一致备份／原生状态恢复未实现；复制活动目录或删除状态不是恢复流程。

恢复路径属于 `daemon/mod.rs`，registry 校验与持久化属于 `daemon/store.rs`。见[存储](storage.md)与[运维](operations.md#runbook)。
