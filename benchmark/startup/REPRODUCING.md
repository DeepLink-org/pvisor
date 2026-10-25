# Startup：从源码到可复核的实验

Benchmark: B-STARTUP ([注册表](../REGISTRY.md#b-startup)), role user-facing.

## Motivation

复现需要固定实验定义、输入身份和测量协议，并保留能够重算结果的原始记录。
相同输入与协议的重复执行会受到宿主负载影响；复现不要求纳秒数逐次相同。

## Experiment design

测量 Popen 前到精确 `REFERENCE_READY` 行被收到的时间，作为 Ready。
Ready 表示环境能够响应第一条命令；较短的 Ready 意味着短任务等待更少。
Exit 使用同一次启动的起点，到正常零退出且输出收集完成为止。

每个 `*_timing.md` 的 TOML 是该条件 argv、cwd、环境覆盖、预热、正式样本数和超时的来源。
准备器使用 semaspec 解析的计划，展开路径、核对输入、执行一次不计入统计的预检，
再由 `semaspec bench` 调用 Ready/Exit 适配器。适配器使用公共 `perf_counter_ns`
观察器，仅计时被测进程；Python 启动、准备、正确性校验、统计和审计在计时外。
这些含占位符的源计划须经准备器运行；`semaspec filter --execute` 不是对应的准备入口。

保留宿主热缓存，不执行全局清缓存。每次使用新的工作区和适用的 stage/磁盘，
逐次验证唯一且有序的正确输出、正常退出、源输入未变以及适用的 Run Bundle。
Firecracker/QEMU 还要求有序 `REFERENCE_EXIT 0` 且无 panic，受控停止不算正常退出。
保留全部有效慢样本；预检、预热或正式样本任一失败，整行无效。
按行独立顺序采样，不据此计算随机配对加速比例；不混合日期、输入或制品不同的批次。

### 干净源码目录：先复现 Native

从仓库根执行。需要 Linux 的 CPU affinity、`/bin/sh` 和 `mise`；工具版本由
`mise.toml`/`mise.lock` 固定，Rust 依赖由 `tools/semaspec/Cargo.lock` 固定。
Native 不需要 pVisor 制品、Python 第三方包、Docker、KVM、镜像或本机历史数据。
首次安装工具和获取 Cargo 依赖需要网络或已有工具/依赖缓存。

```sh
mise trust
mise install --locked
mise run test-startup

out=benchmark/startup/.data/native-repro-01
mise run benchmark -- startup/native_timing --output "$out"
mise exec -- python3 benchmark/run.py --audit "$out"
```

输出目录必须不存在；第二次实验换一个新名字。`--output` 保留原始证据；省略它只做
临时观察，成功审计后删除临时目录，不适合作为以后复核的来源。
`--audit` 会重新核对记录、重算统计，可刷新批次内的审计状态；不运行被测进程、不准备
输入、不更新模板旁的预览表，也不发布 CSV 或用户文档。

### 其他后端：明确提供外部输入

| 条件 | 必须提供 | 当前复现范围 |
| --- | --- | --- |
| Native | Linux、`/bin/sh`、固定工具链 | 已验证独立源码目录的运行与再次审计 |
| pVisor host/staged | `--assets`、`--binary`，以及匹配的 Host listener 或私有环境 | 已保留命令定义；本次未验证干净环境运行 |
| pVisor VM | 上述输入、可运行的 VM/固件环境、rootfs | 本次未验证；需逐次核对实际 VM 隔离 |
| Docker | `--assets`、已准备的不可变镜像、Docker 服务 | 本次未验证；镜像内须有 `/bench/affinity` |
| Firecracker/QEMU | `--assets`、KVM、对应 VMM、`--kernel-receipt`；QEMU 还需 stock initrd | 本次未验证；内核来源和完整性须通过收据校验 |

参考输入目录必须包含 `input-manifest.json` 及其声明的完整 rootfs、`assets.json`、
`vmlinux`、`bzImage`、`kernel.config`、`agent-env.ext4`、`agent-env.tar` 和
`tool-identities.json`；rootfs 须包含参考 workload、affinity launcher 和 `work/`。
[输入校验实现](../support/reference_inputs.py) 核对完整库存、字节、权限和链接。
当前仓库保留校验器，未提供完整参考环境的受维护下载/生成入口；缺少这些输入时，
上述外部后端尚不能仅靠干净 checkout 完成复现。不能用空文件或任意镜像凑齐清单。

提供输入前核对其来源收据与摘要。stock 内核收据准备工具见
[支持手册](../support/README.md)，它要求调用者提供可信内核/config/provenance/extractor，
不会下载或用 pVisor 固件代替 stock 内核。pVisor 的构建步骤见仓库根 README；
`--binary` 的 SHA-256 只标识制品，源码构建对应关系仍需构建收据。

各条件的命令见对应 `*_timing.md`。`ASSETS`、`BINARY`、`KERNEL_RECEIPT` 是调用者
明确选定的输入路径，示例会拒绝未设置的变量。命令显式传入这些选项；
`PVISOR_BENCHMARK_DOCKER_IMAGE` 仍可覆盖资产声明的镜像，实际镜像身份记入 protocol。
改变输入或协议应使用新批次，原始证据和已发布历史值保留原样。

三个镜像配对实验使用独立的脚本协议和审核器，准备/执行/审计命令及额外输入见
[预准备镜像](image_compare.md)、[冷暖客户端](image_lazy_compare.md) 和
[相同 staged 执行器](stage_lazy_compare.md)。它们尚未验证干净环境复现，不能用
Native 的通过结果替代。未接通的注册项保持未准备，入口应拒绝测量。

## Data and analysis

保留的整次启动实验目录包含：

| 文件 | 作用 |
| --- | --- |
| `run.json` | 本次选中的行、每行目录与完成状态、规格及子 manifest 摘要；漏行或失败不能作为完整批次 |
| `row-*/manifest.json` | 规格、脚本、输入、解释器和被测可执行文件的摘要，工具/系统信息、采样设置与导出摘要 |
| `row-*/protocol.json` | 展开后的 argv、cwd、环境覆盖、部分继承环境、CPU affinity、输入与内核身份、测量边界 |
| `row-*/harness/` | 冻结的规格和支持源码 |
| `row-*/trials/` | 每次预检/预热/正式执行的原始输出、时钟、成功校验及适用的 Bundle |
| `row-*/timing.samples.json` | 全部正式 Ready/Exit 纳秒样本和重算统计 |
| `row-*/in.bench.json`、`in.render.json` | semaspec 的单次脚本行导出与完整精度结果 |

默认计划应产生 64 次记录：1 次预检、3 次预热、60 次正式样本。审计先核对整次
选择的完整性和各行 manifest，再重算每行统计；漏项、重复、修改或失败都应拒绝。
独立准备器直接生成的单行批次没有 `run.json`，`--audit` 同样可核对其 manifest。

`--audit` 先核对当前 startup 审核代码与冻结 harness 的摘要，版本不匹配时要求恢复对应 checkout；
部分输入核对仍依赖原始路径下的资产和
内核收据；证据目录目前不是可任意搬迁的自包含归档。脚本和输入摘要不匹配时应
恢复对应版本或重新准备新批次，不能改写原始摘要来让审核通过。
`protocol.json` 记录的继承环境只覆盖 PATH/locale/TZ，不是完整环境封闭性声明。

模板旁的 `*.render.json` 是本地预览：单行重测可保留其他批次的旧行，不能把它当作
同一批对照的证据。需要复核时以 `--output` 目录为准。审计成功后，公开数据仍须走
已有的独立发布审核；不从测试样本自动更新用户成绩，PASS 与人工批准独立。
