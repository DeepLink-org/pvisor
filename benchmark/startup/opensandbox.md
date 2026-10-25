# 尚未准备的启动实验：opensandbox

Benchmark: B-OPENSANDBOX-ENG ([注册表](../REGISTRY.md#b-opensandbox-eng)), role engineering A/B.

## Motivation

判断本地 pVisor OpenSandbox 后端是否能缩短 SDK 创建到可执行命令的等待。

期望结论：在预备好的同 digest Ubuntu、1 CPU/512 MiB、同机 SDK 1.1.0 下，报告 native VM 与官方 Docker server 的就绪延迟分布、差异区间及失败；明确 native guest 或旧版同字节 execd 配置，不混合批次，不推断完整协议、网络策略、生产密度或隔离等级等价。

## Experiment design

目标协议（尚未接线）：服务与镜像预先准备，另列首个请求；固定种子随机交替，每臂至少 20 次新建沙箱。服务重启首个请求另用 20 次独立服务进程重启配对，不包含服务启动时间，也不称为磁盘冷缓存。计时 SDK create 返回，再单列首次命令完成时间；必须校验 stdout、私有文件及删除成功，完整保留失败，不剔除慢样本。所有请求相同资源/argv/环境，包含各后端实际 admission/readiness 与 SDK 开销。VM 的 cgroup 限额覆盖 VMM/supervisor，Docker 限额覆盖容器；这是各自本地部署配置，不能视为相同内核隔离或同等可用 guest RAM。无 Docker 基线时只能报告 native 单臂诊断。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=opensandbox -->
```json
{
  "unprepared": {
    "N": null,
    "Median ms": null,
    "Mean ms": null,
    "SD ms": null,
    "Min ms": null,
    "Max ms": null
  }
}
```

## Data and analysis

`semaspec filter opensandbox.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
