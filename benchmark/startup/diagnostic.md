# 尚未准备的启动实验：diagnostic

Benchmark: B-STARTUP-DIAG ([注册表](../REGISTRY.md#b-startup-diag)), role diagnostic.

## Motivation

定位内核、初始化和宿主启动/收尾阶段对就绪时间及正常退出的贡献。

期望结论：各阶段计时及计时边界，不作为用户启动水位。

## Experiment design

目标协议（尚未接线）：固定 guest payload 与 runner；早期内核时钟和收尾时间分别解释。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=diagnostic -->
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

当前阶段诊断工具 `startup_profile.py --collect NEW_DIR -- COMMAND` 可按
[手册](README.md#启动瓶颈诊断b-startup-diag) 独立采集，`startup_profile.py LOG...` 可离线汇总；
这条诊断路径不表示上面的通用行计时入口已经准备。其时钟、输出与失败契约由
[checks.md](checks.md) 的 `S-STARTUP-004` 经 semaspec 验证。

## Data and analysis

`semaspec filter diagnostic.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
