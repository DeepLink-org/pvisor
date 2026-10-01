# 基准与对比

本栏目是可复现的测量与对比，也是「可核对」承诺的证据来源。

## 首屏数字

| 指标 | 值 | 来源 |
| --- | --- | --- |
| 冷启动延迟（guest init，Apple M4/HVF） | p50 ≈ 105–119 ms | [启动延迟](startup.md) |
| 文件系统开销 | 建设中 | [文件系统开销（规划中）](filesystem.md) |
| 端到端 Agent 任务开销 | 建设中 | [端到端任务（规划中）](agent-tasks.md) |

所有基准写明方法、环境与样本数，见[方法](methodology.md)。

## 基准

[启动延迟](startup.md) · [文件系统开销（规划中）](filesystem.md) · [网络开销（规划中）](network.md) · [apply/drop 成本（规划中）](apply.md) · [端到端任务（规划中）](agent-tasks.md) · [监督成本（规划中）](supervision-cost.md) · [并发密度（规划中）](density.md) · [隔离有效性（规划中）](isolation-tests.md) · [回放保真度](replay-fidelity.md)

## 对比

[Agent 自带沙箱（规划中）](compare-agent-sandboxes.md) · [Docker/devcontainer（规划中）](compare-containers.md) · [云端沙箱（规划中）](compare-cloud-sandboxes.md) · [隔离基座（规划中）](compare-runtimes.md) · [RL 基础设施（规划中）](compare-rl-infra.md)

!!! note "TODO"
    补首屏的「文件系统开销」「端到端任务」数字，把「建设中」替换为数据或链接。

