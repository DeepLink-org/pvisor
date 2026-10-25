# 尚未准备的启动实验：opensandbox_burst

Benchmark: B-OPENSANDBOX-BURST-DIAG ([注册表](../REGISTRY.md#b-opensandbox-burst-diag)), role diagnostic.

## Motivation

回答当前 OpenSandbox 接口同时收到 10 个创建请求时，整批到可用/首命令完成需要多久，并确认小内存设置不会耗尽宿主内存。

期望结论：分列每批全部 create 返回、全部首命令正确完成、每请求时延与发起偏移、实验 cgroup 内存峰值、宿主可用内存下界、失败和 OOM；只解释单机串行批次的突发场景，不作 A/B 加速、并发容量上限或吞吐排名。

## Experiment design

目标协议（尚未接线）：当前冻结 performance daemon、官方 SDK 1.1.0、同一预准备 native guest Ubuntu 模板和固件。每 VM 1 vCPU/128 MiB（包括 supervisor/VMM 的硬上限）；整个独立 delegated scope 限 4 CPU、memory.high=1.5 GiB、memory.max=2 GiB、swap=0，API admission 仅允许 10 个/1280 MiB，CPU 子配额总和 10 CPU 受父组 4 CPU 约束。先 1 个、2 个预检，再固定 5 批各 10 个；每批新起 API，以异步 barrier 同时放行已构造 transport 的 SDK create，完整记录实际发起偏移，API 启动不计时。创建完成后各自执行并校验独立文件与唯一 stdout，全部 VM 保持到共同完成点再删除，批间要求 inventory 空、子 cgroup 消失；全程采集 memory.current/peak/events、swap 与宿主 MemAvailable。开始时至少有 6 GiB 可用；运行中宿主低于 4 GiB、实验用量超过 1.75 GiB、任一请求失败或 OOM 即停止后续批次并清理本实验资源，失败完整保留，不换大预算重试同一批，不剔除慢请求。报告五个批次与范围，不以五批估计尾分位；内存峰值是本实验 cgroup 实际计费，既有宿主共享缓存可能在组外，不当作整机独占物理内存。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=opensandbox-burst -->
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

`semaspec filter opensandbox_burst.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
