# 启动延迟

覆盖冷启动、热启动延迟与常驻内存；对照组为裸进程、`docker run`、Firecracker，以及各 Agent 自带沙箱；工作负载为 `pvisor run -- true`，覆盖 host、host+stage、`--safe`、container、VM 五种配置。

## 已迁移数据：VM 执行器的 guest init 就绪延迟（Apple Silicon）

这组数据只测量 VM 执行器内 guest init 到就绪的阶段，比较 C 与 Rust 两种 guest init 实现；它不是 pVisor 的端到端冷启动延迟，也不涉及 host 和 container 执行器。来自 `benchmark/pvisor/README.md`。测量于 2026-10-01，Apple M4/HVF，libkrunfw 5.5.0，Alpine minirootfs 3.22.1 aarch64，10 warmup / 100 sample：

| 场景 | C ready p50 (ms) | Rust ready p50 (ms) | Rust ready p95 (ms) | p50 变化 |
| --- | ---: | ---: | ---: | ---: |
| 直接命令，网络关闭 | 105.27 | 114.57 | 116.63 | 慢 8.83% |
| 工作区，网络关闭 | 119.18 | 114.48 | 116.24 | 快 3.94% |
| 工作区，网络开启 | 119.46 | 114.81 | 116.33 | 快 3.89% |

方法、脚本与原始报告仍保留在 `benchmark/pvisor/`。

!!! note "建设中"
    host／host+stage／--safe／container／VM 五组配置的冷启动、热启动与常驻内存，以及 p99 与样本数，尚无数据；报告要求见[方法](methodology.md)。
    结论只描述该 HVF runner，不外推到 Linux/KVM。

