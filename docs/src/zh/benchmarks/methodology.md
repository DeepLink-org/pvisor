# 基准方法与环境

所有基准遵守同一套协议：

- 写明硬件、操作系统、内核或 macOS 版本、FUSE 实现、pVisor 版本与提交号；
- 给出 p50、p95、p99 与样本数；
- 提供可一条命令复现的脚本（放在仓库 `benchmark/` 下），报告使用 `pvisor-benchmark/v1` schema；
- 每个对照组写明配置，不与「未调优的对手」比较；
- 结果按日期保留，不覆盖旧数据。

## 环境清单

每份报告开头附下面这张表：

| 项目 | 示例 |
| --- | --- |
| 日期 | 2026-10-02 |
| pVisor 版本与提交 | `0.x.y` / `abc1234` |
| 硬件 | Apple M4，16 GiB；或 CPU 型号、核数、内存 |
| 操作系统与内核 | macOS 26.x；或 Ubuntu 24.04，Linux 6.8 |
| 文件系统与 FUSE 实现 | APFS + macFUSE 5.x；或 ext4 + libfuse 3.x |
| 执行器与参数 | `--executor vm --overlaynet auto` |
| 样本数与预热 | 100 次，丢弃前 5 次 |

## 对照组

- 对照组使用该方案文档推荐的配置，并写明版本与参数；
- 拆分出 pVisor 自身的开销：例如同一执行器下 `--filesystem host` 与暂存的差，而不是只给端到端总数；
- 只有部分阶段的数据（例如只测 guest init）时，标题必须写明测的是哪个阶段，不能写成端到端结论。

## 从现有测量入口开始

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly
just benchmark-compare target/pvisor-benchmark/candidate/raw-report.json target/pvisor-benchmark/main/raw-report.json
# Linux：启动与资源占用矩阵
just benchmark-startup --warmups 10 --samples 100
```

`just benchmark` 测最小 host Run 与读取 Run Bundle 的进程级成本：smoke 为 2 次预热/10 个样本，nightly 为 10 次预热/50 个样本。两者使用 `pvisor-benchmark/v1`。`benchmark-startup` 使用独立的 `startup.json`/`startup.md`，不会伪装成同一个报告 schema；默认入口为 3 次预热/30 个样本，上例显式覆盖。

构建、镜像下载和 rootfs 准备不计入现有 startup 样本；报告必须标明这一边界。无法通过前置探测的 executor 单独列为 SKIP，并保留 stderr；成功测量要求命令成功且 Bundle 为 completed/零退出。不能把失败、跳过或控制降级算成更快的成功样本。

## 解释与归档

同机、同套件、同输入比较 candidate 与 baseline。`benchmark-compare` 默认以 15% 为回归阈值，除非显式启用 `--fail-on-regression`，结果只作报告。保存原始样本、摘要、完整参数、输入摘要与提交号；区分冷镜像、热磁盘缓存与热页缓存，记录后台负载和电源状态。

文件系统、网络、监督成本与并发密度仍缺完整实测。已有工具、规格通过或某个阶段的数字，都不能替代这些结果。
