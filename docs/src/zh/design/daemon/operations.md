# Daemon 运维

使用原生 VM 运行时、私有状态与显式资源容量运行单节点 sandbox 服务。直接调用 `pvisor-daemon`。它拥有 sandbox 服务及可选独立池组件；`pvisor-cache` 保持独立。

## 部署 {#deployment}

平台前提（Linux x86_64、`/dev/kvm`、委派的 cgroup v2 层级）与检查方式见[运维指南的前提条件](../../guides/daemon/index.md#prerequisites)。

将 `OPEN_SANDBOX_API_KEY` 设置为至少 32 字节的受保护随机秘密，不放入 argv。

`serve` 使用已实现且必需的 `--images-dir` 与 `--cgroup-root` 构造 `NativeRuntime`。Cargo 链接 `pvisor` 与 `pvisor-core`；同步 `main` 在参数解析或 Tokio 之前调用 `pvisor::run_krun_internal_if_requested()`，随后派发隐藏的 `native-supervisor --sandbox-dir ABSOLUTE_PATH` 命令。下方部署示例使用当前 CLI，但不提供或验证 guest bootstrap、SDK 兼容或密度。

```sh
pvisor-daemon protocol
pvisor-daemon serve \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

重启保持状态与运行时配置兼容。非 loopback 暴露前在可信反向代理终止 TLS。`--public-endpoint` 设置外部路由可达的 host:port authority；通配或临时端口监听必须提供它。自动端点发布未实现。

运行时与状态路径的绝对路径、长度与持久性约束见[运维指南的安装与启动](../../guides/daemon/index.md#install)。

已有记录的 v1 registry 或 v2 header 缺少原生 `owner.json` marker 时拒绝启动，即使 map 为空；它可能仍持有活动 Podman 容器。保留原始归属状态，使用全新原生状态或经旧 Podman daemon 清理后切换，不要删除记录／预留或伪造 marker。拒绝与迁移规则见[存储](storage.md#migration)。

## 预制镜像合同 {#image-contract}

镜像 manifest 的字段与环境语义见[运维指南的前提条件](../../guides/daemon/index.md#prerequisites)。

长期运行的 guest bootstrap 必须监督工作负载、真实 OpenSandbox 1.1.0 execd 与 egress，自行初始化／鉴权服务、转发信号并回收子进程。它必须在 guest **CID 3** 的 **44772/18080** 端口提供字节透明的 AF_VSOCK listener，桥接真实服务。Supervisor 的 loopback TCP 发布经私有 Unix socket 与原生 vsock 转发连接 guest。普通 rootfs 或只运行 sleep 的进程不够。

Create、对 Running VM 的 Inspect 与 resume 就绪检查经 bridge 要求真实 HTTP 200 的 execd `/ping`、JSON `initialized: true` 的 `/ready` 和 egress `/healthz`，响应体有界。端点解析认证 live Running 状态并检查删除屏障，不为每次数据请求重复完整 health/cgroup 检查。daemon 不注入／初始化 execd，也不合成 command/SSE/file 响应。Python SDK 初始化即使没有网络策略也会解析两个端点。

Bootstrap 与镜像配方**未提供，也未经端到端验证**。旧容器的 `cap-drop=ALL` 限制不适用于此原生 VM 后端；upstream 镜像名称不是原生 bootstrap/vsock 适配器。不提供假就绪，也没有 SDK 兼容或密度证据。

## 部分协议 Profile {#api}

基线：OpenSandbox **1.1.0**、`release-1.1.0`、commit `b1a29cf93a823a95913f7943010febb3f29de05c`；升级需显式审查 schema、SDK 与测试。

| 接口 | 当前行为 |
| --- | --- |
| `POST /v1/sandboxes` | 镜像/argv/env/metadata、CPU/内存硬限制、可选 TTL；202 JSON |
| `GET /v1/sandboxes` | 重复 states、SDK 编码 metadata、page/pageSize；最近持久观察 |
| `GET /v1/sandboxes/{id}` | 对账原生状态；200 JSON |
| `DELETE /v1/sandboxes/{id}` | 确认删除后释放；204 |
| `POST /v1/sandboxes/{id}/pause`、`/resume` | 确认同一 Attempt 的 live vCPU pause/resume；202 空体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 未来 RFC3339 截止时间，延长已有 TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | 通过 daemon authority 发布实际 execd/egress |
| 数据面 | 流式转发预制服务，不重新实现其 API |

生命周期请求使用 `OPEN-SANDBOX-API-KEY`。响应含 `X-Request-ID`，错误为 `{code, message}`。Server-proxy 端点使用生命周期 key；默认端点提供 sandbox 范围的 `X-PVISOR-SANDBOX-TOKEN` headers，SDK 必须保留它们。Token 不能控制生命周期或访问另一个 sandbox。控制秘密向上游转发前被移除，但不替代调用方提供的 execd 访问凭据。

不支持 snapshot、template/pool、metadata 修改、hook、network policy、credential proxy、secure access、volumes、image auth、任意端口、signed endpoint、WebSocket 和 CONNECT。运行时已接入原生 VM 执行；stage/apply、checkpoint/fork 与 offload API 未实现。

## 信任边界 {#security}

原生 OverlayNet 提供 VM 出口网络；OpenSandbox 网络策略请求仍不支持并被拒绝，不等于 deny-all egress。宿主账户、daemon/firmware 和预制镜像属于可信输入；私有状态与同 UID IPC 不防御敌对宿主 UID/root 代码。其他本机用户可能访问 loopback 发布，仍需真实服务鉴权与宿主控制。秘密不进入 supervisor argv 或宿主环境，但保存在私有记录中。不承诺安全审计或敌对多用户隔离。

## 故障处理 {#runbook}

| 症状 | 操作 |
| --- | --- |
| 丢失创建／控制响应 | 检查已有记录／副作用；断连不取消已接受操作，创建没有重试幂等 key |
| 容量耗尽 | 检查保留记录；pause 或低 RSS 不释放预留 |
| Failed／原生 sandbox 缺失 | 保留记录，排查运行时，显式删除以释放资源 |
| 待删除／TTL 清理 | 恢复运行时访问；维护重试，但 TTL 不是硬截止 |
| 存储提交不确定／registry 损坏 | 保留目录，修复存储后重启，不以删除归属状态继续运行 |
| Daemon 关闭 | 独立原生 supervisor/VM 跨 daemon 重启保留；使用相同 owner/状态重启，或安排独立宿主清理 |
| 就绪／端点失败 | 检查真实服务与 guest bootstrap/vsock bridge，而非仅看 VM Running |

## 证据范围 {#validation}

源码测试覆盖 fake-runtime 准入／生命周期／重启／TTL、HTTP 鉴权/schema/过滤/端点及纯运行时解析/argv，不建立原生隔离、SDK 端到端兼容或密度收益。构建、测试、原生 sandbox 和 benchmark 都还没有验证当前 daemon。旧分布式 Cluster gate 和测量不验证该 daemon。
