# Sandbox 生命周期

报告控制完成或释放资源前，确认本机原生状态。Daemon 按 sandbox 串行执行操作，不同 sandbox 的操作可以独立推进。

## 创建与控制 {#control}

| 操作 | 持久／原生顺序 | 响应 |
| --- | --- | --- |
| Create | 持久化 `Pending` 与预留 → 创建／启动 → 核对配置及服务就绪 → 持久化 `Running` | 202 JSON |
| Pause | 查询 → 持久化 `Pausing` → 已确认的 vCPU pause → 确认 Paused → 持久化 `Paused` | 202 空体 |
| Resume | 查询 → 持久化 `Resuming` → 已确认的 vCPU resume 与就绪 → 确认 Running → 持久化 `Running` | 202 空体 |
| Delete | 持久化 `Stopping` → 确认原生对象不存在并独占 owner 锁 → 发布 tombstone → 回收 cgroup／私有 run 存储 → 确认 Missing → 持久移除 registry 记录 | 204 |

已观察到目标 pause/resume 状态时，可以不重复原生控制而返回成功。原生错误不能视为控制成功。已接受生命周期操作运行于自持有的异步任务，HTTP 断开不会取消它。丢失 create 响应不构成幂等重试合同；盲目重提可能创建另一个 sandbox。

私有 IPC 校验同 UID peer、owner、sandbox ID、generation 与秘密 token；持久身份绑定 boot ID 和 cgroup device/inode。ID 不复用、不重新启动。IPC 丢失表示不确定，不是 Missing 或清理证据。持久删除意图与 supervisor 独占锁阻止迟到启动；清理使用身份绑定的 `cgroup.kill`，不保存 PID 或按 PID kill，确认 cgroup 为空且 owner 锁释放后才释放容量。同一 boot 下没有持久 tombstone 证据的 cgroup 被替换或丢失不证明对象不存在。

清理使用所有权绑定，不依赖镜像／启动校验；rootfs/firmware 已移除不妨碍清理。持久 tombstone 支持重试中断的 cgroup／私有存储回收，最终只保留最小 ID 屏障。清理错误保留预留。见[存储](storage.md#gc)。

## 过期 {#expiration}

省略／null timeout 表示手动清理。TTL 至少 60 秒，并受配置上限约束（默认一天，配置最大一年）。续期提供上限以内、未来的 RFC3339 时间；已有截止时间必须延长，已过期或不可续期状态会被拒绝。

维护每秒扫描一次，最多并发八个清理任务。清理在与续期相同的锁内重新检查过期，陈旧扫描不能删除已经续期的 sandbox。待删除意图在清理失败后保留并重试。

TTL 是尽力清理，不是硬执行截止时间。慢原生命令、上传／等待响应头及同 sandbox 控制会延迟删除。过期后拒绝新代理请求；已建立响应流没有总超时。Daemon 停机期间维护停止，更强截止要求需独立宿主监督。

## 服务访问与并发 {#endpoints}

只发布 execd 44772 和 egress 18080 端口。端点发现与每次数据请求的端点解析都会认证 live supervisor，要求当前 RunHandle 为 Running，检查删除屏障并解析当前发布，不信任持久 observation 缓存。发现返回 daemon 路由的 authority，不公开 supervisor loopback 地址。解析不会为每次数据请求重复服务健康探测或完整 cgroup 额度对账。Create、Inspect 和 resume 保留完整配置／就绪检查；上游连接失败由 API adapter 处理，不作为就绪证据。代理建立连接与控制／删除共用生命周期锁，避免连接期间由 daemon 管理的删除／端口复用。外部操作运行时不在此协调范围内。

代理流式转发真实上游 command/file/health/metrics 流量，包括 multipart 和 SSE。上传加等待响应头限制为 120 秒，已建立响应没有总超时。不支持 WebSocket、CONNECT 和任意应用端口。流量／输出配额属于上游服务。

## 独立执行语义 {#integration}

原生 pause/resume 通过 RunHandle 控制同一 Attempt 的 live vCPU，不是 cgroup freeze、内存 offload、checkpoint/restore 或重建 VM。准入记账保留。Execd 转发不是 Gateway 路由、捕获或推理空闲 CPU 释放。

普通 Job 保持独立的[checkpoint/fork 语义](../execution-model.md)。Daemon 不能 stage/apply 文件、恢复原生 VM 状态、获取 node backing 或协调模型响应。接入这些路径需要显式适配器及生命周期／证据合同，不能靠重命名已退役的分布式控制完成。

生命周期编排属于 `daemon/mod.rs`，原生控制／就绪属于 `runtime.rs`，流式转发属于 `daemon/api.rs`。
