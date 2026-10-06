# 单机 daemon 运维

重启时保留同一个私有状态目录和兼容的 Podman 配置。daemon 持有本机沙箱记录与删除意图，不恢复分布式任务，也不重放全局 DAG。

## 查询与删除 {#lifecycle}

使用[安装指南](index.md#start)中启动的带认证生命周期 API：

| 请求 | 含义 |
| --- | --- |
| `GET /v1/sandboxes` | 最近持久化的观察；支持重复 state 筛选、SDK 编码的 metadata 与 page/pageSize |
| `GET /v1/sandboxes/{id}` | 与本机原生状态核对该沙箱 |
| `DELETE /v1/sandboxes/{id}` | 确认原生删除后才释放预留；成功返回 204 |
| `POST /v1/sandboxes/{id}/pause` | 确认原生 cgroup freeze；202 空响应体 |
| `POST /v1/sandboxes/{id}/resume` | 确认原生 cgroup unfreeze；202 空响应体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 用未来 RFC3339 时间 `expiresAt` 延长已有 TTL |

将 `{id}` 替换为创建/列表返回的 sandbox ID。HTTP 断开不会取消已接受的操作。不同沙箱有独立生命周期锁；同一沙箱的控制与连接建立串行协调。

丢失创建响应后不要盲目重复创建：此 API 不是旧任务 ID 幂等协议。先检查保留状态。创建报告原生结果不确定时，保留错误消息中的 sandbox ID，再查询/删除它。创建失败且清理确认完成才释放准入；清理不确定则保留记录与预留。

## 端点认证 {#endpoints}

通过 `GET /v1/sandboxes/{id}/endpoints/44772` 解析 execd，端口 `18080` 解析 egress。返回的 authority 不含 URL scheme，流量经过 daemon，不直接指向容器 loopback 端口。

- 设置 `use_server_proxy=true` 时，路由请求使用生命周期 `OPEN-SANDBOX-API-KEY` header 认证。
- 默认模式在端点 `headers` 中提供随机、沙箱范围的 `X-PVISOR-SANDBOX-TOKEN`。客户端必须保留这些 header；该 token 不授权生命周期操作或访问其他沙箱。
- 转发到 guest 前会移除控制凭据。调用方提供的 guest execd access token 不会被生命周期 key 替换。

不支持签名端点过期、任意应用端口、WebSocket 或 CONNECT。请求/响应流式转发，包括 multipart 与 SSE。上传加等待 upstream header 的上限为 120 秒；已建立的响应流没有总超时。输出/流量配额仍由 upstream 服务负责。

## 过期与停止 {#expiration}

创建 `timeout` 可选，省略/null 表示手动清理。配置 TTL 至少 60 秒，不超过 `--max-timeout-seconds`（默认一天，最高可配置一年）。续期必须延长已有期限。

过期清理是 **best effort**，不是硬执行截止时间。原生命令、上传/header 等待和同一沙箱的控制可能推迟删除；过期后拒绝新的代理请求。续期与过期检查共用生命周期锁，过时扫描不会删除已延长期限的沙箱。

正常停止 daemon 会保留原生沙箱与持久记录供重启使用。重启后恢复 maintenance；daemon 停机期间不保证清理。需要硬截止或停机清理时，另行配置原生宿主监督。退役部署前显式删除沙箱，确认原生清理，再停止对应 daemon。

## 重启与状态所有权 {#restart}

一个 daemon 独占锁定自己的私有状态目录。在原生创建/控制之前持久化 owner identity 与记录。原生操作核验 owner/sandbox labels，随机 sandbox ID 不复用；labels 不能防御同一宿主 UID 下的其他进程。

待删除意图跨重启保留，不会被原生观察覆盖。删除状态未知时保留预留。原生沙箱丢失后仍以 Failed 显示，直到显式删除。maintenance 重试过期/待删除项；列表不是持续的原生进程监控，查询特定沙箱时使用 GET 核对。

不要删状态来修复错误，也不要让两个 daemon 共用它。状态保存原生所有权与未解决预留；把旧 Controller journal 搬进来不会迁移历史。注册表受配置容量和 16 MiB 上限约束，不是无限保留的分布式任务档案，也不是 Run Bundle/artifact store。

## 排障 {#troubleshooting}

| 现象 | 检查 |
| --- | --- |
| 绑定前启动失败 | Linux、可信 Podman 绝对路径、rootless、cgroup v2 与委派 CPU/memory/PID controllers |
| 状态目录忙 | 其他 daemon 持有独占锁；不要删锁或状态绕过 |
| 镜像不可用 | 先在本机准备镜像；不支持自动 pull 或 image auth |
| SDK 创建无法就绪 | 真实 execd 初始化与 `/ping`/`/ready`、egress `/healthz` 及无 capability egress；普通镜像不够 |
| 外部端点不可达 | TLS 代理路由、domain/protocol、`--public-endpoint` 与端点提供的 headers |
| 失败/pause 后准入仍满 | Failed/uncertain/paused 保留硬预留；查询并确认删除 |
| 过期沙箱仍存在 | 清理是 best effort；检查 daemon 在线时间、生命周期锁与原生清理错误 |
| 策略/template/volume 被拒绝 | 不支持的选项被拒绝，不会静默安装或模拟 |

API 错误包含 `{code, message}`，响应包含 `X-Request-ID`。保留 HTTP status、request ID 和脱敏诊断；API 成功不是命令退出状态。见[退出码与错误](../../reference/exit-codes.md#daemon)。
