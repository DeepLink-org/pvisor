# VM guest 回归门禁

Benchmark: B-VM-GUEST-ENG ([注册表](../REGISTRY.md#b-vm-guest-eng))，engineering A/B。
实现：[Criterion 入口](../criterion/src/vm_guest.rs)、[guest 探针](probe.c)。

## 门禁判据

PR 的目标提交和待合并版本在同一台 Linux x86_64/KVM 主机上顺序测量。每个版本都跑
pvisor-vm + Linux 固件、pvisor-vm + 嵌入式 Rust native kernel、固定的 Firecracker；
每个后端 17 项负载，共 34 个独立的产品门禁和 17 个必测参照。
每项取 Criterion 的 `median.point_estimate`（ns/整项操作，越小越好）。
两个 pVisor 后端各自和自己的基线比：**顺序比较中任意一项超过基线的 115% 时，
在同一 runner 上用 16 对平衡 AB/BA 试次复核；复核的配对变化下四分位
（16 对中第 4 小值，保守的 95% 下界）也超过 115% 才失败**，不按平均分抵消。
复核只针对未通过项，阈值不变，也不重跑已通过项。
Firecracker 的两个读数用来识别宿主波动，不要求 pVisor 快于它。三个后端都必须给出有效结果；
缺 KVM、缺制品、超时、校验失败、缺少估计值都不放行。

将 GitHub 状态检查 **`VM guest regression gate`** 设为目标分支的 required check，
才能用这个失败状态阻止合并。PR 的 **Conversation** 中，GitHub Actions 会更新同一条
**VM guest performance** 评论。表格按负载分 17 行，三列并排显示各后端的基线 → 候选
中位耗时（ms/整项）和变化率；顺序比较超过阈值的 pVisor 项会标红，平衡复核确认未超阈值的项标 ✅。
若本次测量未产出完整报告，评论会给出运行链接而不是显示通过。评论不改变门禁结果。
[benchmark CI](../../.github/workflows/pvisor-benchmark.yml) 在同仓库 PR 的两项 VM 门禁结束后直接调用
[评论工作流](../../.github/workflows/vm-guest-comment.yml)，不需要先合并 PR。运行时表与启动表由
`github-actions[bot]` 分别发布，后续提交更新原评论。只有发布作业需要写 PR 评论的权限；
fork 和 Dependabot 使用默认分支的 `workflow_run` 发布路径，该工作流合入默认分支后生效。
作业摘要仍显示每项判定；`vm-guest-<run_id>` 制品保留构建
SHA、制品哈希、逐次命令与日志、Criterion 样本及 `comparison/report.md` 和 `comparison.json`。
若 CI 在测量阶段失败，先看失败 cell 的 `stderr.log`、`stdout.log` 和 `exit.json`。

**启用必需检查前先做 A/A 校准：** 将基线、候选指向同一份 VM 与固件制品，完整运行一次。
若门禁报红，这不是代码变慢的证据。核对两组 `manifest.json` 中的哈希与 CPU 亲和性，
对照该项的 Criterion `sample.json`、Firecracker 读数及宿主负载记录。保留失败批次；
在固定资源的空闲 runner 上建立稳定基线，不用参照项抵消失败或事后提高阈值。

## 测量内容

每次操作在新 VM 中执行，1 vCPU（跨 vCPU 唤醒项为 2 vCPU）、256 MiB、相同的静态工作负载，用 guest 的
`CLOCK_MONOTONIC` 计时。启动、关机、准备目录/镜像、挂载和文件内容回读校验不计入。
每个 cell 用 Flat 采样，10 个统计样本，100 ms 预热和 1 秒目标测量预算；
不丢慢样本。它不是跨机器的排行榜，也不衡量 VM 启动、内存占用或尾延迟。

| 指标 | 一次计时操作 |
| --- | --- |
| `cpu-integer` | 800 万次有数据依赖的 xorshift32；检查最终值 |
| `syscall-getpid` | 131,072 次直接 `SYS_getpid`，每次检查 PID，测进入和返回 guest 内核的成本 |
| `ipc-pipe-roundtrip` | 单 vCPU 上两个进程经两条管道完成 4,096 次单字节请求/应答，每次验证应答；不含 fork、首次握手及 waitpid |
| `memory-copy` | 在已触页的两个 16 MiB 缓冲区间复制 16 次；验证目标字节 |
| `memory-map-touch` | 私有匿名映射 32 MiB，并对每个 4 KiB 页首次写一个字节；不含读回校验和解除映射 |
| `disk-write-fsync` | 每次 4 KiB，顺序写 4 MiB 并 `fsync`；回读逐字节校验 |
| `disk-warm-read` | 预热后按顺序 `pread` 完整 4 MiB，每次 4 KiB |
| `disk-shuffled-read` | 相同文件和预热条件下，乱序 `pread` 所有 4 KiB 块 |
| `disk-shuffled-overwrite-fsync` | 乱序 `pwrite` 已有 4 MiB 文件中的所有块，计入末尾一次 `fsync` |
| `tmpfs-write` | 每次 4 KiB，顺序写 4 MiB，不含 `fsync` |
| `tmpfs-read` | 读取刚写入的 4 MiB 文件，每次 4 KiB，校验内容 |
| `tmpfs-shuffled-read` | 在预热的 tmpfs 文件中乱序 `pread` 所有 4 KiB 块 |
| `tmpfs-shuffled-overwrite` | 乱序覆写已有 tmpfs 文件中的所有 4 KiB 块，不含 `fsync` |
| `tmpfs-metadata` | 1,024 轮创建、关闭、重命名、`stat` 和删除；检查最终路径不存在 |
| `tmpfs-dirscan` | 对含 512 个文件的目录执行 16 次完整的打开、遍历、复制名称、关闭；逐轮在计时外校验名称集合 |
| `ipc-wake-cross-vcpu` | 2 vCPU；两个进程分别绑定 guest CPU 0/1，经两条管道完成 4,096 次单字节请求/应答；覆盖跨 vCPU 唤醒路径 |
| `fs-metadata-tree` | 在 `/work` 上创建 32 个目录 × 32 个空文件，逐项 `stat`，再删除并检查不存在；覆盖 virtio-fs 根的多文件元数据路径 |

定位读写负载都先填满文件并完整读回一次；磁盘文件在预热前另做一次 `fsync`。
乱序负载用相同的固定种子打乱块顺序，每个偏移恰好访问一次，最后检查长度与全部字节。
这些读数是**缓存预热后的文件访问**，不是冷读或物理设备随机 IOPS；磁盘覆写也不是每块一次持久化。
管道往返测的是单 vCPU 上的 IPC 和调度，不是跨核扩展能力或网络延迟。

tmpfs 在 guest 内单独挂载，用 `statfs` 验证 `TMPFS_MAGIC`，不假设 `/tmp` 是 tmpfs。
磁盘项要求证据目录在宿主磁盘上的 ext4、XFS 或 btrfs；pVisor 使用可写 virtio-fs
根目录（Rust native 没有 virtio-blk），Firecracker 使用独占 ext4 镜像、virtio-blk
和 Writeback 模式以传递 flush。这里测的是各自文件路径的读写，不是裸盘带宽或断电持久性。

## 本地复现

需要可读写的 `/dev/kvm`、`e2fsprogs`、支持静态链接的 C 编译器和两套被测制品。
在**各自版本**的 checkout 里分别准备 Linux 固件、Rust kernel 和宿主 GNU 目标的 VM API 运行器：

```sh
mise run fw -- build --target x86_64-unknown-linux-musl  # 记下输出的 Linux 固件文件路径
native="$(mise run kernel -- build --prepare-toolchain)"
PVISOR_BUILD_NATIVE_KERNEL_BUNDLE="$native" mise exec -- \
  cargo build --release --locked -p pvisor-vm --example vm_api_case
```

从候选 checkout 运行下列 Bash 命令。`BASE_VM`、`BASE_FW`、`CANDIDATE_VM`、
`CANDIDATE_FW` 分别指向两版的 `target/release/examples/vm_api_case` 与对应 Linux 固件。
Firecracker 和 ELF Linux 内核需预先准备；CI 固定 v1.13.1 x86_64 和上游 CI
内核 6.1.141，下载地址及 SHA-256 见 [workflow](../../.github/workflows/pvisor-benchmark.yml)。

```bash
set -euo pipefail
mkdir -p benchmark/vm/.data
batch="$(mktemp -d "$PWD/benchmark/vm/.data/gate.XXXXXX")"
export PVISOR_BENCH_GUEST="$batch/probe"
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/probe.c -o "$PVISOR_BENCH_GUEST"
export PVISOR_BENCH_FIRECRACKER=/prepared/firecracker
export PVISOR_BENCH_FC_KERNEL=/prepared/vmlinux-6.1.141

measure() {
  PVISOR_BENCH_LINUX_FW="$3" mise run benchmark -- vm/guest --binary "$2" \
    --output "$batch/$1" --criterion-output "$batch/criterion" -- --save-baseline "$1"
}
measure main "$BASE_VM" "$BASE_FW"
measure candidate "$CANDIDATE_VM" "$CANDIDATE_FW"
mise run benchmark-compare -- "$batch/criterion" --suite vm-guest --output "$batch/comparison"
```

共享同一候选 harness、探针、Firecracker 和对照内核；每对版本使用新批次，避免读取上次残留结果。
不要沿用旧的六项基线：用新探针对目标提交重新测量，否则缺失的基线项会使比较失败。
不要在测量机上同时构建或执行另一个 benchmark。顺序 cohort 中的百分比仅用于门禁，
不能当作随机配对的因果结论。失败后保留证据，不反复执行择优。

CI 在原 runner 上对未通过的 pVisor 项和固定的 guest 本地对照
`linux-tmpfs-shuffled-overwrite` 执行复核。`vm/guest --diagnose "$batch"` 校验探针、两版
VMM 和固件哈希，在新 VM 中交替执行 16 对试次；先后顺序在每四对内平衡。
`PVISOR_BENCH_REVISION_BUILDS` 指向 CI 下载的两个 `benchmark-build-*` 目录，
其他输入路径与测量相同。未通过项的配对变化下四分位不超过阈值时门禁通过，否则失败；
`comparison.sequential.json` 保留原始顺序结果，`paired-diagnostic/` 保存全部原始试次。

**同一候选的 handoff 对照。** 在上述批次首次执行 `--diagnose` 前另设：

```sh
export PVISOR_BENCH_HANDOFF_PROBE="$batch/handoff-probe"
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/handoff_probe.c -o "$PVISOR_BENCH_HANDOFF_PROBE"
```

诊断在同一候选上随机交替显式 `0`、`1`、`auto`，每项 15 组，分别测原始四个
共享磁盘负载和 256 次热 `pread`（连续／每次先等待 1 ms）。guest 读计时不含等待；
VMM 用户态与内核态 CPU 时间包含 VM 启停及校验，不能解释为单个请求的 CPU 成本。
每项另用非计时 VM 核验协商并记录全程路径计数。`handoff/summary.md` 在 CI
作业摘要展示，原始样本随制品保留；`off/fallback` 不是成功测到开启后的性能。
