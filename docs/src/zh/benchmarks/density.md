# 并发环境占多少资源，与 Podman 相比如何？

## 主要结论 {#conclusions}

**空闲探针中，staged 的 **128 路全部完成**，RSS 合计约 **1.56 GiB**；最小 shell VM 的 **32 路全部完成**，RSS 合计约 **3.02 GiB**。同条件 Podman 的 128 路也全部完成，但启动等待更长。safe 和完整工具 OCI 的部分高并发出现失败。这是空闲环境的占用与可靠性，不是真实 Agent 吞吐或容量承诺。**

| 需求 | 选型含义 |
|---|---|
| 大量空闲环境 | 按成功率和驻留资源规划 |
| 完整工具 OCI 环境 | 核对临时存储配额与失败 |
| 真实并行 Agent | 空闲探针不能作为容量保证 |

## Motivation {#motivation}

多 Agent 时，内存、启动和环境复制的成本会累积。并发成功率必须与资源一起报告，不能只给最快成功样本。

## 实验设计 {#interpretation}

每个 Job 输出 ready 后保持 1 秒；并发 1/8/128，每格 5 个批次，无预热。每 20 ms 采样拥有的进程树 RSS 峰值，记录子进程 CPU 与每 Job 总时间。VM 使用 2 vCPU/128 MiB；其他配置与文件系统表一致。它是占用探针，不是真实 Agent 推理或构建负载。只对全成功批次计算性能，成功/尝试数包含失败批次。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

| Environment | Backend | Concurrency | Completed/attempted | Full batches | RSS P50 MiB | CPU P50 ms/job | Job P50/P95 ms |
|---|---|---|---|---|---|---|---|
| Host workspace | native | 128 | 640/640 | 5 | 266.4 | 0.68 | 1002.2/1003.2 |
| Host workspace | staged | 128 | 640/640 | 5 | 1597.2 | 13.96 | 1204.6/1248.6 |
| Tool rootfs | safe | 128 | 638/640 | 3 | 3245.0 | 39.97 | 1446.3/1490.5 |
| Tool rootfs | vm | 8 | 40/40 | 5 | 786.5 | 274.22 | 1281.2/1323.2 |
| Tool rootfs | vm | 32 | guard | 0 | — | — | — |
| Tool rootfs | vm | 128 | guard | 0 | — | — | — |
| Tool rootfs | podman | 8 | 40/40 | 5 | 391.9 | 36.69 | 1114.2/1181.3 |
| Tool rootfs | podman | 32 | 160/160 | 5 | 1570.6 | 45.42 | 1407.4/1679.2 |
| Tool rootfs | podman | 128 | 640/640 | 5 | 6011.3 | 51.42 | 6469.9/8542.5 |
| Tool rootfs | pVisor OCI | 8 | 40/40 | 5 | 210.1 | 493.64 | 1521.9/2512.8 |
| Tool rootfs | pVisor OCI | 32 | 45/160 | 0 | — | — | — |
| Tool rootfs | pVisor OCI | 128 | 55/640 | 0 | — | — | — |
| Minimal shell | vm | 8 | 40/40 | 5 | 789.8 | 283.13 | 1292.1/1306.1 |
| Minimal shell | vm | 32 | 160/160 | 5 | 3093.4 | 370.44 | 1939.0/2115.2 |
| Minimal shell | vm | 128 | guard | 0 | — | — | — |
| Minimal shell | podman | 8 | 40/40 | 5 | 389.2 | 38.50 | 1132.4/1189.4 |
| Minimal shell | podman | 32 | 160/160 | 5 | 1559.6 | 45.99 | 1497.0/1969.4 |
| Minimal shell | podman | 128 | 640/640 | 5 | 5929.0 | 50.74 | 6473.4/8242.6 |
| Minimal shell | pVisor OCI | 8 | 40/40 | 5 | 208.1 | 20.36 | 1048.4/1048.9 |
| Minimal shell | pVisor OCI | 32 | 160/160 | 5 | 834.2 | 22.05 | 1060.1/1066.9 |
| Minimal shell | pVisor OCI | 128 | 640/640 | 5 | 3338.9 | 28.43 | 1300.2/1478.3 |

### 分析

safe 128 路为 **638/640** 成功，失败是 `Address already in use`：临时端口探测与实际监听之间有竞争。不能据成功样本宣称 128 路稳定。VM 单路进程树 RSS 约 100 MiB，8 路约 789 MiB；这是启动后空闲常驻值，不是每台 VM 配置内存或活跃工作集上限。

工具 rootfs 约 749 MiB，pVisor OCI 每 Job 复制私有环境到默认 `/tmp`。32/128 路触发 tmpfs 用户配额（`Disk quota exceeded`），失败数公开保留。最小 shell rootfs 配置对照另列，区别运行时与工具环境成本；Podman 使用同一预制镜像，不做相同的每 Job 完整 rootfs 复制。
### 空闲容量和完整 Agent 容量是两回事 {#baseline-meaning}

这里的原生 shell 和 Podman/crun 给出熟悉的占用基线，128 路成功回答的是“能否同时维持这些空闲进程”。它不能回答“能否同时运行 128 个带 Python、Node、Rust 和 Agent CLI 的任务”。2 vCPU/128 MiB 的空闲 VM 也不能代表完整工具环境的活跃工作集。

完整工具环境的单任务延迟与实际进程树 RSS 见 [Agent 环境对比](agent-tasks.md#reference-env)。新 Docker 资源数据包含私有 daemon 的固定成本，不能与这里可能漏掉后台进程的 Podman RSS 直接排名。并发规划应使用自己的任务工作集，再测成功率和完成时间；当前未发布完整 Agent 的 128 路容量承诺。

### 适用边界 {#acceptance}

共享桌面有编辑器和后台进程，所测配置不是专用性能机或最大容量搜索。主批次 VM 内存 guard 比较保守；配置对照按单路实测 RSS×1.5、至少 128 MiB/Job，另留 2 GiB 再决定是否运行。RSS 求和会重复计算共享页；不是 PSS 或系统总内存。Podman 的后台进程可能不全在被跟踪的父子树中，CPU/RSS 不能据此做严格总资源排名。

