# 尚未准备的启动实验：sandbox_engine

Benchmark: B-SANDBOX-ENGINE-ENG ([注册表](../REGISTRY.md#b-sandbox-engine-eng)), role engineering A/B.

## Motivation

判断以共享 Rust guest 引擎替代每 VM 官方 execd/envd 后，SDK 首次/稳态创建与命令完成是否改善。

期望结论：同机、同预算、同 SDK 的旧制品/官方服务与新制品/原生引擎配对中位数差异、变化比例及 95% bootstrap 区间；OpenSandbox 与 E2B 分列，不外推 Docker 或完整协议性能。

## Experiment design

目标协议（尚未接线）：冻结已有 performance/musl 制品、源码收据及同 digest Ubuntu 来源模板；两臂使用同一共享压缩池、同文件系统、整棵 supervisor/VMM 进程树 1 CPU/512 MiB、同 guest 1 vCPU。固定 seed 随机交替两臂，3 对预检/预热和 20 对正式轮次；每臂每轮新启 API 进程，分别记录其首个请求与随后的稳态请求，每个请求创建全新 VM。排除镜像准备、API 进程启动及 OpenSandbox transport 对象构造；E2B 记录 SDK create 的实际完整调用口径。相同 argv/环境/命令，计时 create、创建后的首/次命令及同次 create→首条正确命令完成。校验 stdout、私有文件、实际 cgroup 上限及 GET 404 删除确认；失败完整保留，不剔除慢有效样本。记录宿主负载和并发任务，不更改全局策略或停止无关服务；分布分离时列出簇比例，不以单个 P50 掩盖。实现、启动服务和模板内容共同变化，结果只归因于完整产品路径，不声称纯 Rust/Go 语言或单一机制贡献。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=sandbox-engine -->
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

`semaspec filter sandbox_engine.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
