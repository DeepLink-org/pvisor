# 隔离有效性：观察真正的宿主影响

`--stage` 保留工作区改动，但单独使用仍能访问视图外的宿主路径。safe、准备好的 VM rootfs 和 OCI 配置阻止了本轮视图外读取、写入和 Unix socket 探针；OCI 声明为可写的工作区挂载会直接改变宿主。

## Motivation

性能数字必须对应实际边界，否则更快可能只是隔离没生效。负对照和宿主最终内容检查帮助解释“请求成功”和“宿主被修改”的区别。

## 实验设计 {#interpretation}

每个配置在独立目录试探绝对路径、符号链接、/proc/self/root、路径遍历、Unix socket 和 lower 目录别名写入。host/staged 是有意开放宿主的负对照。除了进程返回值，读取宿主原文件和 Bundle 的 observed isolation/staging 字段。VM 本页用准备好的工具 rootfs，区别于文件系统表的 host rootfs `/`。

## macOS

本轮在 Linux 实测，以下工作负载没有 macOS 样本；macFUSE/FSKit 开销与并发容量均未测。既有 macOS/HVF 数据继续保留在 [VM 启动时间](startup.md)与[VM 内存报告](vm-memory/index.md)，不移作本页结果。

## Linux：2026-10-04 {#results}

| Profile | Host outside readable | Host outside written | Host lower alias written | Workspace staged |
|---|---|---|---|---|
| host | True | True | True | False |
| staged | True | True | True | True |
| safe | False | False | False | True |
| vm | False | False | False | True |
| container | False | False | True | False |

### 分析

safe/VM 的 lower 别名写入 API 可以返回成功，但内容落在 stage，宿主 lower 没改变；只看 syscall 成败会误判。OCI 则按声明的 writable mount 写到宿主，这是授权范围，不是假定全部 mount 都暂存。普通 host/staged 可以读写 fixture 的 outside 文件并连接其 Unix socket，不能视为隔离失败后仍计为 safe。

网络 direct socket 拒绝另在[网络报告](network.md)验证；暂存合入、冲突和中断恢复在[apply](apply.md)验证。此页是功能矩阵，不把拒绝速度当性能优势。
## 边界与下一轮 {#acceptance}

这些探针不是渗透测试、内核漏洞覆盖或完整安全证明。共享路径、rootfs 内容、网络策略与执行器配置会改变边界。本轮不覆盖凭据窃取、全部 mount/rename 组合或恶意内核利用。版本、制品和每项观察随归档保留；人工 semspec 审批状态与测试通过分别处理。

## 复现与证据 {#run}

从仓库根目录运行；输出目录必须是新目录。动态 firmware 入口需要 GNU/Linux 版 CLI；静态 musl 制品使用另一套 firmware 入口，不能混用。Linux、KVM/FUSE/user namespaces、Python 3.14、Rust/GCC、Git/rg、Node 24/npm、Podman/crun 是本机工具环境；Agent 套件另需 Claude/Codex CLI。

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites isolation --samples 1 --warmups 0
```

先用 `--samples 1 --warmups 0` 检查环境。脚本、输入定义和正确性断言在 `benchmark/pvisor/v1/`；报告会复制二进制、firmware 和当轮脚本并记录摘要。失败不会进入性能分布；准备、策略拒绝和工作负载错误分别保留。每个页面写明有效样本数；小样本 P95/P99 只描述本批次，不估计长期尾延迟。

[环境、制品与采样方法](methodology.md#product-v1) · [批次清单](../../assets/benchmarks/product-v1-20261004/manifest.json) · [逐样本汇总 CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [原始报告与日志归档](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)。报告保留源码的未提交状态；当轮可执行制品 SHA256 是身份依据。归档不含数 GB 的 rootfs、二进制和可重建工作区；输入摘要及每轮脚本保留。
