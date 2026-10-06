# Daemon 运维

使用可信 Podman 绝对路径、私有持久状态目录和显式资源容量，运行单节点 sandbox 服务。直接调用 `pvisor-daemon`，不把预计接入的 CLI companion 作为部署前提。

## 部署 {#deployment}

要求 Linux、外部 rootless Podman、委托 CPU/memory/PID controller 的 cgroup v2，以及本机已预制镜像。通过秘密管理机制将 `OPEN_SANDBOX_API_KEY` 设置为至少 32 字节的强随机秘密，不放在 argv 中。

以下是部署示例，**不是经过测试的安装或 SDK 端到端配方**：

```sh
pvisor-daemon protocol
pvisor-daemon serve \
  --podman /usr/bin/podman \
  --listen 127.0.0.1:8080 \
  --state /var/tmp/pvisor-daemon-state \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

重启保持状态与运行时配置兼容。非 loopback 暴露前在可信反向代理终止 TLS。`--public-endpoint` 设置外部路由可达的 host:port authority；通配或临时端口监听必须提供它。自动端点发布未实现。

默认值：监听 `127.0.0.1:8080`，状态 `.pvisor/daemon`，32 个 sandbox、4000 CPU millis、8 GiB 准入内存，最大 TTL 86400 秒。容量不是完整服务组资源上限。

## 预制镜像合同 {#image-contract}

镜像本机预置，创建使用 `--pull=never`。ENTRYPOINT 必须监督替换 CMD 的请求 argv，无 shell 插值地执行、转发信号并回收子进程。同时运行端口 44772 的真实 **OpenSandbox 1.1.0 execd** 和端口 18080 的 egress 服务，自行完成初始化与服务鉴权。

创建探测 execd `/ping`、`/ready` 与 egress `/healthz`。Daemon 不注入 `/execd`、不执行其初始化握手，也不模拟 command/SSE/file 响应。普通发行版镜像运行 `tail` 不构成 SDK sandbox。

固定版本的上游默认 egress 使用 iptables redirect，不能在 `cap-drop=ALL` 下原样运行。必须部署真正无 capability 的实现，普通 sidecar 不受支持。尚无已验证的端到端预制镜像配方。合同不满足时，普通 SDK 创建／就绪会失败，不返回模拟就绪 sandbox。Python SDK 初始化即使未请求网络策略，也会解析两个端点。

## 部分协议 Profile {#api}

基线：OpenSandbox **1.1.0**、`release-1.1.0`、commit `b1a29cf93a823a95913f7943010febb3f29de05c`；升级需显式审查 schema、SDK 与测试。

| 接口 | 当前行为 |
| --- | --- |
| `POST /v1/sandboxes` | 镜像/argv/env/metadata、CPU/内存硬限制、可选 TTL；202 JSON |
| `GET /v1/sandboxes` | 重复 states、SDK 编码 metadata、page/pageSize；最近持久观察 |
| `GET /v1/sandboxes/{id}` | 对账原生状态；200 JSON |
| `DELETE /v1/sandboxes/{id}` | 确认删除后释放；204 |
| `POST /v1/sandboxes/{id}/pause`、`/resume` | 确认原生 freeze/unfreeze；202 空体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 未来 RFC3339 截止时间，延长已有 TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | 通过 daemon authority 发布实际 execd/egress |
| 数据面 | 流式转发预制服务，不重新实现其 API |

生命周期请求使用 `OPEN-SANDBOX-API-KEY`。响应含 `X-Request-ID`，错误为 `{code, message}`。Server-proxy 端点使用生命周期 key；默认端点提供 sandbox 范围的 `X-PVISOR-SANDBOX-TOKEN` headers，SDK 必须保留它们。Token 不能控制生命周期或访问另一个 sandbox。控制秘密向上游转发前被移除，但不替代调用方提供的 execd 访问凭据。

不支持 snapshot、template/pool、metadata 修改、hook、network policy、credential proxy、secure access、volumes、image auth、任意端口、signed endpoint、WebSocket 和 CONNECT。协议兼容性不会附带原生 VM/stage/offload 能力。

## 信任边界 {#security}

Rootless 容器共享宿主内核，不具有 VM 级边界，也未完成安全审计。宿主账户、Podman 配置/hook 和预制镜像是可信部分。Slirp4netns 禁止 host-loopback 访问，但不是 deny-all egress；请求的网络策略会被拒绝。其他本机用户仍可能访问原生 loopback 端口，多用户暴露需真实服务鉴权与宿主控制。

## 故障处理 {#runbook}

| 症状 | 操作 |
| --- | --- |
| 丢失创建／控制响应 | 检查已有记录／副作用；断连不取消已接受操作，创建没有重试幂等 key |
| 容量耗尽 | 检查保留记录；pause 或低 RSS 不释放预留 |
| Failed／原生 sandbox 缺失 | 保留记录，排查运行时，显式删除以释放资源 |
| 待删除／TTL 清理 | 恢复运行时访问；维护重试，但 TTL 不是硬截止 |
| 存储提交不确定／registry 损坏 | 保留目录，修复存储后重启，不以删除归属状态继续运行 |
| Daemon 关闭 | 容器继续存在；使用相同 owner/状态重启，或安排独立宿主清理 |
| 就绪／端点失败 | 检查真实服务与无 capability egress 合同，而非仅看容器 Running |

## 证据范围 {#validation}

源码测试覆盖 fake-runtime 准入／生命周期／重启／TTL、HTTP 鉴权/schema/过滤/端点及纯运行时解析/argv，不建立原生隔离、SDK 端到端兼容或密度收益。本次文档修改没有运行构建、测试、原生 sandbox 或 benchmark。旧分布式 Cluster gate 和测量不验证该 daemon。
