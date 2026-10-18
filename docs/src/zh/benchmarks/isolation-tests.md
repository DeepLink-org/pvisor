# 各执行器能挡住工作区外的写入吗？

## Motivation {#motivation}

选择执行模式时，既要看允许访问什么，也要看写入落在哪里。命令返回成功无法区分暂存写入和宿主修改；隔离边界也决定了哪些性能对照具有相同语义。

所测 Linux 配置中，pVisor staged、safe 和 VM 在各自 3/3 次检查中允许视图内操作、阻止所列视图外宿主访问，并把工作区写入保留在 stage。pVisor OCI 和 Podman 也阻止了视图外访问，但授权的可写工作区挂载直接修改宿主文件。host 允许所列宿主访问。

<!-- measurement-status:start -->

这些数值与结论仍属于原始历史批次；新框架尚无匹配的复测数据，见[复测覆盖](index.md#measurement-coverage)。

<!-- measurement-status:end -->

## 实验设计 {#interpretation}

测量每个执行模式在路径已知的 fixture 中的边界——哪些访问放行、哪些拒绝、写入最终落在哪里——作为实际隔离边界。实际隔离边界是模式真正执行的那一层；边界越紧，宿主上可达或可改的路径越少。

七种配置各运行三次独立 fixture，每次用固定种子打乱配置顺序，共 21 次。native 与 host 提供宿主可访问的对照；Podman 使用私有 overlay 存储和可写工作区挂载，pVisor OCI 使用 crun 与显式可写挂载；VM 与 OCI 使用同一份预备 Python/Git rootfs。

每次必须成功读取视图内文件、写入并读回工作区文件、使用内部 Unix socketpair。视图外试探包括绝对路径、符号链接、`/proc/self/root`、父目录遍历、绝对路径和符号链接写入、宿主 Unix socket 连接，以及 lower 的绝对路径别名写入。检查实际输出、宿主最终字节、stage 内容和 Bundle 中的执行边界，不能只依据 syscall 返回值。

环境为 Linux/x86_64、AMD Ryzen 7 9700X、内核 7.2.8-200.fc44；各 payload 验证 CPU 亲和性，宿主为 CPU 0、1，VM 为两个 guest vCPU。VM 配置 1 GiB 内存；没有统一宿主内存上限。没有预热，不清空宿主缓存；这些是正确性重复，不产生延迟排名。预备输入在执行前后核验，独立发布审计再次核对全部保留证据。失败单列，不以旧批次补齐。

## 实验数据和分析 {#results}

测量日期：2026-10-06。每模式 N=3，21/21 条件通过，无失败。访问和状态列为“该现象发生次数 / 3”，不是性能统计；四类读取的每条路径分别校验。

| 配置 | 检查通过 | 视图内读/写/socket | 四类视图外读取 | 视图外宿主被写入 | 外部 Unix socket 可连接 | 宿主 lower 别名被写入 | 工作区写入被暂存 |
|---|---:|---|---|---:|---:|---:|---:|
| 原生进程 | 3/3 | 各 3/3 | 3/3（各路径） | 3/3 | 3/3 | 3/3 | 0/3 |
| pVisor host | 3/3 | 各 3/3 | 3/3（各路径） | 3/3 | 3/3 | 3/3 | 0/3 |
| pVisor staged | 3/3 | 各 3/3 | 0/3（各路径） | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor safe | 3/3 | 各 3/3 | 0/3（各路径） | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor VM | 3/3 | 各 3/3 | 0/3（各路径） | 0/3 | 0/3 | 0/3 | 3/3 |
| pVisor OCI（可写挂载） | 3/3 | 各 3/3 | 0/3（各路径） | 0/3 | 0/3 | 3/3 | 0/3 |
| Podman（可写挂载） | 3/3 | 各 3/3 | 0/3（各路径） | 0/3 | 0/3 | 0/3 | 0/3 |

所有模式的视图内读、写、socket 正对照都通过，避免把无法工作的环境误认为隔离有效。staged、safe、VM 的宿主 lower 保持不变，stage 中保留完整写入；safe 和 VM 的 lower 别名写入 API 返回成功，仍未修改宿主。pVisor OCI 在所测别名下直接写入 lower；Podman 未解析该宿主绝对别名，但其相对工作区写入同样直接落到宿主。

这是路径和 Unix socket fixture 的结果。TCP 策略另见[网络评测](network.md)，合入冲突和中断恢复另见[apply 评测](supervision-cost.md#apply-cost)。

### 适用边界 {#acceptance}

挂载授权、rootfs 和配置决定边界，其他配置需要单独验证。所列检查不是内核漏洞或完整逃逸审计，也不证明远端副作用可回滚；macOS 尚未测量。实际执行模式与默认行为见[执行器边界](../security/executor-boundaries.md)。

### 来源与复现 {#run}

完整 CSV/JSON 和原始记录保存在对应本地批次或文档数据归档。

[比较方法](index.md#interpretation) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
