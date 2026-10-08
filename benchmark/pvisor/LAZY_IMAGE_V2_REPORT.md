# Lazy image V2：NumPy 小文件导入的工程 A/B

## 主要结论

**同一 release 制品关闭/开启 V2，NumPy 冷客户端 Ready P50 从 1,030.8 ms 降到 862.1 ms，减少 16.4%；冷客户端 Completion 减少 14.6%。热客户端未检出耗时差异。** 元数据请求与建连显著减少，内容读取量保持不变。该实验属于 `B-LAZY-ENG`，不替代历史 Docker 对照，不支持 Docker、WAN 或并发吞吐排名。

## Motivation

Python 导入涉及多个目录内的模块查找与分散读取。逐文件查询元数据、逐请求重建连接的固定成本，可能抵消按需读取减少下载量的收益。需要在同一镜像、同一制品下确认优化，而不是跨批次比较。

## 实验设计

- 同一冻结静态 musl `pvisor` / `pvisor-cache`，内嵌 firmware。`PVISOR_LAZY_IMAGE_V2=0` 为 V1-compatible 对照，`=1` 为 V2；两者均包含共同的分页/正确性修复，不等价于重新测量历史 V1 二进制。
- 固定 amd64 镜像 `docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`。Python 3.13.14 / NumPy 2.5.2，不导入 pandas；禁用 bytecode 写入，固定线程数及 hash seed，校验数组平方和 1240、矩阵乘法结果和 3680，唯一正确输出后退出 0。
- Linux x86_64/KVM，内核 `7.2.8-200.fc44.x86_64`；launcher CPU 0/1、2 vCPU、2 GiB guest RAM；cache 与计数代理 CPU 2/3。本机 loopback TCP，无延迟/带宽注入，服务端预准备、宿主页缓存热。没有 Docker 对照，也没有等价的整机内存硬上限。
- 初始轮加 3 轮 warmup 排除；30 轮正式随机配对，每模式冷后紧接热，共 120 个有效样本。每对 fresh XDG cache；每次 fresh VM/workspace/stage。校验输出、成功退出、Run Bundle、冷内容非零及热内容为零；失败使批次无效，不按耗时剔除。
- 私有 user/mount/PID namespace、owned tmpfs root 与 `pivot_root` 隔离 Host listener，并允许 libkrun 创建其内部 namespace。支持文件原样复制、保留 symlink，不跟随 guest 绝对链接遍历宿主。执行文件副本与冻结制品 hash 一致；无宿主 listener 停止或安全检查绕过。监督超时 900 s，终止后验证无新 namespace 用户。预先不可读的无关同 UID 进程仅凭稳定 PID/starttime 排除，PID 复用及新不可读进程仍使清理审计失败。
- 分布复用 `publication.py` 规则；均为 unsplit，报告 P50，n=30 的 P95 仅参考。完整轮配对 bootstrap 5,000 次；差值为 V2 减 V1 的边际中位数。

## 实验数据和分析

### 启动耗时

测量日期 2026-10-08，单位 ms，每格 n=30；零正式失败、零按速度剔除。

| 指标 | V1-compatible P50 | V2 P50 | V1 P95 参考 | V2 P95 参考 | V2−V1 的 95% CI |
|---|---:|---:|---:|---:|---:|
| 冷 Ready | 1,030.8 | 862.1 | 1,321.6 | 1,007.9 | −179.8 至 −158.1 |
| 冷 Completion | 1,178.9 | 1,006.6 | 1,460.5 | 1,215.9 | −181.6 至 −153.4 |
| 热 Ready | 448.6 | 447.0 | 476.7 | 463.3 | −4.1 至 0.7 |
| 热 Completion | 583.1 | 583.1 | 644.1 | 661.4 | −9.1 至 1.2 |

冷 Ready 的边际中位数减少 168.7 ms，Completion 减少 172.3 ms。热缓存两个区间均跨零，未检出收益或退化。不能据此将冷启动剩余耗时解释为某个单独阶段的贡献。

### 请求与内容

每格 n=30。除冷 V2 的一次额外连接握手外，条件内计数相同；所有请求计入，包括握手。

| 冷客户端指标 | V1-compatible | V2 |
|---|---:|---:|
| `stat` | 260 | 75 |
| `list` | 14 | 38 |
| `read` | 177 | 177 |
| `ping` | 1 | 5（29/30）；7（1/30） |
| `prepare` | 1 | 1 |
| 请求总数 | 453 | 296（29/30）；298（1/30） |
| 连接数 | 453 | 2（29/30）；3（1/30） |
| 文件内容 bytes | 36,951,980 | 36,951,980 |
| 响应帧及内容 bytes | 37,153,682 | 37,239,643（29/30）；37,239,687（1/30） |

`stat` 减少 71.2%，请求 P50 减少 34.7%，连接 P50 减少 99.6%。目录批量预取增加约 86 KB 元数据响应，未增加内容下载；内容仍按原文件边界与 1 MiB 上限读取，不预下载整镜像或目录内容。热内容两模式均为 0；V1 为 2 请求/2 连接/750 响应 bytes，V2 为 4 请求/1 连接/794 响应 bytes，增加的 2 个 Ping 用于兼容性握手，未检出热时间退化。响应计数不含 TCP/IP 完整网络开销。

该 A/B 同时比较元数据预取和连接复用，不能分别归因两者的收益。剩余 177 次内容读取未做小文件内容打包；macOS/HVF、live S3、WAN、并发吞吐和大规模容量未覆盖。

### 来源与复现

[加工统计 CSV](lazy-image-v2-summary.csv) · [复现手册](README.md#lazy-image-v2-engineering-ab)

- 原始证据：`benchmark/pvisor/.data/lazy-image-v2-formal-20261008/`；136 次启动包含 16 个排除观测，保留 stdout/stderr、请求、Run Bundles、完整冻结源码/制品和隔离回执。
- 不可变 report SHA-256：`df01c5322c9394f66bd0f76c0a721b35e4a1c2e80d036fe7dce6afcb0d5b1ee3`。
- pVisor SHA-256：`612662b10883cd0e50e63d18a9946bf02117d9143bddd0f55325d73be759e783`。
- cache SHA-256：`caa8c7d435bd91d6e9d1110e72468af0b3d438bdd1a3ec82747e8738c0d1b3ef`。
- harness SHA-256：`277cb4b71c265cc3d55136555f6e06fc11e3baa3025aa463cc9a6c743041cf5c`。
- 独立构建记录：`benchmark/pvisor/.data/lazy-image-v2-build-20261008/`，包含命令、完整日志、构建前源码摘要及 dirty patch，构建期间源码摘要未变，制品摘要与测量一致。其后仅修改常规测试与 benchmark；测量冻结 checkout 不应代替构建输入记录，预存/共享构建缓存的所有依赖来源未另行重建验证。
- 失败预检独立保留为 `lazy-image-v2-smoke-20261008-{1,2,3,4}/`，成功预检为 `...-5/`，不进入正式分布，也不修改已有 Ubuntu/NumPy 原始报告。
