# 本机状态与重启恢复

重启使用相同私有状态目录和兼容运行时配置，保留原生归属及未解决的预留。已存状态是最近一次持久观察，不证明容器仍存活。

## 权威与身份 {#authority}

| 状态 | 权威 | 重启行为 |
| --- | --- | --- |
| Registry 版本与 owner UUID | Daemon 持久 registry | 校验并保留相同 owner |
| Sandbox 身份、镜像/argv/env/metadata、资源、TTL 与端点 token | 持久记录 | 保留意图、访问身份与保守容量 |
| Running/Paused/Terminated | 原生运行时观察 | 查询已登记对象，更新变化后的观察 |
| `Stopping` | 持久删除意图 | 重试删除，查询不能覆盖它 |
| 实际容器与服务就绪 | Podman 与预制服务 | Registry 内容不能重建它们 |

一个 daemon 独占锁定 `daemon.lock`。Sandbox ID 使用随机 UUID，不复用。每个原生操作检查 owner/sandbox 标签；registry 归属是本机机制，不是远端选主，也不防御可信宿主账户。

## 启动对账 {#reconcile}

1. 打开并校验私有 registry，拒绝损坏、超限或不兼容内容，不通过遗忘原生归属继续启动。
2. 使用已持久 owner 构造运行时，在绑定 API 前完成原生前置检查。
3. 重试 `Stopping` 记录；持有各自生命周期锁查询其他已登记 sandbox。
4. 将原生 Running/Paused/Stopped/Missing 映射为 `Running`/`Paused`/`Terminated`/`Failed`。缺失 sandbox 保持可见且预留资源，直到显式删除。
5. 报告延后对账错误，不虚构清理成功。启动维护以重试过期／待删除对象。

正常关闭保留容器与记录供重启。List API 返回最近持久观察，GET 对账原生状态。维护是删除循环，不是持续原生进程监控。启动不发现／接管未知容器，也不重建缺失工作负载。

## 不确定性与恢复 {#failures}

| 故障 | 保留边界 | 操作 |
| --- | --- | --- |
| 创建失败，清理已确认 | 记录／预留已移除 | 再次请求前排查镜像／运行时 |
| 创建清理不确定 | Failed 记录、错误中的 ID、完整预留 | 查询／删除该 ID，不盲目重提 |
| 原生查询不可用 | 不虚构原生状态 | 修复运行时访问，再次查询 |
| 删除失败或不存在状态未知 | 保留 `Stopping`、token/记录与预留 | 恢复运行时访问并重试 |
| Registry 提交不确定 | Storage-failed 标记拒绝后续 registry 访问 | 保留状态，修复存储，重启对账 |
| Daemon 停机超过 TTL | 停机期间无维护 | 独立终止要求由宿主监督落实 |

Registry 是本机状态 checkpoint，不是完整执行历史或业务副作用账本。重启不回滚外部调用、不恢复丢失 RAM，也不提供跨主机故障切换。自动一致备份／原生状态恢复未实现；复制活动目录或删除状态不是恢复流程。

恢复路径属于 `daemon/mod.rs`，registry 校验与持久化属于 `daemon/store.rs`。见[存储](storage.md)与[运维](operations.md#runbook)。
