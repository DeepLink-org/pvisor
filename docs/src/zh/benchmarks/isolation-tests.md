# 隔离有效性：观察真正的宿主影响

## 主要结论 {#conclusions}

性能选择应先确定隔离边界。独立 `--stage` 保留工作区改动，却允许访问视图外宿主路径；所测 safe 和准备好的 VM rootfs 阻止了视图外读写。OCI 的可写工作区挂载会直接改变宿主文件，因此其速度不能作为相同暂存语义的对照。

## Motivation {#motivation}

性能数字必须对应实际边界，否则更快可能只是隔离没生效。负对照和宿主最终内容检查帮助解释“请求成功”和“宿主被修改”的区别。

## 实验设计 {#interpretation}

每个配置在独立目录试探绝对路径、符号链接、/proc/self/root、路径遍历、Unix socket 和 lower 目录别名写入。host/staged 是有意开放宿主的负对照。除了进程返回值，读取宿主原文件和 Bundle 的 observed isolation/staging 字段。VM 本页用准备好的工具 rootfs，区别于文件系统表的 host rootfs `/`。

这些结果来自 Linux/x86_64；macOS 的对应负载未测。每项数字的制品、缓存条件与样本保存在关联报告中。

## 实验数据和分析 {#results}

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

### 适用边界 {#acceptance}

只覆盖所列文件访问与退出场景，不是内核漏洞或逃逸审计。挂载、网络与 rootfs 配置决定实际边界；单独 stage 不是完整沙箱。

### 数据来源与复现 {#run}

[配置与采样方法](methodology.md#product-v1) · [Manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv) · [Samples CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw evidence](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz)

复现命令与环境要求见[方法技术记录](../design/benchmark-methodology-evidence.md)。
