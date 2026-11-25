# VM 启动时间专报

Benchmark: B-VM-STARTUP-ENG ([注册表](../REGISTRY.md#b-vm-startup-eng))，engineering A/B。
PR Conversation 的 **VM startup performance** 和 Actions 的 **VM startup regression gate**
展示独立的表格，不向 [15 项 guest 工作负载](guest.md) 添加行。每格是基线 → 候选，
单位为毫秒（Criterion 中位估计），附变化率。

| 启动方式 | pVisor VM + Linux 固件 | pVisor VM + Rust native | Firecracker |
| --- | --- | --- | --- |
| 直接启动 | 新 VM | 新 VM | 新 VM，参照 |
| daemon 预热启动 | 命中一个已停泊的 VM | N/A：不支持延迟挂载根文件系统 | N/A：不由 pvisor-daemon 管理 |

**直接启动（创建 → guest 命令开始执行）**：宿主从发起 VMM spawn 前开始计时，到读到完整的
`VM_GUEST_COMMAND_STARTED` 行为止。这是首个用户命令进入 `main` 后的第一笔写入，意味着 guest
内核已经启动且用户态代码正在执行。文件系统校验、完成标记和正常退出是验收条件，不计入延迟。
pVisor 用 virtio-fs 和 task stdout；Firecracker 从 ext4/virtio-blk 启动同一探针作为 PID 1，
经串口输出。VMM 启动日志和 vCPU resume 都不是停表点。

**预热启动（Create → guest 入口命令开始执行）**：向已停泊槽位发出 HTTP Create，指定 `/bin/sh`
作为该请求的入口命令。shell 的第一条内置 `printf` 向本次独占的 guest 根文件写入试次标记，
之后才 `exec /bin/sleep`。宿主在 Create 进行期间独立观察该槽位可写层中的完整标记（轮询间隔
100 µs）。不使用 202/Running 的返回时间，也不等待后续验证命令完成才停表。
要求实际分配的 VM ID 与被观察槽位相同，并验证服务可用、数据正确和删除后的回收。
预热不包含提前完成的内核引导或补池。两行都是宿主观察到用户命令开始的延迟，包含各自
标记的传递开销；不声称是精确的首条 CPU 指令时间。

**门禁与环境**：同机、4 vCPU / 256 MiB，宿主绑定同四个 CPU；每格 10 个 Flat 统计样本。
两个 pVisor 直接启动项和 Linux 预热项均检查 15% 阈值，只有 Firecracker
作参照。受支持项测量失败、超时或缺基线均不放行；旧 VMM/API Running、命令完成基线均不能复用。
CI 在两个版本间复用同一预先准备的固定 Ubuntu 镜像，不把下载、daemon 启动或补池计入延迟。

## 本地复现（Linux x86_64 / KVM）

按 [guest 门禁准备说明](guest.md#本地复现) 准备两版 VM API 运行器、Linux 固件及固定 Firecracker 和 ELF 内核。
另需两版 `pvisor-daemon`（带 warm pool），动态构建须将对应的 Linux 固件放在其旁并命名为 `libkrunfw.so.5`。
从候选 checkout 执行，使用有 cpu、memory、pids 委派的 cgroup v2；`4-7` 替换为四个可用 CPU：

```bash
set -euo pipefail
mkdir -p benchmark/vm/.data
batch="$(mktemp -d "$PWD/benchmark/vm/.data/startup.XXXXXX")"
export PVISOR_BENCH_GUEST="$batch/probe"
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/startup_probe.c -o "$PVISOR_BENCH_GUEST"
export PVISOR_BENCH_DAEMON_IMAGE=ubuntu PVISOR_BENCH_DAEMON_IMAGES="$batch/images"
"$CANDIDATE_DAEMON" prepare-image --name ubuntu --images-dir "$batch/images" \
  --image mirror.gcr.io/library/ubuntu@sha256:9a04f13b3f80ef056361e30868593b1cfd2279fe19e4081cb93a90bf56819d6a
export PVISOR_BENCH_FIRECRACKER=/prepared/firecracker PVISOR_BENCH_FC_KERNEL=/prepared/vmlinux
mise exec -- cargo build --release --locked --manifest-path benchmark/criterion/Cargo.toml --bin pvisor-benchmarks
measure() {
  systemd-run --user --scope --quiet --unit="pvisor-warm-$1-$$" \
    -p Delegate=yes -p CPUQuota=400% -p MemoryMax=2G -p MemorySwapMax=0 \
    env PVISOR_BENCH_LINUX_FW="$3" PVISOR_BENCH_DAEMON="$4" taskset -c 4-7 \
    "$PWD/target/release/pvisor-benchmarks" vm/startup --binary "$2" --output "$batch/$1" \
    --criterion-output "$batch/criterion" -- --save-baseline "$1"
}
measure main "$BASE_VM" "$BASE_FW" "$BASE_DAEMON"
measure candidate "$CANDIDATE_VM" "$CANDIDATE_FW" "$CANDIDATE_DAEMON"
mise run benchmark-compare -- "$batch/criterion" --suite vm-startup --output "$batch/comparison"
```

比较超阈值时仍保留 `comparison.json`；用 `python3 -I scripts/ci/vm_guest_comment.py --startup-summary "$batch/comparison/comparison.json"`
预览 PR 表格，不发布评论。逐次 `timing.json` 的 `ready_ns` 是直接启动时钟；
`warm-hits.jsonl` 的 `guest_command_start_ns` 是预热时钟。`sample.create_ns`（HTTP）
和 `sample.first_command_ns`（后续验证命令完成）仅作诊断。
