# 本机 Sandbox 准入

创建原生 VM 前，限制单机已接受 sandbox 的资源总量。Daemon 按硬限制之和准入，不排队、不选择主机，也不从低 RSS 样本推断可用容量。

## 请求校验 {#model}

创建必须在 `image.uri` 提供本机 prepared-image key、非空 argv，以及只含 `cpu` 和 `memory` 的 `resourceLimits`。CPU 接受整数／小数核或 millicore；内存接受字节及支持的十进制／二进制后缀。资源量必须为正、可精确表示，并检查溢出。

```json
{
  "image": {"uri": "prepared-agent-1.1.0"},
  "entrypoint": ["sleep", "600"],
  "resourceLimits": {"cpu": "500m", "memory": "256Mi"},
  "timeout": 600,
  "metadata": {"purpose": "local-example"}
}
```

这是请求格式示例，不是已验证镜像配方。Manifest `entrypoint` bootstrap 必须监督这些参数及所需服务，合同见[运维](operations.md#image-contract)。创建期间不拉取镜像。

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

要求 Linux x86_64、可用 `/dev/kvm`、可信绝对路径，以及可写、已委派且启用 CPU/memory/PID controller 与 `cgroup.kill` 的 cgroup v2 层级。前置检查验证真实 controller 写入和 KVM API；没有 host、OCI 命令或 registry-pull 降级。

原生 supervisor 在独立、脱离 daemon 生命周期的子进程中嵌入 `pvisor::PVisor`，只配置 `VmExecutor` 并持有 RunHandle。Sandbox cgroup 限制整个 supervisor/VMM/helper 树：总 CPU 速率 **10–8000 millicores**、硬内存、零 swap、`pids.max=512` 与 group OOM。vCPU 数按 quota 向上取整（最多 8），guest RAM 向下取整到 MiB；硬内存上限还包含宿主侧开销。生命周期／端点观察会重新核对限制。Paused、Failed 和不确定记录继续保守预留资源。

启动 callback 在 owner 锁内检查 started／删除／tombstone marker，通过预先打开的 `cgroup.procs` FD 在 **exec 之前**加入身份绑定的 cgroup。Supervisor 启动时核验成员关系，不迁移已运行的 Tokio 进程；supervisor/Tokio 分配与后续 VM/helper 子进程均计入 sandbox 预算。直接在该 cgroup 外调用隐藏命令会失败关闭。

逻辑准入总量、逐 supervisor/VM 树已安装限制和全节点物理占用是不同量。Daemon、helper、缓存及宿主需要 sandbox 限制之外的余量。没有隐式 CPU/RAM 超卖、基于压力的准入或已测量的全节点物理预算。原生[共享工作集](shared-working-set.md)不授权降低 daemon 预留。

校验与资源算术属于 `daemon/models.rs`，串行预留插入属于 `daemon/mod.rs`，原生 KVM/cgroup 前置检查与控制属于 `runtime.rs`。
