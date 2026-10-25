# 尚未准备的启动实验：kernel

Benchmark: B-KERNEL-ENG ([注册表](../REGISTRY.md#b-kernel-eng)), role engineering A/B.

## Motivation

判断裁剪 guest 内核的收益，以及它是否仍支持工作区、开发工具和网络所需能力。

期望结论：相同 pVisor 二进制、相同 Linux 源码和补丁、相同打包方式，仅配置不同的两份新固件，其就绪/完整任务中位数差及 95% 配对 bootstrap 区间；列出减少的功能与未验证能力。

## Experiment design

目标协议（尚未接线）：冻结固件源码、输入 tarball、补丁、配置与编译记录，分别重建通用配置和裁剪配置；不复用旧固件作为基线。先验证构建和同样负载的输出、暂存隔离与工作区内容；Linux KVM 上以相同两核/内存/工具输入，随机交替 shell-ready、七项文件工具和修复任务，每格 30 次、3 次预热。CPU 缓解、seccomp、namespaces、virtio-fs 等所需配置必须保留；网络/恢复能力需独立正确性检查，不能由 shell 启动通过推导。配置中 guest LSM/设备功能差异必须明确，不称为相同加固程度的排名。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=kernel -->
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

`semaspec filter kernel.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
