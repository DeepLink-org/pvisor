# 尚未准备的启动实验：guest_init

Benchmark: B-STARTUP-ENG ([注册表](../REGISTRY.md#b-startup-eng)), role engineering A/B.

## Motivation

判断 guest init 实现变化是否改变启动等待。

期望结论：同输入就绪中位数差异及 95% bootstrap 区间。

## Experiment design

目标协议（尚未接线）：相同 runner、rootfs 与 payload，随机交错实现，不作为跨产品用户对照。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=guest-init -->
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

`semaspec filter guest_init.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
