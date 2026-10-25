# 尚未准备的启动实验：lazy_startup

Benchmark: B-LAZY-STARTUP ([注册表](../REGISTRY.md#b-lazy-startup)), role user-facing.

## Motivation

频繁创建短任务环境时，用户需要判断客户端按需读取是否比完整拉取、解包更划算，以及镜像服务端准备成本如何摊销。

期望结论：同一固定 linux/amd64 digest、同一负载下（Ubuntu shell、Python/NumPy 脚本与可选 PyTorch CPU 导入分别成批，不合并），Docker 与 pVisor cache-service lazy VM 的客户端冷/热缓存 ready、completion 和传输量；明确服务端预准备成本，不解释为纯 VMM 差异。

## Experiment design

目标协议（尚未接线）：开源 Distribution registry 提供固定 OCI 镜像，pvisor-daemon serve --cache 提供同镜像文件索引和内容。两服务均为本机 loopback，无人工延迟时只能解释为本机模拟远程协议，不是 WAN。按独立网络条件分批；配置两核/2 GiB（容器硬限制与 VM guest RAM 不等价，Docker daemon/containerd 未作整机两核约束，必须披露），随机交替每组至少 30 次，冷客户端缓存后紧接热缓存，新 guest/容器与工作区。Ubuntu 负载校验发行版身份和 shell 输出；Python 负载禁用 bytecode 写入，固定单线程，校验 Python/库版本和确定性 NumPy 数组与矩阵运算（可选 PyTorch 还须校验 CPU-only build 和张量运算），ready 为全部校验后的唯一输出。两组均要求成功退出；失败单列，不剔除慢有效样本。预热/预检不纳入正式样本；非分离分布 P50、分离分布各簇比例和中位数、参考 P95、配对 bootstrap 95% CI。镜像服务端拉取/解包/索引时间单独保留，原始日志、源码和制品摘要放 `.data/`。不回答完整 OS 引导、真实 WAN、并发或大型任务性能。

注册需求与历史证据见注册表。当前通用测量入口尚未接通，表中 null 表示未测。`mise run benchmark` 必须拒绝执行，不能以旧数据或任意命令补齐。

<!-- semspec: table id=lazy-startup -->
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

`semaspec filter lazy_startup.md` 只渲染，显示 NOT RUN；不执行实验。
已准备的批次通过 `semaspec bench --cwd BATCH BATCH/in.json` 执行，之后必须运行对应审计。
本规格不填充历史成绩、不批准语义、不绕过注册或公开数据发布门禁。
