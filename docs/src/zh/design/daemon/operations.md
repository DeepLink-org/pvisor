# Daemon 运维

使用原生 VM 运行时、私有状态与显式资源容量运行单节点 sandbox 服务。直接调用 `pvisor-daemon`。它拥有 sandbox 服务及可选独立池组件；`pvisor cache` 提供缓存客户端命令，`pvisor-daemon serve --cache` 提供缓存服务。

## 部署 {#deployment}

平台前提（Linux x86_64、`/dev/kvm`、委派的 cgroup v2 层级）与检查方式见[运维指南的前提条件](../../guides/daemon/index.md#prerequisites)。

将 `OPEN_SANDBOX_API_KEY` 设置为至少 32 字节的受保护随机秘密，不放入 argv。

`serve --sandbox` 使用 `--images-dir` 与 `--cgroup-root` 构造 `NativeRuntime`。它在独立 supervisor 中启动运行时；内部 VM 派发在 Tokio 之前完成。先按[操作指南](../../guides/daemon/index.md#install)准备镜像和 API key，再启动服务：

```sh
pvisor-daemon protocol
pvisor-daemon serve --sandbox \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

重启保持状态与运行时配置兼容。非 loopback 暴露前在可信反向代理终止 TLS。`--public-endpoint` 设置外部路由可达的 host:port authority；通配监听必须提供它。Loopback 临时端口会公布实际分配地址；非 loopback 监听默认拒绝，只有显式 `--allow-insecure-listen` 才允许远程明文绑定。

运行时与状态路径的绝对路径、长度与持久性约束见[运维指南的安装与启动](../../guides/daemon/index.md#install)。

已有记录的 v1 registry 或 v2 header 缺少原生 `owner.json` marker 时拒绝启动，即使 map 为空；它可能仍持有活动 Podman 容器。保留原始归属状态，使用全新原生状态或经旧 Podman daemon 清理后切换，不要删除记录／预留或伪造 marker。拒绝与迁移规则见[存储](storage.md#migration)。

## 预制镜像合同 {#image-contract}

`prepare-image` 将 registry 镜像准备到共享压缩池，固定 revision 后原子发布本机模板 manifest。Create 只打开该 revision 的 lazy projection，保留其 owner 到 VM 清理完成；完整解压和 registry pull 不在创建路径上。模板 key、准备命令与 SDK 示例见[操作指南](../../guides/daemon/index.md#install)。

新模板默认 `native_guest: true`：`pvisor-guest` 的 PID 1、进程组和 reaper 管理工作负载；主机 `native/opensandbox.rs` 或 `native/e2b.rs` 适配 HTTP/SSE、JSON/Connect，经 host-CID-only vsock 1025 调用 guest 进程和文件引擎。创建就绪检查绑定 sandbox/generation，主 entrypoint 退出会结束沙箱；没有宿主执行降级。

显式 `--execd` 或 `--envd` 保留上游兼容后端。准备器验证静态 Linux ELF 并注入受管理 bootstrap，由私有握手初始化真实服务并验证绑定、鉴权与就绪。旧独立 rootfs manifest 不自动转换：其 bootstrap 必须继续提供对应 guest 服务和 vsock bridge。文件格式与旧后端要求见[crate 合同](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md#runtime-boundary-native-vm-and-prepared-images)。

模板与宿主侧代码都属于可信输入；ELF/摘要检查不证明第三方程序来源安全。准备发布失败时保留旧 manifest；若 rename 或目录同步结果不确定，先检查已发布状态再重试。

## 部分协议 Profile {#api}

OpenSandbox 基线：**1.1.0**、`release-1.1.0`、commit `b1a29cf93a823a95913f7943010febb3f29de05c`；升级需显式审查 schema、SDK 与测试。

| 接口 | 当前行为 |
| --- | --- |
| `POST /v1/sandboxes` | 镜像/argv/env/metadata、CPU/内存硬限制、可选 TTL；202 JSON |
| `GET /v1/sandboxes` | 重复 states、SDK 编码 metadata、page/pageSize；最近持久观察 |
| `GET /v1/sandboxes/{id}` | 对账原生状态；200 JSON |
| `DELETE /v1/sandboxes/{id}` | 确认删除后释放；204 |
| `POST /v1/sandboxes/{id}/pause`、`/resume` | 确认同一 Attempt 的 live vCPU pause/resume；202 空体 |
| `POST /v1/sandboxes/{id}/renew-expiration` | 未来 RFC3339 截止时间，延长已有 TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | 通过 daemon authority 发布实际 execd/egress |
| 数据面 | 默认 guest 引擎与主机协议适配；显式上游模板使用代理 |

生命周期请求使用 `OPEN-SANDBOX-API-KEY`。响应含 `X-Request-ID`，错误为 `{code, message}`。Server-proxy 端点使用生命周期 key；默认端点提供 sandbox 范围的 `X-PVISOR-SANDBOX-TOKEN` headers，SDK 必须保留它们。Token 不能控制生命周期或访问另一个 sandbox。控制秘密向上游转发前被移除，但不替代调用方提供的 execd 访问凭据。

不支持 snapshot、template/pool、metadata 修改、hook、network policy、credential proxy、secure access、volumes、image auth、任意端口、signed endpoint、WebSocket 和 CONNECT。运行时已接入原生 VM 执行；stage/apply、checkpoint/fork 与 offload API 未实现。

同一服务还支持 E2B Python SDK **2.59.0** 的本机部分 profile，使用独立的 `/sandboxes`、`/v2/sandboxes` 与 `/e2b/*` 路由。协议与认证配置见[E2B 接入](../../guides/daemon/index.md#e2b)。本机 `prepare-image` 模板准备和 SDK 模板构建 API 是不同接口。

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
| 就绪／端点失败 | 按模板后端检查 guest 绑定、主进程与认证；显式上游模板另查服务初始化和 bridge |

## 证据范围 {#validation}

常规 Rust 测试覆盖准入、生命周期、重启、TTL、鉴权、schema、状态迁移、原生 IPC/cgroup 身份与清理。真实 VM 的官方 SDK 检查覆盖 OpenSandbox 1.1.0 和 E2B Python 2.59.0 的创建、并发沙箱、命令、文件、pause/resume、鉴权和删除；独立重启检查验证相同 VM、文件与活动进程跨 API 重启保留。

验收入口为 `crates/pvisor-daemon/tests/opensandbox_sdk.py`、`e2b_sdk.py` 与 `native_sdk_restart.py`，运行前需要隔离的 daemon、已准备模板和真实 KVM/cgroup 环境；命令见[crate 测试说明](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md#validation)。普通 `mise run test -- pvisor-daemon` 不会自动执行这些 SDK 驱动。

这些结果限定于列出的核心 profile。完整 SDK 一致性、生产密度、网络策略与敌对多用户安全仍需各自验证；旧 Cluster 测量不能替代当前 daemon 的证据。
