# 哪些执行模式能阻止工作区外的宿主访问？

## 主要结论 {#conclusions}

**性能选择应先确定隔离边界。所测 host_process + `--stage` 保留工作区改动，却允许访问视图外宿主路径；所测 safe 和准备好的 VM rootfs 阻止了视图外读写。OCI 的可写工作区挂载会直接改变宿主文件，因此其速度不能作为相同暂存语义的对照。**

| 需求 | 选型含义 |
|---|---|
| 只需暂存工作区改动 | stage 不等于限制宿主访问 |
| 需要阻止视图外读写 | 使用已验证的隔离配置 |
| OCI 可写工作区挂载 | 写入宿主属于挂载授权 |

## Motivation {#motivation}

性能数字必须对应实际边界，否则更快可能只是隔离没生效。负对照和宿主最终内容检查帮助解释“请求成功”和“宿主被修改”的区别。

## 实验设计 {#interpretation}

每个配置在独立目录试探绝对路径、符号链接、/proc/self/root、路径遍历、Unix socket 和 lower 目录别名写入。host/staged 是有意开放宿主的负对照。除了进程返回值，读取宿主原文件和 Bundle 的 observed isolation/staging 字段。该隔离矩阵使用准备好的工具 rootfs；不同 rootfs 与授权路径需各自验证。

数据来自 2026-10-04 的固定 Linux/x86_64 制品与声明配置；不将该矩阵视为所有新版默认行为，当前默认值见[执行器边界](../security/executor-boundaries.md)。macOS 的对应负载未测。

固定制品与测量日期按表注明。失败与校验不通过的样本不计入成功耗时，失败数量单列；既有数据没有事先的宿主干扰剔除规则，所有通过校验的慢样本保留。30 次及更少采样的 P95 仅为观察参考，不给 P99 或稳定尾延迟承诺。

## 实验数据和分析 {#results}

| Profile | Host outside readable | Host outside written | Host lower alias written | Workspace staged |
|---|---|---|---|---|
| host | True | True | True | False |
| staged / host_process | True | True | True | True |
| safe | False | False | False | True |
| vm | False | False | False | True |
| container | False | False | True | False |

### 分析

safe/VM 的 lower 别名写入 API 可以返回成功，但内容落在 stage，宿主 lower 没改变；只看 syscall 成败会误判。OCI 则按声明的 writable mount 写到宿主，这是授权范围，不是假定全部 mount 都暂存。普通 host/staged 可以读写 fixture 的 outside 文件并连接其 Unix socket，不能视为隔离失败后仍计为 safe。

网络 direct socket 拒绝另在[网络报告](network.md)验证；暂存合入、冲突和中断恢复在[apply](apply.md)验证。此页是功能矩阵，不把拒绝速度当性能优势。

### 适用边界 {#acceptance}

只覆盖所列文件访问与退出场景，不是内核漏洞或逃逸审计。挂载、网络与 rootfs 配置决定实际边界；单独 stage 不是完整沙箱。

### 数据下载与复现 {#run}

[整理后的表格 CSV](isolation-tests.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
