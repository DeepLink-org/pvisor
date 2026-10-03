# 并发密度与资源成本

原生、host、staged 和 Podman 在 128 路空闲 Job 探针中均完成全部任务；safe 的 128 路出现少量端口竞争。主批次 VM 测到 8 路，32/128 路因宿主内存预算未跑；补测按实测 RSS 预算，完成 32 路 VM 和 128 路最小 shell OCI，128 路 VM 仍未测。这些数据是已观察到的容量下界。

## Motivation

多 Agent 时，内存、启动和环境复制的成本会累积。并发成功率必须与资源一起报告，不能只给最快成功样本。

## 实验设计 {#interpretation}

每个 Job 输出 ready 后保持 1 秒；并发 1/8/32/128，每格 5 个批次，无预热。每 20 ms 采样拥有的进程树 RSS 峰值，记录子进程 CPU 与每 Job 总时间。VM 使用 2 vCPU/128 MiB；其他配置与文件系统表一致。它是占用探针，不是真实 Agent 推理或构建负载。只对全成功批次计算性能，成功/尝试数包含失败批次。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| Rootfs batch | Backend | Concurrency | Completed/attempted | Full batches | RSS P50 MiB | CPU P50 ms/job | Job P50/P95/P99 ms |
|---|---|---|---|---|---|---|---|
| main | native | 1 | 5/5 | 5 | 2.0 | 1.01 | 1002.3/1002.5/1002.6 |
| main | native | 8 | 40/40 | 5 | 16.5 | 0.78 | 1001.5/1002.6/1002.7 |
| main | native | 32 | 160/160 | 5 | 66.4 | 0.71 | 1002.1/1002.9/1003.3 |
| main | native | 128 | 640/640 | 5 | 266.4 | 0.68 | 1002.2/1003.2/1010.6 |
| main | host | 1 | 5/5 | 5 | 12.1 | 7.98 | 1018.9/1021.6/1022.1 |
| main | host | 8 | 40/40 | 5 | 95.3 | 6.75 | 1019.3/1019.9/1019.9 |
| main | host | 32 | 160/160 | 5 | 383.4 | 7.21 | 1019.9/1021.1/1021.5 |
| main | host | 128 | 640/640 | 5 | 1533.1 | 7.72 | 1032.1/1055.7/1059.0 |
| main | staged | 1 | 5/5 | 5 | 12.4 | 12.80 | 1039.8/1127.7/1137.4 |
| main | staged | 8 | 40/40 | 5 | 99.5 | 11.96 | 1075.5/1101.6/1110.4 |
| main | staged | 32 | 160/160 | 5 | 398.8 | 12.83 | 1132.2/1211.5/1214.7 |
| main | staged | 128 | 640/640 | 5 | 1597.2 | 13.96 | 1204.6/1248.6/1315.3 |
| tools | safe | 1 | 5/5 | 5 | 26.0 | 61.04 | 1194.1/1238.4/1238.7 |
| tools | safe | 8 | 40/40 | 5 | 205.2 | 53.25 | 1300.5/1411.6/1416.2 |
| tools | safe | 32 | 160/160 | 5 | 821.6 | 37.05 | 1135.1/1156.4/1158.8 |
| tools | safe | 128 | 638/640 | 3 | 3245.0 | 39.97 | 1446.3/1490.5/1504.6 |
| tools | vm | 1 | 5/5 | 5 | 99.5 | 213.12 | 1219.2/1235.1/1238.2 |
| tools | vm | 8 | 40/40 | 5 | 786.5 | 274.22 | 1281.2/1323.2/1326.8 |
| tools | vm | 32 | guard | 0 | — | — | — |
| tools | vm | 128 | guard | 0 | — | — | — |
| tools | podman | 1 | 5/5 | 5 | 49.1 | 31.66 | 1054.4/1056.4/1056.8 |
| tools | podman | 8 | 40/40 | 5 | 391.9 | 36.69 | 1114.2/1181.3/1190.5 |
| tools | podman | 32 | 160/160 | 5 | 1570.6 | 45.42 | 1407.4/1679.2/1740.7 |
| tools | podman | 128 | 640/640 | 5 | 6011.3 | 51.42 | 6469.9/8542.5/9251.6 |
| tools | container | 1 | 5/5 | 5 | 26.2 | 245.13 | 1269.6/1393.1/1415.9 |
| tools | container | 8 | 40/40 | 5 | 210.1 | 493.64 | 1521.9/2512.8/2516.0 |
| tools | container | 32 | 45/160 | 0 | — | — | — |
| tools | container | 128 | 55/640 | 0 | — | — | — |
| shell | vm | 1 | 5/5 | 5 | 98.5 | 217.11 | 1239.3/1272.2/1278.7 |
| shell | vm | 8 | 40/40 | 5 | 789.8 | 283.13 | 1292.1/1306.1/1318.2 |
| shell | vm | 32 | 160/160 | 5 | 3093.4 | 370.44 | 1939.0/2115.2/2132.7 |
| shell | vm | 128 | guard | 0 | — | — | — |
| shell | podman | 1 | 5/5 | 5 | 48.8 | 36.72 | 1060.1/1085.2/1090.0 |
| shell | podman | 8 | 40/40 | 5 | 389.2 | 38.50 | 1132.4/1189.4/1237.3 |
| shell | podman | 32 | 160/160 | 5 | 1559.6 | 45.99 | 1497.0/1969.4/2071.9 |
| shell | podman | 128 | 640/640 | 5 | 5929.0 | 50.74 | 6473.4/8242.6/8752.4 |
| shell | container | 1 | 5/5 | 5 | 26.1 | 18.34 | 1037.9/1054.4/1057.6 |
| shell | container | 8 | 40/40 | 5 | 208.1 | 20.36 | 1048.4/1048.9/1049.1 |
| shell | container | 32 | 160/160 | 5 | 834.2 | 22.05 | 1060.1/1066.9/1071.4 |
| shell | container | 128 | 640/640 | 5 | 3338.9 | 28.43 | 1300.2/1478.3/1528.9 |

### 分析

主批次 safe 128 路为 **638/640** 成功，失败是 `Address already in use`：临时端口探测与实际监听之间有竞争。不能据成功样本宣称 128 路稳定。VM 单路进程树 RSS 约 100 MiB，8 路约 789 MiB；这是启动后空闲常驻值，不是每台 VM 配置内存或活跃工作集上限。

工具 rootfs 约 749 MiB，pVisor OCI 每 Job 复制私有环境到默认 `/tmp`。32/128 路触发 tmpfs 用户配额（`Disk quota exceeded`），失败数公开保留。最小 shell rootfs 补测另列，区别运行时与工具环境成本；Podman 使用同一预制镜像，不做相同的每 Job 完整 rootfs 复制。
## 边界与下一轮 {#acceptance}

共享桌面有编辑器和后台进程，本轮不是专用性能机或最大容量搜索。主批次 VM 内存 guard 比较保守；补测按单路实测 RSS×1.5、至少 128 MiB/Job，另留 2 GiB 再决定是否运行。RSS 求和会重复计算共享页；不是 PSS 或系统总内存。Podman 的后台进程可能不全在被跟踪的父子树中，CPU/RSS 不能据此做严格总资源排名。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites density --samples 30 --warmups 3
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
