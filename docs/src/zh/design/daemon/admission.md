# 本机 Sandbox 准入

调用 Podman 前，限制单机已接受 sandbox 的资源总量。Daemon 按硬限制之和准入，不排队、不选择主机，也不从低 RSS 样本推断可用容量。

## 请求校验 {#model}

创建必须提供镜像 URI、非空 argv，以及只含 `cpu` 和 `memory` 的 `resourceLimits`。CPU 接受整数／小数核或 millicore；内存接受字节及支持的十进制／二进制后缀。资源量必须为正、可精确表示，并检查溢出。

```json
{
  "image": {"uri": "localhost/prepared-agent:1.1.0"},
  "entrypoint": ["sleep", "600"],
  "resourceLimits": {"cpu": "500m", "memory": "256Mi"},
  "timeout": 600,
  "metadata": {"purpose": "local-example"}
}
```

这是请求格式示例，不是已验证镜像配方。镜像 ENTRYPOINT 必须监督这些参数及所需服务，合同见[运维](operations.md#image-contract)。创建期间不拉取镜像。

可选 platform 必须与本机 Linux 架构一致。不支持的 snapshot/template、resource requests、network policy、credential proxy、secure access、volumes、镜像认证、lifecycle hooks 和非空 extensions 在原生创建前失败。环境与 metadata map 有界且经过校验。

## 容量记账 {#admission}

| 边界 | 准入规则 |
| --- | --- |
| Sandbox 数 | Registry 记录数不得超过 `max_sandboxes` |
| CPU | 全部记录的 `cpu_millis` 与新请求之和不超过配置容量 |
| 内存 | 全部记录的 `memory_bytes` 与新请求之和不超过配置容量 |

串行的持久插入构成预留屏障，并发创建不能重复消费容量。拒绝返回 `429 CAPACITY_EXCEEDED`，不会生成等待执行的分配。没有租户配额或经鉴权的逐租户资源账户。

Paused、Terminated、Failed 和不确定记录保留完整 CPU/内存/数量记账，直到显式删除，或失败创建已确认清理。Pause **不释放**逻辑 CPU。原生删除及不存在确认先于持久记录移除；未知清理不能形成可复用容量。

## 实际控制与物理内存 {#reservations}

Linux 后端要求 rootless Podman、cgroup v2 和已委托的 CPU、memory、PID controller。它安装 CPU quota、内存与 swap 硬设置、PID 限制、私有 namespace、`no-new-privileges` 和 `cap-drop=ALL`，再核对容器配置中的资源设置。前提失败不会降级到 rootful 或 host。

逻辑准入总量、逐容器已安装限制和全节点物理占用是不同量。Daemon、helper、缓存及宿主需要 sandbox 限制之外的余量。没有隐式 CPU/RAM 超卖、基于压力的准入或已测量的全节点物理预算。原生[共享工作集](shared-working-set.md)不授权降低 daemon 预留。

校验与资源算术属于 `daemon/models.rs`，串行预留插入属于 `daemon/mod.rs`，Podman 前置检查与控制配置属于 `runtime.rs`。
