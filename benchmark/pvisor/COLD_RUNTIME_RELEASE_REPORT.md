# Linux 冷压缩：节省多少内存，恢复要付出什么成本？

## 主要结论

**重复数据的第一冷窗口中，启用冷压缩的 worker 组计费为 131.047 MiB，关闭时为 293.992 MiB；但启用组的启动占用、生命周期峰值和首次恢复耗时更高。** 四条件完整性预检通过。这是每条件一次的工程观测，尚不足以决定默认启用，也不能证明比 Docker、Firecracker 或 QEMU 更高的任务密度。

本报告服务 **B-COLD-RUNTIME-ENG，engineering A/B**，不填充用户 benchmark 的性能排名。只使用同一份冻结源码的独立 GNU release cohort。

## Motivation

空闲 Agent 的内存成本需要看实际释放了多少驻留页，以及再次访问时要付出多少代价。完整 worker 组计费、启动和恢复余量、CPU 成本必须一起展示，不能用较小的编码对象替代净物理内存收益。

## 实验设计

- Linux x86_64/KVM，宿主 `7.2.8-200.fc44.x86_64`；GNU 动态链接 release，仓库 opt-level `z`、thin LTO、gateway feature。记录中继承的“debug SDK timing”文字不适用于此制品；摘要、设备 I/O 和诊断暂停仍计入对应边界。
- 同一 rootfs、固件及 payload 生成器，按固定种子打乱四格：cold off/on × repeated/random-unique。每格新建一个 256 MiB、1 vCPU VM，64 MiB 数据，n=1；实际最大并发为一个 VM。
- measured worker/VM/helper/store cgroup 为四核 quota、2 GiB、零 swap；协调器在单独的 512 MiB/一核 quota 服务，排除在产品占用中。CPU 可在 0–15 上运行，没有专属绑核；未改变全局 KSM/sysctl，userfaultfd 使用已有设备授权。
- ready → 固定 20 秒窗口 → 第一次完整恢复和设备 I/O → 100% 随机 mutation → 固定 35 秒窗口 → 第二次完整恢复 → 退出。**第二冷窗口两种初始内容都已经成为随机数据**；repeated 在此只标识起始内容。
- on 使用 prefault 的匿名 RAM 和实例内 userfaultfd pager/store，off 使用默认 shared-file RAM。开关同时改变 backing、初始驻留及回收策略，不是只隔离编码器的 A/B。
- 每格启动前连续 30 秒未检测到其他同用户 VM/build；运行中持续检查。本批没有检测到竞争任务，但最多有六项权限受限观测，轮询也可能遗漏短任务，不能证明全宿主独占。
- 完整 worker 组 memory.current/peak、CPU、smaps 和累计 pager 计数留存。独立重新生成期望摘要，核对 ready/restore1/mutation/restore2/exit 共 20 条全 64 MiB 摘要及设备 I/O 记录；24 个阶段、输入不变、heartbeat、真实 discard/refault、原生 reap 和 owned unit 清理均通过。

失败不算零耗时，不补格、不合并旧批。n=1 不给 P50/P95/P99、置信区间或因果百分比；不回答多 VM 密度、长期热点、业务吞吐或跨产品排名。

## 实验数据和分析

内存为 MiB（2²⁰ 字节），CPU 为退出后完整 worker 组累计秒数；恢复为 guest 的完整摘要加 64 MiB 设备写入/fsync/读回墙钟 ms，不是纯缺页或 host RPC 时间。每格一次观测；peak 是生命周期高水位。

| 初始内容 | Cold | ready MiB | cold1 MiB | cold2 MiB | peak MiB | CPU s | 恢复1 ms | 恢复2 ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| repeated | off | 293.027 | 293.992 | 330.746 | 333.223 | 13.128 | 163.603 | 455.319 |
| repeated | on | 371.094 | 131.047 | 266.457 | 377.141 | 13.555 | 505.722 | 203.304 |
| random-unique | off | 296.727 | 296.730 | 304.520 | 307.969 | 13.082 | 205.389 | 462.634 |
| random-unique | on | 367.223 | 259.871 | 264.332 | 373.809 | 13.922 | 214.592 | 203.903 |

稳定的重复内容在第一窗口中有明显的组计费优势，同时首次访问付出了更高成本。随机内容的启用组在本次两个冷窗口也更低，但启动和峰值仍更高；单次观测不能推导普适收益。第二次恢复更短也不构成延迟保证。关闭组的文件缓存回收同样会改变组计费，不应将它解释为压缩。

规划资源时要按启动、生命周期峰值和恢复余量留空间，不能仅用 cold1 占用推算可增加的 VM 数。下一步需要多轮重复、真实热/冷工作集及完整完成任务的密度对照。

下载加工数据：[四条件汇总 CSV](cold-runtime-release.csv)、[24 阶段内存与 pager 计数 CSV](cold-runtime-release-phases.csv)。off 的 pager 字段为空，表示机制未启用；累计计数不跨阶段相加，encoded store 不含索引、allocator、scratch 和原 RAM。

来源摘要：

- example SHA-256：`277575d4e446608a889e0fc757df1620c7a68de5855b70f9968de2a8584f1fc1`。
- 产品 source manifest SHA-256：`6819bddee16d1d90c24ac8300775553a84cec85447f8199a1c6f9d0dd82baf39`；记录 HEAD `1d413e307bd7932d7d20a7d42fd39854849e26e3` 及其未提交改动，不能只由 HEAD 推导制品内容。
- firmware SHA-256：`b61f68dac3ef20a88e1ee387733e4baed2f7c02edac940c8882e0b549dac95e4`。
- cohort report SHA-256：`f42aa046e0d1f525fe662bb0038d459a4a39e2742cab9baa05b73e12cb82e638`；原始报告、逐次记录、guard、smaps、输入清单及独立审计保存在本地 `benchmark/.data/6cr/`，构建收据在 `benchmark/.data/6crb/`。
- 所有 CSV 行保留样本数、制品、输入、报告及每格原始报告摘要。复测方法见 [README 的冷压缩入口](README.md#cold-runtimestorage-validation)，使用 NEW 输出目录并先构建、再等待 quiet window。
