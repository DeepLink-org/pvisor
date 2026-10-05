# 基准方法与比较边界

## 主要结论 {#conclusions}

本章的数字用于判断具体任务的等待、资源和可靠性。**只有同配置、同计时口径的数据才能直接比较**：最小 VM 与完整 Ubuntu、工具时间与启动到退出、RSS 与 cgroup 内存，都分别呈现。未测工具不使用厂商宣传值补进排名。

## Motivation {#motivation}

启动更快不一定意味着任务更快；可写挂载与暂存视图也提供不同工作流。公开对比需要让用户看清测量的是哪种环境、等待发生在哪里，以及结果是否适用于自己的配置。

## 实验设计 {#interpretation}

### 环境与制品

| 数据范围 | 配置与身份 |
|---|---|
| 当前本地文件系统 | Linux/KVM，两核预算，VM 2 vCPU / 4 GiB；冻结集成源码，release/performance 分别测量 |
| 同工具参考环境 | Linux/x86_64，Docker Engine 29.7.2 rootless、Firecracker 1.13.1、QEMU 10.2.2；2 vCPU，shell 128 MiB、工具任务 16 GiB |
| 完整发行版部署 | pVisor host rootfs；参考 VM 使用 Ubuntu 26.04.1、generic 内核、initrd、systemd；启动 2 GiB、工具任务 16 GiB |
| macOS | Apple M4 / HVF；启动和冷页数据使用各自报告固定的配置 |
| Cluster | 1/2/4 Worker，增配 CPU 预算；探针使用冻结 debug 制品，不与 release 启动时间拼接 |

所有版本号是实测身份，不表示第三方当前最新版。源码提交号不能替代 dirty 工作树的可执行身份；二进制 SHA256、输入摘要、参数和原始报告为准。当前文件系统重测覆盖本地与 lazy；其他主题仍为其固定制品的可用证据，不能宣称全套数据均已重测当前集成版本。

### 计时与正确性

Ready 从宿主启动命令前到第一条有效 guest 输出；worker 为内部操作和校验；任务时间为启动到结果；Exit 另计进程结束。下载、镜像构建、工具安装与每轮输入准备独立记录。默认 3 次预热、30 次测量、随机顺序；N=10、N=3 或顺序测量会在对应页说明。

通过零退出、文件数量/内容、SHA256、编译与测试结果、Run Bundle 和实际执行器检查结果。失败不进入成功耗时分布，样本数、失败原因和容量 guard 保留。共享宿主没有完全隔离后台负载、锁定频率或清空所有缓存；小幅差距不构成稳定排名。

### 比较条件

Docker 对照使用已运行的私有 rootless daemon 和 writable bind mount。pVisor staged 保留改动，独立 stage 不自动禁止视图外访问；VM 通过 virtio-fs 使用共享文件服务。Firecracker/QEMU 使用私有 ext4。内核、网络、工具版本与安全部署参数见逐批报告；开发基线不是生产安全配置排名。

### 资源口径 {#reference-resources}

RSS 是每 20 ms 的进程范围求和，可能重复计共享页、漏掉短峰；Docker 需追踪容器 ID 对应 shim，包含专属 daemon 时明确注明。配置的 guest RAM 不等于实际驻留量。Cluster 使用互不重叠 cgroup 的 `memory.current`，其中 file 可能含 guest RAM，不能直接扣除。macOS 冷页 RAM 代理也不等于净物理内存节约。

## 实验数据和分析 {#results}

### 当前文件系统证据 {#filesystem-service}

[本地 release](../../assets/benchmarks/filesystem-service-20261005/local-release.tsv) · [本地 performance](../../assets/benchmarks/filesystem-service-20261005/local-performance.tsv) · [lazy 冷/热](../../assets/benchmarks/filesystem-service-20261005/lazy-performance.tsv) · [制品清单](../../assets/benchmarks/filesystem-service-20261005/manifest.tsv)

本地每种编译配置 150 个正式任务，lazy 120 个，共 420 个任务、2,820 项操作测量。缓存 fixture 验证按需块读取与热客户端缓存；它不代表生产 Rust 缓存、网络或 S3 性能。旧/新版本 A/B 仅在[技术分析](../design/filesystem-performance-analysis.md#filesystem-service)解释。

### 同工具环境参考 {#reference-env}

各组共用工具制品和输入，参考 VM 采用裁剪内核、静态 init，不启动完整发行版。准备环境的首次输出、文件操作和 CLI 工具闭环分别用于[启动](startup.md#reference-startup)、[文件系统](filesystem.md#reference-fs)和[完整任务](agent-tasks.md#reference-env)对比。

[汇总与分布](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [样本](../../assets/benchmarks/reference-env-20261004/samples.csv) · [兼容性](../../assets/benchmarks/reference-env-20261004/compatibility.tsv)

### 完整 Ubuntu 部署 {#full-ubuntu}

完整 Ubuntu cloud VM 使用官方发行版内核、initrd 和正常服务，pVisor 复用宿主工具目录。这测量部署方式导致的用户等待，不能隔离出纯 VMM 性能。首次 cloud-init 与已经配置的模板分开，下载和模板准备不计时。

[汇总](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [样本](../../assets/benchmarks/full-ubuntu-20261004/samples.csv)

### QEMU 完整发行版配置 {#full-ubuntu-qemu}

q35 与 microvm 使用同一完整 Ubuntu 模板，各格 N=10；与 Firecracker/pVisor 的分布独立，不合并样本。

[汇总](../../assets/benchmarks/full-ubuntu-qemu-20261004/summary.tsv) · [配置清单](../../assets/benchmarks/full-ubuntu-qemu-20261004/manifest.tsv)

### 其他主题与复现 {#product-v1}

网络、apply/drop、隔离、回放、监督与密度的参数和样本见各主题以及[原始清单](../../assets/benchmarks/product-v1-20261004/manifest.tsv)。详细环境、命令、失败诊断和资源审计保存在[方法技术记录](../design/benchmark-methodology-evidence.md)。重跑时输出新目录，固定输入和制品，保留失败，不混合不同环境的分布。

## 基准证据 TSV {#evidence-format}

公开的 `docs/src/assets/benchmarks/` 报告使用三列 TSV：`path`、`type`、`value`。每个字段占一行，统计值变化不会带来 JSON 的缩进、逗号和整块对象差异。路径采用 JSON Pointer：`~0` 表示 `~`，`~1` 表示 `/`；数组下标从 0 开始并保持原顺序。

```tsv
path	type	value
	object	-
/samples	array	-
/samples/0	object	-
/samples/0/elapsed_ms	float	12.125
/samples/0/passed	boolean	true
```

`type` 区分 `object`、`array`、`string`、`integer`、`float`、`boolean` 和 `null`。容器使用 `-` 标记，空字符串使用 `""`；空容器、null 与缺失字段不会混为一谈。字符串的制表符、换行、反斜线和控制字符使用转义，末尾空格使用 `\u0020`，避免多行记录和 Git 尾随空白告警。数字保持原有数值与整数/浮点类型，不截断精度。

[转换索引](../../assets/benchmarks/conversion.tsv)记录原 JSON 文件名/字节 SHA256、TSV 文件名/字节 SHA256、规范化数据摘要、大小和行数。历史报告中的 JSON 文件名和原摘要保留其原始含义；通过索引找到对应 TSV，不将旧摘要冒充 TSV 摘要。测量样本、失败、协议、来源信息与现有 CSV/图表保持不变。

```bash
python3 benchmark/pvisor/evidence_tsv.py check docs/src/assets/benchmarks
python3 benchmark/pvisor/evidence_tsv.py convert \
  docs/src/assets/benchmarks/cluster-scalability-20261005/vm.tsv \
  /tmp/pvisor-vm-evidence.json
```

转换工具可以双向读写；重建的 JSON 保留数据，但不承诺还原旧文件的空白和键顺序。此次迁移的原始字节另保存在 `target/benchmark-json-originals-20261005/`，每个文件已验证原 SHA256；该目录是本机恢复备份，不进入发布附件。

当前绘图与汇总脚本接受 TSV，也能读取新实验的运行时 JSON。参考环境与 Ubuntu 发布脚本自动把公开附件转为 TSV，运行时协议和 `target/` 下的实验输出继续使用各自原格式。格式校验与绘图不启动 VM，也不重跑基准。

