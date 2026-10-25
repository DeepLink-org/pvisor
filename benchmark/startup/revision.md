# 尚未准备的启动实验：revision

Benchmark: B-STARTUP-REVISION-ENG ([注册表](../REGISTRY.md#b-startup-revision-eng)), role engineering A/B.

## Motivation

定位指定提交与父提交或已归档历史版本是否改变首条有效输出和正常退出等待。

期望结论：host、staged、VM 的 Ready / Exit 统计及同轮中位数差的 95% 配对 bootstrap 区间；不能用跨批次数字归因。

## Experiment design

目标协议（尚未接线）：精确源码归档、相同 release/musl/gateway 构建参数和冻结内核；相同 rootfs、探针、CPU 0,1、2 vCPU / 128 MiB、热缓存。各条件 3 次预热、60 对正式样本，固定 seed 随机交替两个版本；Ready / Exit 同次采样。版本在独立 user/mount/PID namespace 中运行，私有 Host 状态目录，退出 namespace 时销毁自有服务；不重启宿主已有 Host。复用当前启动 observer 与完整正确性门禁，保留全部有效慢样本，无事后剔除。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=revision -->
```json
{
  "baseline": {
    "Ready median ms": null,
    "Exit median ms": null
  },
  "candidate": {
    "Ready median ms": null,
    "Exit median ms": null
  }
}
```

## Data and analysis

`semaspec filter revision.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
