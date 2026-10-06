# 单机 daemon 运维

重启时保留同一个私有状态目录和不变的原生运行时绝对路径／cgroup 身份。daemon 持有本机沙箱记录与删除意图，不恢复分布式任务，也不重放全局 DAG。

## 查询与删除 {#lifecycle}

使用[安装指南](index.md#start)中启动的带认证生命周期 API：

| 请求 | 含义 |
| --- | --- |
| `GET /v1/sandboxes` | 最近持久化的观察；支持重复 state 筛选、SDK 编码的 metadata 与 page/pageSize |
| `GET /v1/sandboxes/{id}` | 与本机原生状态核对该沙箱 |
| `DELETE /v1/sandboxes/{id}` | 确认原生删除后才释放预留；成功返回 204 |
| `POST /v1/sandboxes/{id}/pause` | 确认已确认的 live vCPU pause；202 空响应体 |
| `POST /v1/sandboxes/{id}/resume` | 确认已确认的 live vCPU resume；202 空响应体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 用未来 RFC3339 时间 `expiresAt` 延长已有 TTL |

将 `{id}` 替换为创建/列表返回的 sandbox ID。HTTP 断开不会取消已接受的操作。不同沙箱有独立生命周期锁；同一沙箱的控制与连接建立串行协调。

丢失创建响应后不要盲目重复创建：此 API 不是旧任务 ID 幂等协议。先检查保留状态。创建报告原生结果不确定时，保留错误消息中的 sandbox ID，再查询/删除它。创建失败且清理确认完成才释放准入；清理不确定则保留记录与预留。

## 端点认证 {#endpoints}

通过 `GET /v1/sandboxes/{id}/endpoints/44772` 解析 execd，端口 `18080` 解析 egress。返回的 authority 不含 URL scheme，流量经过 daemon，不直接指向supervisor loopback 发布。

- 设置 `use_server_proxy=true` 时，路由请求使用生命周期 `OPEN-SANDBOX-API-KEY` header 认证。
- 默认模式在端点 `headers` 中提供随机、沙箱范围的 `X-PVISOR-SANDBOX-TOKEN`。客户端必须保留这些 header；该 token 不授权生命周期操作或访问其他沙箱。
- 转发到 guest 前会移除控制凭据。调用方提供的 guest execd access token 不会被生命周期 key 替换。

不支持签名端点过期、任意应用端口、WebSocket 或 CONNECT。请求/响应流式转发，包括 multipart 与 SSE。上传加等待 upstream header 的上限为 120 秒；已建立的响应流没有总超时。输出/流量配额仍由 upstream 服务负责。

## 过期与停止 {#expiration}

创建 `timeout` 可选，省略/null 表示手动清理。配置 TTL 至少 60 秒，不超过 `--max-timeout-seconds`（默认一天，最高可配置一年）。续期必须延长已有期限。

过期清理是 **best effort**，不是硬执行截止时间。原生命令、上传/header 等待和同一沙箱的控制可能推迟删除；过期后拒绝新的代理请求。续期与过期检查共用生命周期锁，过时扫描不会删除已延长期限的沙箱。

正常停止 daemon 会保留原生沙箱与持久记录供重启使用。重启后恢复 maintenance；daemon 停机期间不保证清理。需要硬截止或停机清理时，另行配置原生宿主监督。退役部署前显式删除沙箱，确认原生清理，再停止对应 daemon。

## 重启与状态所有权 {#restart}

一个 daemon 独占私有状态；原生创建／控制前持久化 owner 身份。私有 IPC 校验同 UID peer、owner、sandbox ID、generation 与秘密 token；持久身份绑定 boot ID 和 cgroup device/inode。ID 不复用、不重新启动。IPC 丢失表示不确定，不是 Missing 或清理证据。持久删除意图与 supervisor 独占锁阻止迟到启动；清理使用身份绑定的 `cgroup.kill`，不保存 PID 或按 PID kill，确认 cgroup 为空且 owner 锁释放后才释放容量。同一 boot 下没有持久 tombstone 证据的 cgroup 被替换或丢失不证明对象不存在。

待删除意图跨重启保留，不会被原生观察覆盖。删除状态未知时保留预留。原生沙箱丢失后仍以 Failed 显示，直到显式删除。maintenance 重试过期/待删除项；列表不是持续的原生进程监控，查询特定沙箱时使用 GET 核对。

不要删状态来修复错误，也不要让两个 daemon 共用它。状态保存原生所有权与未解决预留；把旧 Controller journal 搬进来不会迁移历史。注册表受配置容量和总计 16 MiB 逻辑版本 1 等价预算约束，不计逐文件重复的 owner 信封；这不是物理目录大小上限。它不是无限保留的分布式任务档案，也不是 Run Bundle/artifact store。

删除／对账可在原 rootfs 或 firmware 缺失时继续基于 tombstone 的清理，回收空的所属 cgroup 和私有 run/RAM/temp/socket／秘密记录，保留最小 ID 屏障。回收错误保留预留供重试；不要自行删除 tombstone 或 registry 状态。见[存储](../../design/daemon/storage.md#gc)。

版本 1 中已有记录的 `sandboxes.json` 若缺少原生 `owner.json` marker，会在既有 store 独占锁内被拒绝：它可能仍持有活动 Podman 容器。使用全新原生状态并保留／清理旧部署，或通过旧 Podman daemon 删除全部 sandbox、确认清理后，再用已清空的 registry 切换后端。不要删除 registry 条目、预留或所有权状态，也不要伪造原生 marker 绕过检查。原生 daemon 不会把这些容器接管为 Missing 或静默释放其预留。

版本 2 的 `sandboxes.json` 只保留 owner/header 与空 map；私有 `records/meta.json` 和带 owner 信封的逐 sandbox 文件持有清单。它们须与原生 `owner.json` 一同保留。修改只提交目标记录，v1 只在运行时接受 owner 后迁移；见[存储](../../design/daemon/storage.md#commit)与[激活](../../design/daemon/storage.md#migration)。

## 排障 {#troubleshooting}

| 现象 | 检查 |
| --- | --- |
| 绑定前启动失败 | Linux x86_64/KVM、短绝对状态路径、可信 manifest 和真实委派 cgroup v2 |
| 已有记录的 v1 registry 缺原生 `owner.json` | 保留旧状态／容器；使用全新原生状态，或通过旧 Podman daemon 清理全部 sandbox 后切换 |
| v2 header 缺原生 `owner.json`，即使 map 为空 | 恢复原始归属状态；不要删除 `records/`、预留或伪造 marker |
| 状态目录忙 | 其他 daemon 持有独占锁；不要删锁或状态绕过 |
| 镜像不可用 | 先在本机准备镜像；不支持自动 pull 或 image auth |
| SDK 创建无法就绪 | 真实 execd 初始化与 `/ping`/`/ready`、egress `/healthz` 及 guest CID 3 的 44772/18080 vsock bridge；普通 rootfs 不够 |
| 外部端点不可达 | TLS 代理路由、domain/protocol、`--public-endpoint` 与端点提供的 headers |
| 失败/pause 后准入仍满 | Failed/uncertain/paused 保留硬预留；查询并确认删除 |
| 过期沙箱仍存在 | 清理是 best effort；检查 daemon 在线时间、生命周期锁与原生清理错误 |
| 策略/template/volume 被拒绝 | 不支持的选项被拒绝，不会静默安装或模拟 |

API 错误包含 `{code, message}`，响应包含 `X-Request-ID`。保留 HTTP status、request ID 和脱敏诊断；API 成功不是命令退出状态。见[退出码与错误](../../reference/exit-codes.md#daemon)。
